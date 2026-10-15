//! Hand-written FlatBuffers reader for Cytrus v6 manifests (`.manifest` / `.config`).
//!
//! Schema: `docs/cytrus-manifest.fbs`. No file identifier, no size prefix: the root table offset is
//! the first `u32`. Every read is bounds-checked, so a truncated or hostile buffer yields an error
//! instead of a panic. The reader walks vtables directly over the input slice and only allocates
//! the owned output (strings, chunk vectors); a 51 MB dofus3 manifest parses well under 1 s in release.

use anyhow::{Context, Result, bail};

/// Raw SHA-1 digest.
pub type Hash = [u8; 20];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chunk {
    pub hash: Hash,
    pub size: u64,
    /// Offset inside the file (`File::chunks`) or inside the bundle blob (`Bundle::chunks`).
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Relative path with `/` separators.
    pub name: String,
    pub size: u64,
    /// SHA-1 of the whole file. All zeroes when absent (only tolerated for symlinks).
    pub hash: Hash,
    /// Empty => the file is a single chunk `{hash, 0, size}` (see [`File::effective_chunks`]).
    pub chunks: Vec<Chunk>,
    pub executable: bool,
    pub symlink: Option<String>,
}

impl File {
    /// Chunk list with the "empty list = one chunk covering the whole file" rule applied.
    pub fn effective_chunks(&self) -> std::borrow::Cow<'_, [Chunk]> {
        if self.chunks.is_empty() {
            std::borrow::Cow::Owned(vec![Chunk { hash: self.hash, size: self.size, offset: 0 }])
        } else {
            std::borrow::Cow::Borrowed(&self.chunks)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// SHA-1 of the concatenated raw chunk hashes (not the blob content). Used for the URL.
    pub hash: Hash,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub name: String,
    pub files: Vec<File>,
    pub bundles: Vec<Bundle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub fragments: Vec<Fragment>,
}

impl Manifest {
    pub fn fragment(&self, name: &str) -> Option<&Fragment> {
        self.fragments.iter().find(|f| f.name == name)
    }
}

/// Parse a Cytrus v6 FlatBuffers manifest.
pub fn parse(bytes: &[u8]) -> Result<Manifest> {
    let r = Reader { b: bytes };
    let root = r.u32(0).context("manifest: missing root offset")? as usize;
    let root = Table::new(&r, root)?;
    let mut fragments = Vec::new();
    if let Some(v) = root.vector(0)? {
        fragments.reserve(v.len);
        for i in 0..v.len {
            let t = v.table(i)?;
            fragments.push(parse_fragment(&t).with_context(|| format!("manifest: fragment #{i}"))?);
        }
    }
    Ok(Manifest { fragments })
}

fn parse_fragment(t: &Table) -> Result<Fragment> {
    let name = t.string(0)?.unwrap_or_default();
    let mut files = Vec::new();
    if let Some(v) = t.vector(1)? {
        files.reserve(v.len);
        for i in 0..v.len {
            let f = v.table(i)?;
            files.push(parse_file(&f).with_context(|| format!("fragment {name}: file #{i}"))?);
        }
    }
    let mut bundles = Vec::new();
    if let Some(v) = t.vector(2)? {
        bundles.reserve(v.len);
        for i in 0..v.len {
            let b = v.table(i)?;
            let hash = b.hash(0)?.with_context(|| format!("fragment {name}: bundle #{i} has no hash"))?;
            bundles.push(Bundle { hash, chunks: parse_chunks(&b, 1)? });
        }
    }
    Ok(Fragment { name, files, bundles })
}

fn parse_file(t: &Table) -> Result<File> {
    let name = t.string(0)?.context("file without name")?;
    let size = t.u64_field(1)?;
    let symlink = t.string(5)?.filter(|s| !s.is_empty());
    let hash = match t.hash(2)? {
        Some(h) => h,
        None if symlink.is_some() => [0; 20],
        None => bail!("file {name}: missing hash"),
    };
    let chunks = parse_chunks(t, 3)?;
    let executable = t.u8_field(4)? != 0;
    Ok(File { name, size, hash, chunks, executable, symlink })
}

fn parse_chunks(t: &Table, slot: usize) -> Result<Vec<Chunk>> {
    let Some(v) = t.vector(slot)? else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(v.len);
    for i in 0..v.len {
        let c = v.table(i)?;
        let hash = c.hash(0)?.with_context(|| format!("chunk #{i} has no hash"))?;
        out.push(Chunk { hash, size: c.u64_field(1)?, offset: c.u64_field(2)? });
    }
    Ok(out)
}

// --- low-level reader -------------------------------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    #[inline]
    fn slice(&self, pos: usize, n: usize) -> Result<&'a [u8]> {
        match pos.checked_add(n) {
            Some(end) if end <= self.b.len() => Ok(&self.b[pos..end]),
            _ => bail!("manifest truncated: read {n} bytes at {pos} beyond {}", self.b.len()),
        }
    }
    #[inline]
    fn u32(&self, pos: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.slice(pos, 4)?.try_into().unwrap()))
    }
    #[inline]
    fn i32(&self, pos: usize) -> Result<i32> {
        Ok(i32::from_le_bytes(self.slice(pos, 4)?.try_into().unwrap()))
    }
    #[inline]
    fn u16(&self, pos: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.slice(pos, 2)?.try_into().unwrap()))
    }
    #[inline]
    fn i64(&self, pos: usize) -> Result<i64> {
        Ok(i64::from_le_bytes(self.slice(pos, 8)?.try_into().unwrap()))
    }
    /// Follow a `uoffset_t` stored at `pos`.
    #[inline]
    fn indirect(&self, pos: usize) -> Result<usize> {
        let off = self.u32(pos)? as usize;
        pos.checked_add(off).filter(|p| *p < self.b.len()).context("manifest: offset out of bounds")
    }
}

