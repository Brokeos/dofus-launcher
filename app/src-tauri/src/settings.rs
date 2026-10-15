//! Launcher settings, persisted as plain JSON in `%APPDATA%\dofus-launcher\settings.json`.
use dofus_core::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseOnLaunch {
    /// Minimize the window once the game is started.
    Minimize,
    /// Keep the window as is.
    Keep,
    /// Hide the window; the launcher exits when the last game instance exits
    /// (it must stay alive meanwhile: the game asks it for login tokens).
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LauncherSettings {
    pub install_dir: String,
    pub close_on_launch: CloseOnLaunch,
    /// Bytes/s, `None` = unlimited.
    pub speed_limit_bps: Option<u64>,
    pub max_concurrency: usize,
    pub language: String,
    /// Portrait chosen per account: account id -> "<classId>-<0|1>" (male/female).
    pub avatars: BTreeMap<String, String>,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        LauncherSettings {
            install_dir: paths::default_install_dir(dofus_core::DEFAULT_RELEASE).display().to_string(),
            close_on_launch: CloseOnLaunch::Minimize,
            speed_limit_bps: None,
            max_concurrency: 6,
            language: "fr".into(),
            avatars: BTreeMap::new(),
        }
    }
}

impl LauncherSettings {
    fn path() -> PathBuf {
        paths::data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        paths::write_atomic(&Self::path(), &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Clamp values coming from the UI.
    pub fn sanitized(mut self) -> Self {
        self.max_concurrency = self.max_concurrency.clamp(1, 16);
        if self.speed_limit_bps == Some(0) {
            self.speed_limit_bps = None;
        }
        if !matches!(self.language.as_str(), "fr" | "en" | "es" | "de" | "pt") {
            self.language = "fr".into();
        }
        self.avatars.retain(|_, v| is_valid_avatar(v));
        if self.install_dir.trim().is_empty() {
            self.install_dir = LauncherSettings::default().install_dir;
        }
        self
    }

    pub fn install_dir(&self) -> PathBuf {
        PathBuf::from(&self.install_dir)
    }
}

/// Dofus 3 breed ids (19 is unused) and sex 0/1, e.g. `"16-0"` = male Eliotrope.
pub fn is_valid_avatar(v: &str) -> bool {
    let Some((class, sex)) = v.split_once('-') else { return false };
    matches!(class.parse::<u8>(), Ok(1..=18 | 20)) && matches!(sex, "0" | "1")
}
