//! Ankama authentication: OAuth2 PKCE (system browser + loopback redirect), Haapi session,
//! Shield certificates, multi-account secure store. Spec: docs/01-auth-haapi.md (section 10 recipe).
//!
//! Contract refinements vs docs/05-implementation-plan.md (semantics unchanged):
//! - `AccountInfo` serializes camelCase and gains `needs_login` (401 / failed restore / unreadable
//!   secrets: a browser login is required), `account_type` and `avatar_url`.
//! - `AuthManager::load_from(dir)` (tests / custom data dir) and `set_language(lang)` (RefreshApiKey `lang`).
//! - Errors are `AuthError` (French UI messages) wrapped in `anyhow`; use `err.downcast_ref::<AuthError>()`.
//! - Logging in again with an already-stored account refreshes its session (Zaap refuses duplicates);
//!   its stored Shield certificate is kept.
//! - `logout` removes the account entirely (DeleteApiKey best effort, secrets incl. certificate deleted),
//!   like Zaap `removeAccount`; the main flag moves to another account if needed.
//! - `create_game_token`: 401 sets `needs_login`, 403 sets `errored`; a certificate that no longer
//!   decrypts with this machine's hm2 is dropped (Zaap deletes it) and the call proceeds without it.
//! - `restore_sessions` returns `Err` only for transient failures (network, 429, VPN refusal) so the
//!   app can retry later; other per-account failures set `needs_login`.

mod callback;
mod error;
mod haapi;
mod pkce;
pub mod shield;
mod store;

pub use error::AuthError;
pub use haapi::HAAPI_BASE;
pub use pkce::{REDIRECT_URI, authorize_url, code_challenge, code_verifier};
pub use store::AccountInfo;

use haapi::{CodeSent, Haapi, id_field, scalar_string};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use store::{AccountRecord, Secrets, Store};
use tokio_util::sync::CancellationToken;

/// Browser login wait limit.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const REFRESH_THROTTLE_MS: i64 = 172_800_000; // 48 h
const SECONDARY_REFRESH_MS: i64 = 1_296_000_000; // 15 days

/// Result of a browser login.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginOutcome {
    LoggedIn { account: AccountInfo },
    /// Shield validation needed: call `shield_validate` with the e-mail/SMS code (or the
    /// authenticator code when `otp`). `domain` is the masked e-mail domain.
    #[serde(rename_all = "camelCase")]
    ShieldRequired { account_id: i64, domain: Option<String>, otp: bool },
}

/// Key awaiting Shield validation (in memory only, like Zaap `temporaryApiKey`).
#[derive(Clone)]
struct Pending {
    key: String,
    refresh_token: Option<String>,
    account: Value,
    otp: bool,
}

#[derive(Default)]
struct State {
    accounts: Vec<AccountRecord>,
    secrets: HashMap<i64, Secrets>,
    pending: HashMap<i64, Pending>,
}

struct Inner {
    store: Store,
    haapi: Haapi,
    state: Mutex<State>,
    lang: Mutex<String>,
    login: Mutex<Option<(u64, CancellationToken)>>,
    login_gen: std::sync::atomic::AtomicU64,
    account_locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    fingerprint: OnceLock<shield::Fingerprint>,
}

/// Thread-safe account/session manager. Cheap to clone.
#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager").field("accounts", &self.accounts().len()).finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_transient(e: &AuthError) -> bool {
    matches!(e, AuthError::Network(_) | AuthError::RateLimited | AuthError::AnonymousIpForbidden)
}

async fn cancellable<T>(
    token: &CancellationToken,
    fut: impl std::future::Future<Output = Result<T, AuthError>>,
) -> Result<T, AuthError> {
    tokio::select! {
        _ = token.cancelled() => Err(AuthError::LoginCancelled),
        r = fut => r,
    }
}

