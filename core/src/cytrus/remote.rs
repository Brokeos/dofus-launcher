//! Remote Cytrus v6 metadata: `cytrus.json`, `.meta`, `.config`, `.manifest`, release assets tar.
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{CytrusError, manifest};

/// Public CDN root (`games.cytrusUrl` of the Krosmoz universe config).
pub const CYTRUS_BASE: &str = "https://cytrus.cdn.ankama.com";

/// Cytrus platform key for this OS (`windows` on win32, otherwise `process.platform`).
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// GET with a few retries on network errors / 5xx (metadata requests are small).
pub(crate) async fn get_bytes(url: &str) -> Result<bytes::Bytes> {
    let mut delay = Duration::from_millis(500);
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..4 {
        if attempt > 0 {
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        let res = crate::http::client().get(url).timeout(Duration::from_secs(120)).send().await;
        match res {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => return Ok(b),
                Err(e) => last = Some(anyhow::Error::new(e).context(format!("GET {url}: body"))),
            },
            Ok(r) if r.status().is_server_error() || r.status().as_u16() == 429 => {
                last = Some(anyhow::anyhow!("GET {url}: HTTP {}", r.status()));
            }
            Ok(r) => bail!("GET {url}: HTTP {}", r.status()),
            Err(e) => last = Some(anyhow::Error::new(e).context(format!("GET {url}"))),
        }
    }
    Err(last.unwrap())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CytrusRepo {
    pub version: u32,
    #[serde(default)]
    pub name: String,
    pub games: std::collections::BTreeMap<String, CytrusGame>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CytrusGame {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub game_id: i64,
    #[serde(default)]
    pub assets: CytrusAssets,
    /// platform -> release -> version
    #[serde(default)]
    pub platforms: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CytrusAssets {
    /// release -> SHA-1 of the assets tar
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, String>,
}

/// `GET {base}/cytrus.json`, requiring `version == 6`.
pub async fn fetch_repo() -> Result<CytrusRepo> {
    let body = get_bytes(&format!("{CYTRUS_BASE}/cytrus.json")).await?;
    parse_repo(&body)
}

pub(crate) fn parse_repo(body: &[u8]) -> Result<CytrusRepo> {
    let v: serde_json::Value = serde_json::from_slice(body).context("cytrus.json: invalid JSON")?;
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
    if version != 6 {
        return Err(CytrusError::UnsupportedRepository(version).into());
    }
    serde_json::from_value(v).context("cytrus.json: unexpected structure")
}

pub(crate) fn release_version(repo: &CytrusRepo, release: &str) -> Result<String> {
    let game = repo.games.get(crate::DOFUS_GAME_UID).context("cytrus.json: no `dofus` game")?;
    let version = game
        .platforms
        .get(platform())
        .and_then(|p| p.get(release))
        .ok_or_else(|| CytrusError::UnknownRelease(release.to_string()))?;
    if !version.starts_with('6') {
        bail!("cytrus: version {version} of release {release} is not a v6 version");
    }
    Ok(version.clone())
}

fn release_url(release: &str, version: &str, ext: &str) -> String {
    format!("{CYTRUS_BASE}/{}/releases/{release}/{}/{version}.{ext}", crate::DOFUS_GAME_UID, platform())
}

pub(crate) fn bundle_url(hash: &manifest::Hash) -> String {
    let h = hex::encode(hash);
    format!("{CYTRUS_BASE}/{}/bundles/{}/{h}", crate::DOFUS_GAME_UID, &h[..2])
}

/// One `.meta` entry: total size of a fragment.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FragmentSize {
    pub name: String,
    pub total_size: u64,
}

/// `GET .meta` — per-fragment total sizes.
pub async fn fetch_meta(release: &str, version: &str) -> Result<Vec<FragmentSize>> {
    let body = get_bytes(&release_url(release, version, "meta")).await?;
    serde_json::from_slice(&body).context(".meta: invalid JSON")
}

/// `GET .config` — manifest with only the `configuration` fragment.
pub async fn fetch_config_manifest(release: &str, version: &str) -> Result<manifest::Manifest> {
    let body = get_bytes(&release_url(release, version, "config")).await?;
    manifest::parse(&body).context(".config: invalid manifest")
}

fn cache_path(cache_dir: &Path, release: &str, version: &str) -> PathBuf {
    cache_dir.join(format!("{release}-{version}.manifest"))
}

/// Full manifest, cached as `<cache_dir>/<release>-<version>.manifest` (atomic write). A cached file
/// that does not parse is discarded and re-downloaded. Older cached versions of the same release are pruned.
pub async fn fetch_manifest(release: &str, version: &str, cache_dir: &Path) -> Result<manifest::Manifest> {
    let path = cache_path(cache_dir, release, version);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        match tokio::task::spawn_blocking(move || manifest::parse(&bytes)).await? {
            Ok(m) => return Ok(m),
            Err(e) => tracing::warn!("cached manifest {} unreadable ({e:#}); re-downloading", path.display()),
        }
    }
    let body = get_bytes(&release_url(release, version, "manifest")).await?;
    let parsed = {
        let body = body.clone();
        tokio::task::spawn_blocking(move || manifest::parse(&body)).await?.context(".manifest: invalid manifest")?
    };
    let dir = cache_dir.to_path_buf();
    let release = release.to_string();
    let keep = path.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = crate::paths::write_atomic(&keep, &body) {
            tracing::warn!("cannot cache manifest {}: {e}", keep.display());
            return;
        }
        // Prune other cached versions of this release.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(&format!("{release}-")) && name.ends_with(".manifest") && e.path() != keep {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    })
    .await?;
    Ok(parsed)
}

/// Assets tar `{game}/hashes/xx/<hash>`, then `release.json` extracted.
pub async fn fetch_release_info_from(repo: &CytrusRepo, release: &str) -> Result<serde_json::Value> {
    let game = repo.games.get(crate::DOFUS_GAME_UID).context("cytrus.json: no `dofus` game")?;
    let hash = game.assets.meta.get(release).ok_or_else(|| CytrusError::UnknownRelease(release.to_string()))?;
    if hash.len() < 2 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("cytrus.json: invalid assets hash {hash:?}");
    }
    let url = format!("{CYTRUS_BASE}/{}/hashes/{}/{hash}", crate::DOFUS_GAME_UID, &hash[..2]);
    // Observed 2026-10-15: the assets key is a content address but NOT the SHA-1 of the tar bytes,
    // so integrity relies on the tar structure + JSON parsing.
    let tar = get_bytes(&url).await?;
    let data = tar_entry(&tar, "release.json")?.context("release assets: no release.json in tar")?;
    serde_json::from_slice(data).context("release.json: invalid JSON")
}

