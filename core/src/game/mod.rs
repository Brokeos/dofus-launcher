//! `game`: zaap.yml rendering, game settings, process detection, launch.
//! See lib.rs, docs/05-implementation-plan.md and docs/02-game-launch-and-zaap-protocol.md.
//!
//! Deviations from the contract in docs/05:
//! - `render_zaap_config` takes a [`RenderContext`] holding the *stored* settings; defaults from the
//!   rendered `settings:` defs are applied while iterating (like Zaap's `refreshAvailableSettings`).
//! - `ZaapConfig::executable` is a `String` defaulting to `"zaap-start"` (Zaap default) instead of
//!   `Option<String>`; `max_instances: None` means unlimited (`maxInstances: true`). Extra fields:
//!   `settings_values` (effective values), `has_log_button`.
//! - Templates are preprocessed so Nunjucks attribute names starting with a digit
//!   (`game.settings.4kReady`) become subscripts (`game.settings["4kReady"]`) for minijinja.
//! - Fragments: `configuration` is always moved first and duplicates removed (Zaap only prepends it).
//! - `GameSessions` is `Clone` (shared `Arc` inside) instead of being wrapped in `Arc` by callers.
//! - `launch` takes an `on_exit` callback and returns [`LaunchedGame`] (`pid`, `instance_id`, `hash`).
//!   The session is removed from the registry when the game exits.
//!
//! `zaap-start` mode (Dofus 3): we run `cmd.exe /d /v:off /s /c ".\zaap-start.bat <args>"` with
//! cwd = install dir, env = inherited + ZAAP_* (zaap.yml `env` not merged, like Zaap), flags
//! `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` (DETACHED_PROCESS would cancel CREATE_NO_WINDOW and
//! leave cmd without console). Arguments are quoted libuv-style (`""` for the empty DirectX arg) and
//! values containing `%`, `"` or control characters are refused (cmd would expand them).
//! The returned pid is cmd.exe's. Real `Dofus.exe` pid discovery: the watcher thread polls a process
//! snapshot (250 ms, up to 60 s) for a `Dofus.exe` under the install dir whose parent pid is that
//! cmd.exe (the batch runs it synchronously), then opens a process handle on it (identity is held by
//! the handle, not the pid) and records it as `GameSession::game_pid`. Exit = cmd.exe exit, then the
//! game handle if cmd ended first.
pub mod config;
pub mod launch;
pub mod process;
pub mod sessions;
pub mod settings;

pub use config::{
    OsInfo, RenderContext, SettingDef, SettingOption, ZAAP_START, ZaapConfig, preprocess_template,
    render_zaap_config,
};
pub use launch::{GameExit, LaunchRequest, LaunchedGame, launch, zaap_env};
pub use process::{GAME_EXE, running_game_pids};
pub use sessions::{GameSession, GameSessions};
pub use settings::{GameSettings, SettingError};
