//! Cytrus v6: remote metadata, FlatBuffers manifest, install / update / repair engine.
//! Spec: docs/03-cytrus-updates.md (section 6), contract: docs/05-implementation-plan.md.
//!
//! # Deviations from the contract in 05-implementation-plan.md
//! - `run_update` takes `on_progress: impl Fn(Progress) + Send + Sync + 'static` as specified and a
//!   `CancellationToken`. Errors carry a [`CytrusError`] (downcast with `err.downcast_ref::<CytrusError>()`)
//!   for the cases the UI must distinguish: `Locked`, `GameRunning`, `Cancelled`, `InsufficientSpace`,
//!   `UnknownRelease`, `UnsupportedRepository`.
//! - `UpdateOptions` gains `UpdateOptions::new(release, install_dir, fragments)` (defaults:
//!   `verify_all=false`, `max_concurrency=6`, no speed limit). `max_concurrency == 0` means 6.
//! - `Progress`/`Phase` serialize in camelCase (`bytesDone`, `"downloading"`…) for the TS UI.
//! - Extra public items: [`fetch_meta`], [`remote::FragmentSize`], [`remote::fetch_repo`], [`normalize_fragments`].
//! - `LocalState` also exposes `release: Option<String>`.
//!
//! # Semantics
//! - Fragments are de-duplicated, `configuration` is forced first. Unknown fragment names are skipped (warning).
//! - Fast path: a file is trusted when the sidecar `(size, mtime)` stamp matches the disk and its recorded
//!   hash equals the manifest hash. Otherwise the file is re-hashed (only if its size matches; a size
//!   mismatch means rebuild). `verify_all` re-hashes every target file (repair).
//! - Chunks are reused by hash from any local file (verified with SHA-1 on read, network fallback).
//!   Partially written `<path>.dl-tmp` files are resumed chunk by chunk after SHA-1 verification.
//! - Cleanup (only after full success) deletes files known to the *previous* state (`manifest.json` or
//!   sidecar) that are no longer in the selected fragments, plus their stale `.dl-tmp`. Unlike Zaap's
//!   repair, we never walk-and-delete unknown files (user screenshots, mods, game-created files survive).
//!   Never deleted: `manifest.json`, `.dofus-launcher-state.json`, `.dofus-launcher.lock`,
//!   `.release.infos.json`, `migration.launcher.json`, `*.log`.
//! - Rule 5 (game running) is enforced here too: any process whose image lives under the install dir
//!   blocks `run_update` with `CytrusError::GameRunning`.
//! - Space check: bytes to download (upper bound, before local reuse) + largest rebuilt file + 64 MiB.
//! - Symlink entries (never observed on Windows) are created on Unix only; skipped with a warning on Windows.
//! - Spec 03 says `assets.meta[release]` is the SHA-1 of the assets tar: observed false (dofus3 key
//!   `457dc7b4…`, tar SHA-1 `1150de1b…`), so the tar is not hash-checked.
//! - Per-file fsync is not done before rename (performance); after a power loss use verify_all.

pub mod download;
mod engine;
pub mod manifest;
pub mod remote;
pub mod state;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

pub use engine::{fetch_configuration, run_update};
pub use remote::{CYTRUS_BASE, FragmentSize, fetch_meta, platform};

#[derive(Debug, thiserror::Error)]
pub enum CytrusError {
    #[error("cytrus.json version {0} is not supported (expected 6)")]
    UnsupportedRepository(u64),
    #[error("release `{0}` not found in cytrus.json")]
    UnknownRelease(String),
    #[error("another update is already running on this install directory")]
    Locked,
    #[error("the game is running from this install directory (pid {0:?}); close it first")]
    GameRunning(Vec<u32>),
    #[error("not enough disk space: {needed} bytes needed, {available} available")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("update cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteRelease {
    pub game: String,
    pub release: String,
    /// e.g. `6.0_3.7.10.10`
    pub version: String,
}

/// Resolve the current remote version of `release` from `cytrus.json` (requires repository version 6).
pub async fn resolve(release: &str) -> Result<RemoteRelease> {
    let repo = remote::fetch_repo().await?;
    let version = remote::release_version(&repo, release)?;
    Ok(RemoteRelease { game: crate::DOFUS_GAME_UID.to_string(), release: release.to_string(), version })
}

/// Assets `release.json` of a release (`serverBlocked`, `updateBlocked`, `multiAccount`…).
pub async fn fetch_release_info(release: &str) -> Result<serde_json::Value> {
    let repo = remote::fetch_repo().await?;
    remote::fetch_release_info_from(&repo, release).await
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalState {
    pub release: Option<String>,
    /// Set only after a fully verified install/update.
    pub installed_version: Option<String>,
    pub installed_fragments: Vec<String>,
}

/// Read our sidecar state from the install dir (missing/corrupt => empty state).
pub fn local_state(install_dir: &Path) -> Result<LocalState> {
    let s = state::read_sidecar(install_dir);
    Ok(LocalState { release: s.release, installed_version: s.installed_version, installed_fragments: s.installed_fragments })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Preparing,
    Checking,
    Downloading,
    Finalizing,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub speed_bps: u64,
}

#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub release: String,
    pub install_dir: PathBuf,
    /// From the rendered zaap.yml; de-duplicated, `configuration` forced first.
    pub fragments: Vec<String>,
    /// Repair: re-hash every file instead of trusting the (size, mtime) stamps.
    pub verify_all: bool,
    pub max_concurrency: usize,
    pub speed_limit_bps: Option<u64>,
}

impl UpdateOptions {
    pub fn new(release: impl Into<String>, install_dir: impl Into<PathBuf>, fragments: Vec<String>) -> Self {
        UpdateOptions {
            release: release.into(),
            install_dir: install_dir.into(),
            fragments,
            verify_all: false,
            max_concurrency: 6,
            speed_limit_bps: None,
        }
    }
}

/// De-duplicate preserving order, with `configuration` first.
pub fn normalize_fragments(fragments: &[String]) -> Vec<String> {
    let mut out = vec!["configuration".to_string()];
    for f in fragments {
        let f = f.trim();
        if !f.is_empty() && !out.iter().any(|x| x == f) {
            out.push(f.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_are_deduped_configuration_first() {
        let v: Vec<String> = ["win32_x64", "start-script", "configuration", "bones", "start-script"].iter().map(|s| s.to_string()).collect();
        assert_eq!(normalize_fragments(&v), vec!["configuration", "win32_x64", "start-script", "bones"]);
    }

    #[test]
    fn progress_serializes_camel_case() {
        let p = Progress { phase: Phase::Downloading, bytes_done: 1, bytes_total: 2, files_done: 0, files_total: 1, speed_bps: 3 };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"phase\":\"downloading\"") && s.contains("\"bytesDone\":1") && s.contains("\"speedBps\":3"), "{s}");
    }
}