/// Minimal POSIX/ustar reader: returns the content of the regular file whose name (or basename) is `wanted`.
pub(crate) fn tar_entry<'a>(tar: &'a [u8], wanted: &str) -> Result<Option<&'a [u8]>> {
    let mut pos = 0usize;
    while pos + 512 <= tar.len() {
        let hdr = &tar[pos..pos + 512];
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let cstr = |b: &[u8]| -> String {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf8_lossy(&b[..end]).into_owned()
        };
        let mut name = cstr(&hdr[0..100]);
        if &hdr[257..262] == b"ustar" {
            let prefix = cstr(&hdr[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let size_field = cstr(&hdr[124..136]);
        let size = u64::from_str_radix(size_field.trim().trim_matches('\0'), 8)
            .with_context(|| format!("tar: invalid size {size_field:?}"))? as usize;
        let typeflag = hdr[156];
        let data_start = pos + 512;
        let data_end = data_start.checked_add(size).filter(|e| *e <= tar.len()).context("tar: truncated entry")?;
        let base = name.trim_start_matches("./");
        if (typeflag == b'0' || typeflag == 0) && (base == wanted || base.rsplit('/').next() == Some(wanted)) {
            return Ok(Some(&tar[data_start..data_end]));
        }
        pos = data_start + size.div_ceil(512) * 512;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cytrus").join(name)).unwrap()
    }

    #[test]
    fn parses_cytrus_json_sample() {
        let repo = parse_repo(&sample("cytrus.json")).unwrap();
        assert_eq!(repo.version, 6);
        let v = release_version(&repo, "dofus3").unwrap();
        assert_eq!(v, "6.0_3.7.10.10");
        assert!(release_version(&repo, "nope").is_err());
        assert!(repo.games["dofus"].assets.meta.contains_key("dofus3"));
    }

    #[test]
    fn rejects_other_repo_versions() {
        let e = parse_repo(br#"{"version":5,"games":{}}"#).unwrap_err();
        assert!(matches!(e.downcast_ref::<CytrusError>(), Some(CytrusError::UnsupportedRepository(5))));
    }

    #[test]
    fn meta_sample_parses() {
        let m: Vec<FragmentSize> = serde_json::from_slice(&sample("dofus-dofus3-windows-6.0_3.7.10.10.meta")).unwrap();
        assert!(m.iter().any(|f| f.name == "configuration" && f.total_size == 7879));
    }

    fn tar_header(name: &str, size: usize) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let s = format!("{size:011o}\0");
        h[124..136].copy_from_slice(s.as_bytes());
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    #[test]
    fn tar_reader_finds_entry() {
        let mut tar = Vec::new();
        for (name, data) in [("icon.png", &b"PNGDATA"[..]), ("release.json", br#"{"serverBlocked":false}"#)] {
            tar.extend(tar_header(name, data.len()));
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend(vec![0u8; 1024]);
        assert_eq!(tar_entry(&tar, "release.json").unwrap().unwrap(), br#"{"serverBlocked":false}"#);
        assert!(tar_entry(&tar, "missing").unwrap().is_none());
        assert!(tar_entry(&tar[..515], "icon.png").is_err());
    }
}