impl AuthManager {
    /// Reads `data_dir()/accounts.json` and the DPAPI secrets.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(crate::paths::data_dir())
    }

    /// Same as `load` with an explicit data directory.
    pub fn load_from(dir: PathBuf) -> anyhow::Result<Self> {
        let store = Store::new(dir);
        let mut accounts = store.load_accounts()?;
        let mut secrets = HashMap::new();
        for rec in &mut accounts {
            match store.load_secrets(rec.info.id) {
                Ok(Some(s)) => {
                    secrets.insert(rec.info.id, s);
                }
                Ok(None) => rec.info.needs_login = true,
                Err(e) => {
                    tracing::warn!(account = rec.info.id, error = %e, "secrets illisibles");
                    rec.info.needs_login = true;
                }
            }
        }
        Ok(AuthManager {
            inner: Arc::new(Inner {
                store,
                haapi: Haapi::default(),
                state: Mutex::new(State { accounts, secrets, pending: HashMap::new() }),
                lang: Mutex::new("fr".into()),
                login: Mutex::new(None),
                login_gen: Default::default(),
                account_locks: Mutex::new(HashMap::new()),
                fingerprint: OnceLock::new(),
            }),
        })
    }

    /// Language sent to RefreshApiKey (`fr`, `en`, ...).
    pub fn set_language(&self, lang: &str) {
        *lock(&self.inner.lang) = lang.to_string();
    }

    fn fingerprint(&self) -> &shield::Fingerprint {
        self.inner.fingerprint.get_or_init(shield::Fingerprint::current)
    }

    fn account_lock(&self, id: i64) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.account_locks).entry(id).or_default().clone()
    }

    fn persist_accounts(&self, st: &State) -> Result<(), AuthError> {
        self.inner.store.save_accounts(&st.accounts)
    }

    /// Apply `f` to an account's info and persist.
    fn update_info(&self, id: i64, f: impl FnOnce(&mut AccountInfo)) -> Result<(), AuthError> {
        let mut st = lock(&self.inner.state);
        let rec = st.accounts.iter_mut().find(|r| r.info.id == id).ok_or(AuthError::UnknownAccount(id))?;
        f(&mut rec.info);
        self.persist_accounts(&st)
    }

    fn save_secrets(&self, id: i64, s: &Secrets) -> Result<(), AuthError> {
        self.inner.store.save_secrets(id, s)?;
        lock(&self.inner.state).secrets.insert(id, s.clone());
        Ok(())
    }

    /// SignOn + throttled refresh for stored accounts (main first; secondaries refreshed after 15 days).
    pub async fn restore_sessions(&self) -> anyhow::Result<()> {
        let mut ids: Vec<(i64, bool)> = {
            let st = lock(&self.inner.state);
            st.accounts
                .iter()
                .filter(|r| !r.info.needs_login && st.secrets.contains_key(&r.info.id))
                .map(|r| (r.info.id, r.info.is_main))
                .collect()
        };
        ids.sort_by_key(|(_, main)| !main);
        for (id, main) in ids {
            let guard = self.account_lock(id);
            let _g = guard.lock().await;
            let Some(secrets) = lock(&self.inner.state).secrets.get(&id).cloned() else { continue };
            match self.inner.haapi.sign_on(&secrets.key).await {
                Ok(account) => self.apply_account(id, &account, None)?,
                Err(e) if is_transient(&e) => return Err(e.into()),
                Err(e) => {
                    tracing::warn!(account = id, error = %e, "restauration de session échouée");
                    self.update_info(id, |i| i.needs_login = true)?;
                    continue;
                }
            }
            let throttle = if main { REFRESH_THROTTLE_MS } else { SECONDARY_REFRESH_MS };
            match self.refresh_if_due(id, secrets, throttle).await {
                Ok(_) => {}
                Err(e) if is_transient(&e) => return Err(e.into()),
                Err(e) if e.status() == Some(401) => self.update_info(id, |i| i.needs_login = true)?,
                Err(e) => tracing::warn!(account = id, error = %e, "RefreshApiKey échoué"),
            }
        }
        Ok(())
    }

    /// RefreshApiKey when `refresh_date` is older than `throttle_ms`; persists the new refresh token
    /// (and key when the server returns a different one).
    async fn refresh_if_due(&self, id: i64, mut s: Secrets, throttle_ms: i64) -> Result<Secrets, AuthError> {
        let Some(refresh_token) = s.refresh_token.clone() else { return Ok(s) };
        if s.refresh_date.is_some_and(|d| d + throttle_ms > now_ms()) {
            return Ok(s);
        }
        let lang = lock(&self.inner.lang).clone();
        let r = self.inner.haapi.refresh(&s.key, &refresh_token, &lang).await?;
        if let Some(rt) = r.refresh_token.filter(|t| !t.is_empty()) {
            s.refresh_token = Some(rt);
        }
        if let Some(k) = r.key.filter(|k| !k.is_empty() && *k != s.key) {
            s.key = k;
        }
        s.refresh_date = Some(now_ms());
        self.save_secrets(id, &s)?;
        Ok(s)
    }

    /// Full OAuth PKCE flow: binds 127.0.0.1:9001, opens the browser via `open_url`, waits up to
    /// 10 minutes for the redirect. A new call cancels a login already in progress.
    pub async fn login_with_browser(&self, open_url: impl Fn(&str) + Send) -> anyhow::Result<LoginOutcome> {
        let generation = self.inner.login_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let token = CancellationToken::new();
        if let Some((_, prev)) = lock(&self.inner.login).replace((generation, token.clone())) {
            prev.cancel();
        }
        let result = self.login_flow(&token, open_url).await;
        let mut slot = lock(&self.inner.login);
        if slot.as_ref().is_some_and(|(g, _)| *g == generation) {
            *slot = None;
        }
        Ok(result?)
    }

    async fn login_flow(&self, token: &CancellationToken, open_url: impl Fn(&str) + Send) -> Result<LoginOutcome, AuthError> {
        // A just-cancelled login may still hold the port for a moment.
        let mut listener = callback::bind().await;
        for _ in 0..10 {
            if !matches!(listener, Err(AuthError::PortBusy)) {
                break;
            }
            cancellable(token, async { tokio::time::sleep(Duration::from_millis(100)).await; Ok(()) }).await?;
            listener = callback::bind().await;
        }
        let listener = listener?;
        let verifier = code_verifier();
        open_url(&authorize_url(&code_challenge(&verifier)));
        let redirect = cancellable(token, async {
            tokio::time::timeout(LOGIN_TIMEOUT, callback::wait_for_redirect(&listener))
                .await
                .map_err(|_| AuthError::LoginTimeout)
        })
        .await?;
        drop(listener);
        let code = match redirect {
            callback::Redirect::Code(c) => c,
            callback::Redirect::Error(e) => return Err(AuthError::OAuth(e)),
        };
        let haapi = &self.inner.haapi;
        let tokens = cancellable(token, haapi.exchange_code(&code, &verifier)).await?;
        let key = tokens.access_token;
        let account = cancellable(token, haapi.account(&key)).await?;
        let id = id_field(&account, "id").ok_or_else(|| AuthError::InvalidResponse("id de compte absent".into()))?;
        let security = string_list(account.get("security"));
        tracing::info!(account = id, "connexion OAuth réussie");

        if security.iter().any(|s| s == "SHIELD") {
            let otp = match cancellable(token, haapi.security_code(&key, false)).await? {
                CodeSent::Sent { domain } => Some((false, domain)),
                CodeSent::Otp => Some((true, None)),
                CodeSent::NotNeeded => None,
            };
            if let Some((otp, domain)) = otp {
                lock(&self.inner.state)
                    .pending
                    .insert(id, Pending { key, refresh_token: tokens.refresh_token, account, otp });
                return Ok(LoginOutcome::ShieldRequired { account_id: id, domain, otp });
            }
        }
        let info = cancellable(token, self.finalize(id, key, tokens.refresh_token, None, account)).await?;
        Ok(LoginOutcome::LoggedIn { account: info })
    }

    /// Cancels a login in progress (no-op otherwise).
    pub async fn cancel_login(&self) {
        if let Some((_, t)) = lock(&self.inner.login).take() {
            t.cancel();
        }
    }

    /// SignOnWithApiKey + RefreshApiKey, then store secrets and metadata.
    async fn finalize(
        &self,
        id: i64,
        key: String,
        refresh_token: Option<String>,
        certificate: Option<Value>,
        account_account: Value,
    ) -> Result<AccountInfo, AuthError> {
        let guard = self.account_lock(id);
        let _g = guard.lock().await;
        let account = self.inner.haapi.sign_on(&key).await?;
        let previous_cert = lock(&self.inner.state).secrets.get(&id).and_then(|s| s.certificate.clone());
        let secrets = Secrets { key, refresh_token, refresh_date: None, certificate: certificate.or(previous_cert) };
        self.save_secrets(id, &secrets)?;
        self.apply_account(id, &account, Some(&account_account))?;
        if let Err(e) = self.refresh_if_due(id, secrets, REFRESH_THROTTLE_MS).await {
            tracing::warn!(account = id, error = %e, "RefreshApiKey après connexion échoué");
        }
        let st = lock(&self.inner.state);
        st.accounts.iter().find(|r| r.info.id == id).map(|r| r.info.clone()).ok_or(AuthError::UnknownAccount(id))
    }

    /// Insert/update the record from a Haapi account object; clears `needs_login`/`errored`.
    fn apply_account(&self, id: i64, account: &Value, fallback: Option<&Value>) -> Result<(), AuthError> {
        let pick = |k: &str| account.get(k).filter(|v| !v.is_null()).or_else(|| fallback.and_then(|f| f.get(k)));
        let mut st = lock(&self.inner.state);
        let first = st.accounts.is_empty();
        let idx = match st.accounts.iter().position(|r| r.info.id == id) {
            Some(i) => i,
            None => {
                st.accounts.push(AccountRecord {
                    info: AccountInfo {
                        id,
                        login: String::new(),
                        nickname: None,
                        tag: None,
                        security: Vec::new(),
                        is_main: first,
                        active: true,
                        errored: false,
                        needs_login: false,
                        account_type: None,
                        avatar_url: None,
                    },
                    haapi: Value::Null,
                });
                st.accounts.len() - 1
            }
        };
        let rec = &mut st.accounts[idx];
        if let Some(l) = pick("login").and_then(Value::as_str) {
            rec.info.login = l.to_string();
        }
        rec.info.nickname = pick("nickname").and_then(|v| scalar_string(Some(v))).filter(|s| !s.is_empty());
        rec.info.tag = pick("tag").and_then(|v| scalar_string(Some(v))).filter(|s| !s.is_empty());
        rec.info.security = string_list(pick("security"));
        rec.info.account_type = pick("type").and_then(|v| scalar_string(Some(v)));
        rec.info.avatar_url = pick("avatar_url").and_then(Value::as_str).map(str::to_string);
        rec.info.needs_login = false;
        rec.info.errored = false;
        rec.haapi = account.clone();
        self.persist_accounts(&st)
    }

    /// Re-sends the Shield code (SMS when `sms`, e-mail otherwise).
    pub async fn shield_resend(&self, account_id: i64, sms: bool) -> anyhow::Result<()> {
        let key = lock(&self.inner.state).pending.get(&account_id).map(|p| p.key.clone()).ok_or(AuthError::NoPendingShield)?;
        if let CodeSent::Otp = self.inner.haapi.security_code(&key, sms).await?
            && let Some(p) = lock(&self.inner.state).pending.get_mut(&account_id)
        {
            p.otp = true;
        }
        Ok(())
    }

    /// ValidateCode / ValidateOtp with this machine's hm1/hm2, stores the certificate, finishes login.
    pub async fn shield_validate(&self, account_id: i64, code: &str) -> anyhow::Result<AccountInfo> {
        let pending = lock(&self.inner.state).pending.get(&account_id).cloned().ok_or(AuthError::NoPendingShield)?;
        let fp = self.fingerprint();
        let (hm1, hm2) = fp.hm_pair();
        let name = format!("launcher-{}", fp.username);
        let cert = match self.inner.haapi.validate(&pending.key, pending.otp, code.trim(), &hm1, &hm2, &name).await {
            Ok(c) => c,
            Err(e) => {
                if e.reason() == Some("ALREADYSECURED") {
                    lock(&self.inner.state).pending.remove(&account_id);
                }
                return Err(e.into());
            }
        };
        let probe = Secrets { key: String::new(), refresh_token: None, refresh_date: None, certificate: Some(cert.clone()) };
        let (_, enc) = probe.certificate_parts().ok_or_else(|| AuthError::InvalidResponse("certificat Shield incomplet".into()))?;
        shield::certificate_hash_inner(&enc, &hm1, &hm2)?;
        lock(&self.inner.state).pending.remove(&account_id);
        tracing::info!(account = account_id, "certificat Shield obtenu");
        Ok(self.finalize(account_id, pending.key, pending.refresh_token, Some(cert), pending.account).await?)
    }

    /// All stored accounts.
    pub fn accounts(&self) -> Vec<AccountInfo> {
        lock(&self.inner.state).accounts.iter().map(|r| r.info.clone()).collect()
    }

    /// Makes `account_id` the only main account.
    pub fn set_main(&self, account_id: i64) -> anyhow::Result<()> {
        let mut st = lock(&self.inner.state);
        if !st.accounts.iter().any(|r| r.info.id == account_id) {
            return Err(AuthError::UnknownAccount(account_id).into());
        }
        for r in &mut st.accounts {
            r.info.is_main = r.info.id == account_id;
        }
        Ok(self.persist_accounts(&st)?)
    }

    /// Includes/excludes the account from multi-account game launches.
    pub fn set_active(&self, account_id: i64, active: bool) -> anyhow::Result<()> {
        Ok(self.update_info(account_id, |i| i.active = active)?)
    }

    /// DeleteApiKey (best effort) then removes the account and its local secrets.
    pub async fn logout(&self, account_id: i64) -> anyhow::Result<()> {
        let guard = self.account_lock(account_id);
        let _g = guard.lock().await;
        let key = {
            let st = lock(&self.inner.state);
            if !st.accounts.iter().any(|r| r.info.id == account_id) {
                return Err(AuthError::UnknownAccount(account_id).into());
            }
            st.secrets.get(&account_id).map(|s| s.key.clone())
        };
        if let Some(key) = key
            && let Err(e) = self.inner.haapi.delete_key(&key).await
        {
            tracing::warn!(account = account_id, error = %e, "DeleteApiKey échoué (nettoyage local poursuivi)");
        }
        self.inner.store.delete_secrets(account_id)?;
        let mut st = lock(&self.inner.state);
        st.secrets.remove(&account_id);
        st.pending.remove(&account_id);
        let was_main = st.accounts.iter().any(|r| r.info.id == account_id && r.info.is_main);
        st.accounts.retain(|r| r.info.id != account_id);
        if was_main && let Some(r) = st.accounts.first_mut() {
            r.info.is_main = true;
        }
        Ok(self.persist_accounts(&st)?)
    }

    /// RefreshApiKey (48 h throttle) then `Account/CreateToken?game=<game_id>&certificate_id&certificate_hash`.
    pub async fn create_game_token(&self, account_id: i64, game_id: i32) -> anyhow::Result<String> {
        Ok(self.create_game_token_inner(account_id, game_id).await?)
    }

    async fn create_game_token_inner(&self, id: i64, game_id: i32) -> Result<String, AuthError> {
        let guard = self.account_lock(id);
        let _g = guard.lock().await;
        let (secrets, errored) = {
            let st = lock(&self.inner.state);
            let rec = st.accounts.iter().find(|r| r.info.id == id).ok_or(AuthError::UnknownAccount(id))?;
            match st.secrets.get(&id) {
                Some(s) if !rec.info.needs_login => (s.clone(), rec.info.errored),
                _ => return Err(AuthError::SessionExpired),
            }
        };
        let mut secrets = match self.refresh_if_due(id, secrets.clone(), REFRESH_THROTTLE_MS).await {
            Ok(s) => s,
            Err(e) if e.status() == Some(401) => {
                self.update_info(id, |i| i.needs_login = true)?;
                return Err(AuthError::SessionExpired);
            }
            Err(e) if is_transient(&e) => return Err(e),
            Err(e) => {
                tracing::warn!(account = id, error = %e, "RefreshApiKey échoué avant CreateToken");
                secrets
            }
        };
        let mut cert = None;
        if let Some((cert_id, enc)) = secrets.certificate_parts() {
            let (hm1, hm2) = self.fingerprint().hm_pair();
            match shield::certificate_hash_inner(&enc, &hm1, &hm2) {
                Ok(hash) => cert = Some((cert_id, hash)),
                Err(e) => {
                    tracing::warn!(account = id, error = %e, "certificat Shield inutilisable, supprimé");
                    secrets.certificate = None;
                    self.save_secrets(id, &secrets)?;
                }
            }
        }
        let cert_ref = cert.as_ref().map(|(i, h)| (i.as_str(), h.as_str()));
        match self.inner.haapi.create_token(&secrets.key, game_id, cert_ref).await {
            Ok(token) => {
                if errored {
                    self.update_info(id, |i| i.errored = false)?;
                }
                Ok(token)
            }
            Err(e) if e.status() == Some(401) => {
                self.update_info(id, |i| i.needs_login = true)?;
                Err(AuthError::SessionExpired)
            }
            Err(e) if e.status() == Some(403) => {
                self.update_info(id, |i| i.errored = true)?;
                tracing::warn!(account = id, error = %e, "CreateToken refusé (403)");
                Err(AuthError::AccountErrored)
            }
            Err(e) => Err(e),
        }
    }

    /// JSON object for Thrift `userInfo_get` (field names of Zaap `USER_ACCOUNTS` entries).
    pub fn user_info_json(&self, account_id: i64) -> Option<String> {
        let st = lock(&self.inner.state);
        st.accounts.iter().find(|r| r.info.id == account_id).map(|r| zaap_user_info(r).to_string())
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| scalar_string(Some(x))).collect())
        .unwrap_or_default()
}