struct Table<'r, 'a> {
    r: &'r Reader<'a>,
    pos: usize,
    vtable: usize,
    vt_len: usize,
}

struct Vector<'r, 'a> {
    r: &'r Reader<'a>,
    /// Position of the first element.
    start: usize,
    len: usize,
}

impl<'r, 'a> Table<'r, 'a> {
    fn new(r: &'r Reader<'a>, pos: usize) -> Result<Self> {
        let soff = r.i32(pos)? as i64;
        let vtable = pos as i64 - soff;
        if vtable < 0 || vtable as usize >= r.b.len() {
            bail!("manifest: vtable out of bounds at table {pos}");
        }
        let vtable = vtable as usize;
        let vt_len = r.u16(vtable)? as usize;
        if vt_len < 4 {
            bail!("manifest: invalid vtable length {vt_len}");
        }
        r.slice(vtable, vt_len)?;
        Ok(Table { r, pos, vtable, vt_len })
    }

    /// Absolute position of field `slot`, or None if absent.
    #[inline]
    fn field(&self, slot: usize) -> Result<Option<usize>> {
        let o = 4 + 2 * slot;
        if o + 2 > self.vt_len {
            return Ok(None);
        }
        let off = self.r.u16(self.vtable + o)? as usize;
        Ok(if off == 0 { None } else { Some(self.pos + off) })
    }

    fn u64_field(&self, slot: usize) -> Result<u64> {
        match self.field(slot)? {
            None => Ok(0),
            Some(p) => {
                let v = self.r.i64(p)?;
                u64::try_from(v).map_err(|_| anyhow::anyhow!("manifest: negative long {v}"))
            }
        }
    }

    fn u8_field(&self, slot: usize) -> Result<u8> {
        match self.field(slot)? {
            None => Ok(0),
            Some(p) => Ok(self.r.slice(p, 1)?[0]),
        }
    }

    fn vector(&self, slot: usize) -> Result<Option<Vector<'r, 'a>>> {
        let Some(p) = self.field(slot)? else { return Ok(None) };
        let vp = self.r.indirect(p)?;
        let len = self.r.u32(vp)? as usize;
        Ok(Some(Vector { r: self.r, start: vp + 4, len }))
    }

