//! Local install state: Zaap-format `manifest.json` (shared with the official launcher) and our
//! sidecar `.dofus-launcher-state.json` (installed version/fragments + per-file (size, mtime) stamps
//! taken right after a verified write, used for the fast path). Both are written atomically.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::manifest;

pub const ZAAP_MANIFEST: &str = "manifest.json";
pub const SIDECAR: &str = ".dofus-launcher-state.json";
pub const LOCK_FILE: &str = ".dofus-launcher.lock";
pub const RELEASE_INFOS: &str = ".release.infos.json";
pub const MIGRATION: &str = "migration.launcher.json";
pub const TMP_SUFFIX: &str = ".dl-tmp";

/// Never deleted by the cleanup pass (relative to the install root, compared case-insensitively).
pub fn is_protected(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    matches!(lower.as_str(), ZAAP_MANIFEST | SIDECAR | LOCK_FILE | RELEASE_INFOS | MIGRATION) || lower.ends_with(".log")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZaapChunk {
    pub hash: String,
    pub offset: u64,
    pub size: u64,
}

/// One `manifest.json` entry (Zaap format; the transient `done` field is not written).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZaapFile {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub chunks: Vec<ZaapChunk>,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub symlink: Option<String>,
}

impl ZaapFile {
    pub fn from_manifest(f: &manifest::File) -> Self {
        ZaapFile {
            name: f.name.clone(),
            size: f.size,
            hash: hex::encode(f.hash),
            chunks: f
                .effective_chunks()
                .iter()
                .map(|c| ZaapChunk { hash: hex::encode(c.hash), offset: c.offset, size: c.size })
                .collect(),
            executable: f.executable,
            symlink: f.symlink.clone(),
        }
    }
}

pub type ZaapManifest = BTreeMap<String, ZaapFile>;

/// Read `manifest.json`; a missing or corrupt file yields an empty map (we never trust it blindly anyway).
pub fn read_zaap_manifest(root: &Path) -> ZaapManifest {
    let p = root.join(ZAAP_MANIFEST);
    match std::fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            tracing::warn!("{} unreadable ({e}); ignoring", p.display());
            ZaapManifest::new()
        }),
        Err(_) => ZaapManifest::new(),
    }
}

/// (size, mtime) recorded right after a verified write or a successful full hash.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime: u64,
    #[serde(with = "hex_hash")]
    pub hash: manifest::Hash,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata, hash: manifest::Hash) -> Self {
        Stamp { size: meta.len(), mtime: mtime_ns(meta), hash }
    }
    /// Fast path: same size + mtime as recorded, and recorded hash is the expected one.
    pub fn matches(&self, meta: &std::fs::Metadata, expected: &manifest::Hash) -> bool {
        self.hash == *expected && self.size == meta.len() && self.mtime == mtime_ns(meta) && self.mtime != 0
    }
}

pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

mod hex_hash {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(h: &[u8; 20], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(h))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 20], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(D::Error::custom)?;
        v.try_into().map_err(|_| D::Error::custom("hash must be 20 bytes"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sidecar {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub release: Option<String>,
    /// Set only after a fully verified run.
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub installed_fragments: Vec<String>,
    #[serde(default)]
    pub files: BTreeMap<String, Stamp>,
}

pub fn read_sidecar(root: &Path) -> Sidecar {
    let p = root.join(SIDECAR);
    match std::fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            tracing::warn!("{} unreadable ({e}); ignoring", p.display());
            Sidecar::default()
        }),
        Err(_) => Sidecar::default(),
    }
}

pub fn write_sidecar(root: &Path, s: &Sidecar) -> Result<()> {
    let mut s = s.clone();
    s.schema = 1;
    let bytes = serde_json::to_vec(&s)?;
    crate::paths::write_atomic(&root.join(SIDECAR), &bytes).context("write state sidecar")
}

pub fn write_zaap_manifest(root: &Path, m: &ZaapManifest) -> Result<()> {
    let bytes = serde_json::to_vec(m)?;
    crate::paths::write_atomic(&root.join(ZAAP_MANIFEST), &bytes).context("write manifest.json")
}

/// Write a small JSON identity file only if its content differs (atomic).
pub fn write_json_if_changed(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Ok(b) = std::fs::read(path)
        && serde_json::from_slice::<serde_json::Value>(&b).ok().as_ref() == Some(value) {
            return Ok(());
        }
    crate::paths::write_atomic(path, &serde_json::to_vec(value)?).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zaap_manifest_roundtrip_and_tolerance() {
        let json = r#"{"a/b.txt":{"name":"a/b.txt","size":3,"hash":"00","chunks":[{"hash":"00","offset":0,"size":3}],"executable":false,"symlink":null,"done":true}}"#;
        let m: ZaapManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m["a/b.txt"].size, 3);
        let out = serde_json::to_string(&m).unwrap();
        assert!(!out.contains("done"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ZAAP_MANIFEST), b"{corrupt").unwrap();
        assert!(read_zaap_manifest(dir.path()).is_empty());
    }

    #[test]
    fn protected_names() {
        assert!(is_protected("manifest.json"));
        assert!(is_protected(".dofus-launcher.lock"));
        assert!(is_protected("Logs/dofus.123.LOG"));
        assert!(!is_protected("Dofus.exe"));
    }
}
