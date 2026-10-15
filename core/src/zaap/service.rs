//! `ZaapService` handlers (spec 02 §2.3-2.4). Transport-independent: used by the Thrift server and
//! the text socket.
use std::sync::Arc;

use super::Backend;
use super::protocol::{TStruct, TValue, ttype};
use crate::game::{GameSession, GameSessions};

/// `ErrorCode` values used by our handlers.
pub mod code {
    pub const UNKNOWN: i32 = 1;
    pub const UNAUTHORIZED: i32 = 2;
    pub const INVALID_GAME_SESSION: i32 = 3;
    pub const INVALID_CREDENTIALS: i32 = 1002;
    pub const AUTH_WRONG_WINDOW_ID: i32 = 2018;
    pub const AUTH_NO_MORE_ACCOUNT_AVAILABLE: i32 = 2020;
    pub const SETTINGS_KEY_NOT_FOUND: i32 = 4001;
    pub const SETTINGS_INVALID_VALUE: i32 = 4002;
    pub const USER_INFO_UNAVAILABLE: i32 = 5001;
    pub const OVERLAY_UNEXPECTED_ERROR: i32 = 6003;
    pub const SYSTEM_NOTIFICATION_UNEXPECTED_ERROR: i32 = 7002;
    pub const USER_NOT_GUEST: i32 = 8002;
}

/// `exception ZaapError { 1: required i32 code, 2: optional string details }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaapError {
    pub code: i32,
    pub details: Option<String>,
}

impl ZaapError {
    pub fn new(code: i32) -> Self {
        ZaapError {
            code,
            details: None,
        }
    }
    pub fn to_struct(&self) -> TStruct {
        let s = TStruct::new().with(1, TValue::I32(self.code));
        match &self.details {
            Some(d) => s.with(2, TValue::string(d.clone())),
            None => s,
        }
    }
}

/// Handler outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `None` for `void` methods.
    Success(Option<TValue>),
    /// Encoded as result field 1.
    Error(ZaapError),
    /// Any other failure → `TApplicationException(UNKNOWN, message)`.
    AppException(String),
}

/// Argument shapes (field id, thrift type) of each IDL method; payArticle* use negative ids.
pub fn method_params(name: &str) -> Option<&'static [(i16, u8)]> {
    use ttype::{BOOL, I32, STRING, STRUCT};
    Some(match name {
        "connect" => &[(1, STRING), (2, STRING), (3, I32), (4, STRING)],
        "auth_getGameToken" => &[(1, STRING), (2, I32)],
        "updater_isUpdateAvailable"
        | "askForGuestComplete"
        | "userInfo_get"
        | "release_restartOnExit"
        | "zaapVersion_get"
        | "zaapMustUpdate_get"
        | "hasPremiumAccess" => &[(1, STRING)],
        "settings_get" => &[(1, STRING), (2, STRING)],
        "settings_set" => &[(1, STRING), (2, STRING), (3, STRING)],
        "release_exitAndRepair" => &[(1, STRING), (2, BOOL)],
        "payArticle" => &[(1, STRING), (-1, STRING), (-2, I32), (-3, STRUCT)],
        "payArticleWithPid" => &[
            (1, STRING),
            (-1, STRING),
            (-2, I32),
            (-3, I32),
            (-4, STRUCT),
        ],
        "payCart" => &[
            (1, STRING),
            (-1, STRING),
            (-2, STRING),
            (-3, STRING),
            (-4, STRUCT),
        ],
        "payCartWithPid" => &[
            (1, STRING),
            (-1, STRING),
            (-2, STRING),
            (-3, STRING),
            (-4, I32),
            (-5, STRUCT),
        ],
        "hasGameAccess" => &[(1, STRING), (2, I32)],
        "auth_getGameTokenWithWindowId" => &[(1, STRING), (2, I32), (3, I32)],
        "system_addNotification" | "addNotification" => &[(1, STRING), (2, STRUCT)],
        _ => return None,
    })
}

/// Result struct has an `error` field (all but hasPremiumAccess / hasGameAccess).
pub fn has_error_field(name: &str) -> bool {
    !matches!(name, "hasPremiumAccess" | "hasGameAccess")
}

#[derive(Clone)]
pub struct Service {
    pub sessions: GameSessions,
    pub backend: Arc<dyn Backend>,
}

fn ok_str(s: impl Into<String>) -> Outcome {
    Outcome::Success(Some(TValue::string(s)))
}
fn ok_bool(b: bool) -> Outcome {
    Outcome::Success(Some(TValue::Bool(b)))
}
fn err(code: i32) -> Outcome {
    Outcome::Error(ZaapError::new(code))
}

impl Service {
    fn authorize(&self, args: &TStruct) -> Result<GameSession, Outcome> {
        let gs = args.get_str(1).unwrap_or_default();
        self.sessions
            .get(gs)
            .ok_or_else(|| err(code::INVALID_GAME_SESSION))
    }

    async fn token(&self, account_id: i64, game_id: i32) -> Outcome {
        match self.backend.create_game_token(account_id, game_id).await {
            Ok(t) => ok_str(t),
            Err(e) => {
                tracing::warn!(account_id, "create_game_token failed: {e:#}");
                Outcome::AppException(format!("{e:#}"))
            }
        }
    }

