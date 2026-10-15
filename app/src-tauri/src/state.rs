//! Application state shared by Tauri commands, the update task and the Zaap Thrift backend.
use crate::settings::LauncherSettings;
use dofus_core::auth::AuthManager;
use dofus_core::cytrus::{self, Progress, UpdateOptions};
use dofus_core::game::{GameSessions, GameSettings, RenderContext, ZaapConfig, render_zaap_config};
use dofus_core::{DEFAULT_RELEASE, paths};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Remote metadata is refreshed at most this often (on demand, no background polling of Ankama).
const REMOTE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    Install,
    Update,
    Verify,
}

pub struct Task {
    pub kind: TaskKind,
    pub cancel: CancellationToken,
    pub progress: Option<Progress>,
}

#[derive(Clone)]
pub struct Remote {
    pub version: String,
    pub server_blocked: bool,
    pub update_blocked: bool,
    /// `zaap.yml` of the remote version, rendered with the current game settings.
    pub config: ZaapConfig,
    pub fetched_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseState {
    pub remote_version: Option<String>,
    pub installed_version: Option<String>,
    pub install_dir: String,
    pub installed: bool,
    pub update_available: bool,
    pub server_blocked: bool,
    pub update_blocked: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskState {
    pub kind: TaskKind,
    pub progress: Option<Progress>,
}

pub struct Inner {
    pub auth: AuthManager,
    pub sessions: GameSessions,
    pub settings: Mutex<LauncherSettings>,
    pub task: Mutex<Option<Task>>,
    pub remote: Mutex<Option<Remote>>,
    /// Serializes remote refreshes so concurrent callers don't fetch twice.
    pub remote_refresh: tokio::sync::Mutex<()>,
    /// Zaap Thrift server shutdown token, `Some` once listening.
    pub zaap: tokio::sync::Mutex<Option<CancellationToken>>,
}

pub type App = Arc<Inner>;

pub fn release() -> &'static str {
    DEFAULT_RELEASE
}

impl Inner {
    pub fn new(auth: AuthManager) -> App {
        Arc::new(Inner {
            auth,
            sessions: GameSessions::new(),
            settings: Mutex::new(LauncherSettings::load()),
            task: Mutex::new(None),
            remote: Mutex::new(None),
            remote_refresh: tokio::sync::Mutex::new(()),
            zaap: tokio::sync::Mutex::new(None),
        })
    }

    pub fn settings(&self) -> LauncherSettings {
        self.settings.lock().unwrap().clone()
    }

    pub fn install_dir(&self) -> PathBuf {
        self.settings.lock().unwrap().install_dir()
    }

    pub fn game_settings(&self) -> GameSettings {
        GameSettings::load(release()).unwrap_or_default()
    }

    fn render_ctx(&self) -> RenderContext {
        RenderContext::new(&self.settings().language, self.game_settings())
    }

    /// Configuration fragment of `version`, cached under `%APPDATA%\dofus-launcher\cache`.
    async fn config_dir(version: &str) -> anyhow::Result<PathBuf> {
        let dir = paths::data_dir().join("cache").join(format!("config-{}-{version}", release()));
        if !dir.join("zaap.yml").is_file() {
            let tmp = dir.with_extension("partial");
            let _ = std::fs::remove_dir_all(&tmp);
            cytrus::fetch_configuration(release(), version, &tmp).await?;
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::rename(&tmp, &dir)?;
        }
        Ok(dir)
    }

    /// Refresh remote version, release flags and remote config (cached for `REMOTE_TTL` unless `force`).
    pub async fn refresh_remote(&self, force: bool) -> anyhow::Result<Remote> {
        let _guard = self.remote_refresh.lock().await;
        if let Some(r) = self.remote.lock().unwrap().clone()
            && !force
            && r.fetched_at.elapsed() < REMOTE_TTL
        {
            return Ok(r);
        }
        let rel = cytrus::resolve(release()).await?;
        let info = cytrus::fetch_release_info(release()).await.unwrap_or_default();
        let flag = |k: &str| info.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let dir = Self::config_dir(&rel.version).await?;
        let config = render_zaap_config(&dir, &self.render_ctx())?;
        let remote = Remote {
            version: rel.version,
            server_blocked: flag("serverBlocked"),
            update_blocked: flag("updateBlocked"),
            config,
            fetched_at: Instant::now(),
        };
        *self.remote.lock().unwrap() = Some(remote.clone());
        Ok(remote)
    }

    /// Re-render the cached remote config after a game setting change (fragments may change).
    pub fn rerender_remote_config(&self) {
        let version = self.remote.lock().unwrap().as_ref().map(|r| r.version.clone());
        let Some(version) = version else { return };
        let dir = paths::data_dir().join("cache").join(format!("config-{}-{version}", release()));
        match render_zaap_config(&dir, &self.render_ctx()) {
            Ok(config) => {
                if let Some(r) = self.remote.lock().unwrap().as_mut() {
                    r.config = config;
                }
            }
            Err(e) => tracing::warn!("re-rendering remote config: {e:#}"),
        }
    }

    /// Best available config: remote (latest) or the installed one when offline.
    pub fn current_config(&self) -> Option<ZaapConfig> {
        if let Some(r) = self.remote.lock().unwrap().as_ref() {
            return Some(r.config.clone());
        }
        render_zaap_config(&self.install_dir(), &self.render_ctx()).ok()
    }

    pub fn release_state(&self) -> ReleaseState {
        let dir = self.install_dir();
        let local = cytrus::local_state(&dir).unwrap_or_default();
        let remote = self.remote.lock().unwrap().clone();
        let installed = local.installed_version.is_some();
        let update_available = match (&remote, &local.installed_version) {
            (Some(r), Some(v)) => {
                v != &r.version || !r.config.fragments.iter().all(|f| local.installed_fragments.contains(f))
            }
            _ => false,
        };
        ReleaseState {
            remote_version: remote.as_ref().map(|r| r.version.clone()),
            installed_version: local.installed_version,
            install_dir: dir.display().to_string(),
            installed,
            update_available,
            server_blocked: remote.as_ref().is_some_and(|r| r.server_blocked),
            update_blocked: remote.as_ref().is_some_and(|r| r.update_blocked),
        }
    }

    pub fn task_state(&self) -> Option<TaskState> {
        self.task
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| TaskState { kind: t.kind, progress: t.progress.clone() })
    }

    pub fn update_options(&self, dir: &Path, fragments: Vec<String>, verify_all: bool) -> UpdateOptions {
        let s = self.settings();
        let mut opts = UpdateOptions::new(release(), dir, fragments);
        opts.verify_all = verify_all;
        opts.max_concurrency = s.max_concurrency;
        opts.speed_limit_bps = s.speed_limit_bps;
        opts
    }
}