    fn bytes(&self, slot: usize) -> Result<Option<&'a [u8]>> {
        match self.vector(slot)? {
            None => Ok(None),
            Some(v) => Ok(Some(self.r.slice(v.start, v.len)?)),
        }
    }

    fn string(&self, slot: usize) -> Result<Option<String>> {
        match self.bytes(slot)? {
            None => Ok(None),
            Some(b) => Ok(Some(std::str::from_utf8(b).context("manifest: invalid UTF-8 string")?.to_owned())),
        }
    }

    fn hash(&self, slot: usize) -> Result<Option<Hash>> {
        match self.bytes(slot)? {
            None => Ok(None),
            Some([]) => Ok(None),
            Some(b) => Ok(Some(b.try_into().map_err(|_| anyhow::anyhow!("manifest: hash of {} bytes", b.len()))?)),
        }
    }
}

impl<'r, 'a> Vector<'r, 'a> {
    fn table(&self, i: usize) -> Result<Table<'r, 'a>> {
        let p = self.start.checked_add(i.checked_mul(4).context("overflow")?).context("overflow")?;
        Table::new(self.r, self.r.indirect(p)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Vec<u8> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cytrus").join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn h(s: &str) -> Hash {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    #[test]
    fn parses_dofus3_config() {
        let m = parse(&sample("dofus-dofus3-windows-6.0_3.7.10.10.config")).unwrap();
        assert_eq!(m.fragments.len(), 1);
        let f = &m.fragments[0];
        assert_eq!(f.name, "configuration");
        assert_eq!(f.files.len(), 2);
        assert_eq!(f.files[0].name, "zaap-windows.yml");
        assert_eq!(f.files[0].size, 2351);
        assert_eq!(f.files[0].hash, h("39e2d941f97682525f3eee6994018db5f15d3c12"));
        assert!(f.files[0].chunks.is_empty());
        assert!(!f.files[0].executable);
        assert_eq!(f.files[0].symlink, None);
        assert_eq!(f.files[1].name, "zaap.yml");
        assert_eq!(f.files[1].size, 5528);
        assert_eq!(f.bundles.len(), 1);
        let b = &f.bundles[0];
        assert_eq!(b.hash, h("dcc5681b6ae05bad6a4ceb6de377b4c6aa8c8f9d"));
        assert_eq!(b.chunks.len(), 2);
        assert_eq!(b.chunks[1], Chunk { hash: h("f9e45efca111b3df9a7eb58b8371308ac66731a6"), size: 5528, offset: 2351 });
        // Bundle hash = SHA-1 of the concatenated chunk hashes.
        use sha1::Digest;
        let mut s = sha1::Sha1::new();
        for c in &b.chunks {
            s.update(c.hash);
        }
        let d: Hash = s.finalize().into();
        assert_eq!(d, b.hash);
        let eff = f.files[1].effective_chunks();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].size, 5528);
    }

    #[test]
    fn parses_main_and_beta_configs() {
        let m = parse(&sample("dofus-main-windows-6.0_2.73.3.14.config")).unwrap();
        assert_eq!(m.fragments[0].files[0].name, "zaap.yml");
        assert_eq!(m.fragments[0].files[0].hash, h("d9c10a1b38e37231e0e950a143b4a92f226dc452"));
        let m = parse(&sample("dofus-beta-windows-6.0_3.7.3.3.config")).unwrap();
        assert_eq!(m.fragments[0].name, "configuration");
    }

    #[test]
    fn truncated_input_errors_instead_of_panicking() {
        let full = sample("dofus-dofus3-windows-6.0_3.7.10.10.config");
        for n in 0..full.len() {
            let _ = parse(&full[..n]); // must not panic
        }
        assert!(parse(&full[..full.len() / 2]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn head_of_real_manifest_fails_cleanly() {
        // 256 KiB head of the 51 MB dofus3 manifest: offsets point past the end.
        let head = sample("manifest-head/dofus3-6.0_3.7.10.10.manifest.head256k");
        assert!(parse(&head).is_err());
    }
}