    /// Dispatch by method name. `None` = unknown method.
    pub async fn call(&self, name: &str, args: &TStruct) -> Option<Outcome> {
        method_params(name)?;
        let out = match self.handle(name, args).await {
            Ok(o) | Err(o) => o,
        };
        Some(out)
    }

    async fn handle(&self, name: &str, a: &TStruct) -> Result<Outcome, Outcome> {
        Ok(match name {
            "connect" => {
                // Only the hash is checked (Zaap ignores the other arguments).
                let hash = a.get_str(4).unwrap_or_default();
                match self.sessions.mark_connected(hash) {
                    Some(s) => {
                        tracing::info!(instance_id = s.instance_id, "game connected");
                        ok_str(s.hash)
                    }
                    None => err(code::INVALID_CREDENTIALS),
                }
            }
            "auth_getGameToken" => {
                let s = self.authorize(a)?;
                let game_id = a.get_i32(2).unwrap_or(crate::DOFUS_GAME_ID);
                match s.account_id {
                    Some(id) => self.token(id, game_id).await,
                    None => Outcome::AppException(
                        "Trying to connect an account not registered in apiKeys".into(),
                    ),
                }
            }
            "auth_getGameTokenWithWindowId" => {
                self.authorize(a)?;
                let game_id = a.get_i32(2).unwrap_or(crate::DOFUS_GAME_ID);
                let window = a.get_i32(3).unwrap_or(0);
                if window <= 0 {
                    return Err(err(code::AUTH_WRONG_WINDOW_ID));
                }
                match self.backend.active_account_ids().get(window as usize - 1) {
                    Some(&id) => self.token(id, game_id).await,
                    None => err(code::AUTH_NO_MORE_ACCOUNT_AVAILABLE),
                }
            }
            "updater_isUpdateAvailable" => {
                self.authorize(a)?;
                ok_bool(self.backend.is_update_available())
            }
            "settings_get" => {
                self.authorize(a)?;
                match self
                    .backend
                    .game_setting_get(a.get_str(2).unwrap_or_default())
                {
                    Some(json) => ok_str(json),
                    None => err(code::SETTINGS_KEY_NOT_FOUND),
                }
            }
            "settings_set" => {
                self.authorize(a)?;
                let key = a.get_str(2).unwrap_or_default();
                let value = a.get_str(3).unwrap_or_default();
                if self.backend.game_setting_get(key).is_none() {
                    return Err(err(code::SETTINGS_KEY_NOT_FOUND));
                }
                if serde_json::from_str::<serde_json::Value>(value).is_err() {
                    return Err(err(code::SETTINGS_INVALID_VALUE));
                }
                match self.backend.game_setting_set(key, value) {
                    Ok(()) => Outcome::Success(None),
                    Err(_) => err(code::SETTINGS_INVALID_VALUE),
                }
            }
            "askForGuestComplete" => {
                self.authorize(a)?;
                err(code::USER_NOT_GUEST)
            }
            "userInfo_get" => {
                let s = self.authorize(a)?;
                match s.account_id.and_then(|id| self.backend.user_info_json(id)) {
                    Some(json) => ok_str(json),
                    None => err(code::USER_INFO_UNAVAILABLE),
                }
            }
            "release_restartOnExit" => {
                let s = self.authorize(a)?;
                self.sessions.set_restart_on_exit(&s.hash);
                self.backend.on_restart_on_exit(s.instance_id);
                Outcome::Success(None)
            }
            "release_exitAndRepair" => {
                let s = self.authorize(a)?;
                self.backend
                    .on_exit_and_repair(s.instance_id, a.get_bool(2).unwrap_or(false));
                Outcome::Success(None)
            }
            "zaapVersion_get" => {
                self.authorize(a)?;
                ok_str(serde_json::to_string(crate::ZAAP_VERSION).unwrap_or_default())
            }
            "zaapMustUpdate_get" => {
                self.authorize(a)?;
                ok_bool(false)
            }
            "payArticle" | "payArticleWithPid" | "payCart" | "payCartWithPid" => {
                self.authorize(a)?;
                err(code::OVERLAY_UNEXPECTED_ERROR)
            }
            "system_addNotification" | "addNotification" => {
                self.authorize(a)?;
                err(code::SYSTEM_NOTIFICATION_UNEXPECTED_ERROR)
            }
            // No error field in these results: answer false for an unknown session.
            "hasPremiumAccess" => ok_bool(false),
            "hasGameAccess" => {
                ok_bool(self.authorize(a).is_ok() && a.get_i32(2) == Some(crate::DOFUS_GAME_ID))
            }
            _ => unreachable!("method_params covers every dispatched name"),
        })
    }
}

/// Result struct for a REPLY: field 0 = success, field 1 = error.
pub fn result_struct(name: &str, out: &Outcome) -> Option<TStruct> {
    match out {
        Outcome::Success(None) => Some(TStruct::new()),
        Outcome::Success(Some(v)) => Some(TStruct::new().with(0, v.clone())),
        Outcome::Error(e) if has_error_field(name) => {
            Some(TStruct::new().with(1, TValue::Struct(e.to_struct())))
        }
        // Zaap sends an empty REPLY here (spec 02 §2.5).
        Outcome::Error(_) => Some(TStruct::new()),
        Outcome::AppException(_) => None,
    }
}
