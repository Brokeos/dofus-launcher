//! Dofus-only replacement for the Ankama Launcher (Zaap 3.15.5) — core logic.
//! Specs: ../docs/01..04 (reverse-engineered with rea). Module owners:
//! - `auth`   : OAuth PKCE login, Haapi client, Shield certificates, secure account store.
//! - `cytrus` : Cytrus v6 metadata, FlatBuffers manifest, install/update/repair engine.
//! - `game`   : zaap.yml rendering, game settings, process detection, launch.
//! - `zaap`   : local Thrift `ZaapService` server the game talks to (127.0.0.1:26116).
//! - `extras` : news and server status (public endpoints).

pub mod auth;
pub mod cytrus;
pub mod extras;
pub mod game;
pub mod http;
pub mod paths;
pub mod zaap;

/// Version string we present as (official launcher version reproduced on the wire).
pub const ZAAP_VERSION: &str = "3.15.5";
/// Launcher application id (`ZAAP_GAME_ID`, OAuth `client_id`).
pub const LAUNCHER_GAME_ID: i32 = 102;
/// Dofus Haapi game id.
pub const DOFUS_GAME_ID: i32 = 1;
/// Cytrus game uid for Dofus.
pub const DOFUS_GAME_UID: &str = "dofus";
/// Default Dofus 3 release in cytrus.json.
pub const DEFAULT_RELEASE: &str = "dofus3";