/// Zaap `parseAccount` + launcher flags (spec 01 section 6.2).
fn zaap_user_info(rec: &AccountRecord) -> Value {
    let a = &rec.haapi;
    let i = &rec.info;
    let field = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
    let nickname_with_tag = match (&i.nickname, &i.tag) {
        (Some(n), Some(t)) => Value::String(format!("{n}#{t}")),
        _ => Value::Null,
    };
    json!({
        "id": i.id,
        "type": i.account_type.clone().map(Value::String).unwrap_or_else(|| field("type")),
        "login": i.login,
        "nickname": i.nickname,
        "firstname": field("firstname"),
        "lastname": field("lastname"),
        "nicknameWithTag": nickname_with_tag,
        "tag": i.tag,
        "security": i.security,
        "addedDate": field("added_date"),
        "locked": field("locked"),
        "parentEmailStatus": field("parent_email_status"),
        "avatar": i.avatar_url,
        "isGuest": false,
        "isMain": i.is_main,
        "active": i.active,
        "isErrored": i.errored,
        "needRefresh": i.needs_login,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> (tempfile::TempDir, AuthManager) {
        let dir = tempfile::tempdir().unwrap();
        let m = AuthManager::load_from(dir.path().to_path_buf()).unwrap();
        (dir, m)
    }

    fn signon_account(id: i64, nick: &str) -> Value {
        json!({"id": id, "type": "ANKAMA", "login": format!("{nick}@example.com"), "nickname": nick,
               "tag": 1234, "security": ["SHIELD"], "added_date": "2020-01-01T00:00:00+01:00",
               "locked": "0", "firstname": null, "lastname": null, "avatar_url": "https://a/x.png"})
    }

    #[test]
    fn multi_account_flags_persist_and_user_info() {
        let (dir, m) = manager();
        m.apply_account(1, &signon_account(1, "Alpha"), None).unwrap();
        m.apply_account(9_007_199_254_740_993, &signon_account(9_007_199_254_740_993, "Beta"), None).unwrap();
        m.save_secrets(1, &Secrets { key: "k".into(), refresh_token: None, refresh_date: None, certificate: None }).unwrap();
        let accs = m.accounts();
        assert_eq!(accs.len(), 2);
        assert!(accs[0].is_main && !accs[1].is_main && accs.iter().all(|a| a.active));

        m.set_main(9_007_199_254_740_993).unwrap();
        m.set_active(1, false).unwrap();
        assert!(m.set_main(5).is_err());

        let reloaded = AuthManager::load_from(dir.path().to_path_buf()).unwrap();
        let accs = reloaded.accounts();
        assert!(!accs[0].is_main && accs[1].is_main && !accs[0].active);
        assert!(!accs[0].needs_login, "account 1 has secrets");
        assert!(accs[1].needs_login, "account without secrets needs login");

        let info: Value = serde_json::from_str(&reloaded.user_info_json(1).unwrap()).unwrap();
        assert_eq!(info["id"], 1);
        assert_eq!(info["nicknameWithTag"], "Alpha#1234");
        assert_eq!(info["tag"], "1234");
        assert_eq!(info["isMain"], false);
        assert_eq!(info["active"], false);
        assert_eq!(info["type"], "ANKAMA");
        assert_eq!(info["security"][0], "SHIELD");
        let big: Value = serde_json::from_str(&reloaded.user_info_json(9_007_199_254_740_993).unwrap()).unwrap();
        assert_eq!(big["id"].as_i64(), Some(9_007_199_254_740_993));
        assert!(reloaded.user_info_json(42).is_none());
    }

    #[tokio::test]
    async fn token_requires_session_and_shield_requires_pending() {
        let (_dir, m) = manager();
        let e = m.create_game_token(7, 1).await.unwrap_err();
        assert!(matches!(e.downcast_ref::<AuthError>(), Some(AuthError::UnknownAccount(7))));
        m.apply_account(7, &signon_account(7, "G"), None).unwrap();
        let e = m.create_game_token(7, 1).await.unwrap_err();
        assert_eq!(e.to_string(), "Session expirée, reconnectez-vous");
        let e = m.shield_validate(7, "123456").await.unwrap_err();
        assert!(matches!(e.downcast_ref::<AuthError>(), Some(AuthError::NoPendingShield)));
    }

    #[tokio::test]
    async fn cancel_without_login_is_noop() {
        let (_dir, m) = manager();
        m.cancel_login().await;
    }

    #[test]
    fn futures_are_send() {
        fn send<T: Send>(_: &T) {}
        let (_dir, m) = manager();
        send(&m.login_with_browser(|_| {}));
        send(&m.create_game_token(1, 1));
        send(&m.restore_sessions());
        send(&m.shield_validate(1, "x"));
        send(&m.logout(1));
        fn sync_send<T: Send + Sync + Clone>() {}
        sync_send::<AuthManager>();
    }

    #[test]
    fn outcome_serialization() {
        let o = LoginOutcome::ShieldRequired { account_id: 3, domain: Some("g***.com".into()), otp: false };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(v, json!({"type": "shieldRequired", "accountId": 3, "domain": "g***.com", "otp": false}));
    }
}
