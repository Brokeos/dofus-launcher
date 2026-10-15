# 05 — Implementation plan & module contracts

Goal: a lightweight, robust Dofus-only launcher replacing Ankama Launcher (Zaap 3.15.5).
Stack: Tauri 2 (Rust backend, Svelte 5 + TS frontend). Workspace: `core/` (all logic, no Tauri dep,
unit-testable) and `app/` (Tauri shell + UI). Specs: `01-auth-haapi.md`, `02-game-launch-and-zaap-protocol.md`,
`03-cytrus-updates.md`, `04-architecture-settings-perf.md`.

## Verified facts that drive the design (2026-10-15)

- OAuth redirect `http://127.0.0.1:9001/authorized` is **accepted** by `auth.ankama.com` for `client_id=102`
  (browser test: login form shown; a random redirect gives 403). → Use a loopback listener; never touch the
  `zaap://` protocol handler of the official launcher.
- Dofus 3 = cytrus game `dofus`, release `dofus3` (version `6.0_3.7.10.10` today); `main` is still Dofus 2.x.
- Dofus 3 `zaap.yml` (sample `re/samples/bundle-dofus3-config/`): no `executable` → `zaap-start` mode
  (`zaap-start.bat` runs `Dofus.exe -logFile "%ZAAP_LOGS_PATH%\dofus.%ACCOUNT_ID%.log" %*`).
  Arguments: `--port {ZAAP_PORT} --gameName {ZAAP_GAME} --gameRelease {ZAAP_RELEASE} --instanceId {n} --hash {uuid}
  --canLogin {ZAAP_CAN_AUTH} --langCode {lang} --autoConnectType {0|1|2} --connectionPort {5555|443}
  [--hdReady] [--4kReady] "{directxVersion: '' | '-force-d3d11'}"`. `maxInstances: true`. Release meta
  `multiAccount: true`.
- News `GET haapi/json/Ankama/v5/Cms/Items/Get?template_key=NEWS&site=DOFUS&lang=fr&page=1&count=N` and
  `https://status.cdn.ankama.com/export.json` are public (no APIKEY).

## Robustness rules (fixing the official launcher's problems)

1. Atomic state writes (`paths::write_atomic`); never truncate+rewrite in place.
2. Files are assembled in `<file>.dl-tmp`, SHA-1 verified, then renamed over the target.
3. Interrupted update ≠ dirty. Resume from verified state; full re-hash only on explicit "Vérifier".
4. Exclusive lock file `<install>/.dofus-launcher.lock` held during any write operation.
5. Refuse to update while any `Dofus.exe` whose image path is inside the install dir is running (process snapshot).
6. Space check = bytes to download + largest file (temp overhead), not full install size.
7. No self-update loop, no telemetry, no chat/shop/video. Server status/news fetched on demand (≥ 5 min cache).
8. Never log secrets (api keys, refresh tokens, game tokens, hash, certificates).

## Data layout (ours)

`%APPDATA%\dofus-launcher\`
- `settings.json` — launcher settings (plain JSON).
- `accounts.json` — non-secret account metadata list.
- `secrets\<accountId>.bin` — DPAPI-encrypted (CurrentUser) JSON `{key, refreshToken, refreshDate, certificate?}`.
- `logs\launcher.log`, `logs\gamesLogs\dofus-dofus3\` (= `ZAAP_LOGS_PATH`).
- `cache\` — cached manifests keyed by version.

Install dir (default `%LOCALAPPDATA%\Ankama\Dofus-dofus3`) keeps Zaap-compatible files so the official launcher
can adopt it: `manifest.json` (Zaap format, written atomically), `.release.infos.json`
`{"gameUid":"dofus","release":"dofus3"}`, `migration.launcher.json` `{"launcher":1}`.

## Module contracts (core crate `dofus_core`)

Error type per module: `thiserror` enums; public fns return `anyhow::Result<T>` unless stated.

### `auth` (owner: auth agent)
```rust
pub struct AccountInfo { pub id: i64, pub login: String, pub nickname: Option<String>, pub tag: Option<String>,
                         pub security: Vec<String>, pub is_main: bool, pub active: bool, pub errored: bool } // Serialize/Deserialize/Clone
pub enum LoginOutcome { LoggedIn(AccountInfo), ShieldRequired { account_id: i64, domain: Option<String>, otp: bool } }
pub struct AuthManager; // Clone (Arc inside), thread-safe
impl AuthManager {
    pub fn load() -> anyhow::Result<Self>;                       // reads accounts.json + secrets
    pub async fn restore_sessions(&self) -> anyhow::Result<()>;  // SignOn + throttled refresh for stored accounts
    /// Full OAuth PKCE flow: binds 127.0.0.1:9001, opens browser via `open_url` callback, waits (timeout 10 min).
    pub async fn login_with_browser(&self, open_url: impl Fn(&str) + Send) -> anyhow::Result<LoginOutcome>;
    pub async fn cancel_login(&self);
    pub async fn shield_resend(&self, account_id: i64, sms: bool) -> anyhow::Result<()>;
    pub async fn shield_validate(&self, account_id: i64, code: &str) -> anyhow::Result<AccountInfo>;
    pub fn accounts(&self) -> Vec<AccountInfo>;
    pub fn set_main(&self, account_id: i64) -> anyhow::Result<()>;
    pub fn set_active(&self, account_id: i64, active: bool) -> anyhow::Result<()>;
    pub async fn logout(&self, account_id: i64) -> anyhow::Result<()>; // DeleteApiKey + delete local secrets
    /// RefreshApiKey (48h throttle) then Account/CreateToken?game=<game_id>&certificate_id&certificate_hash.
    pub async fn create_game_token(&self, account_id: i64, game_id: i32) -> anyhow::Result<String>;
    /// JSON object for Thrift `userInfo_get` (same field names as Zaap USER_ACCOUNTS entry).
    pub fn user_info_json(&self, account_id: i64) -> Option<String>;
}
pub mod shield { pub fn hm_pair() -> (String, String); pub fn certificate_hash(enc_b64: &str, hm1: &str, hm2: &str) -> anyhow::Result<String>; }
```

### `cytrus` (owner: cytrus agent)
```rust
pub struct RemoteRelease { pub game: String, pub release: String, pub version: String } // version like "6.0_3.7.10.10"
pub async fn resolve(release: &str) -> anyhow::Result<RemoteRelease>;           // cytrus.json (require version 6)
pub async fn fetch_release_info(release: &str) -> anyhow::Result<serde_json::Value>; // assets release.json (serverBlocked, updateBlocked…)
pub mod manifest { pub struct Manifest { pub fragments: Vec<Fragment> } /* Fragment/File/Bundle/Chunk, hashes as [u8;20] */
                   pub fn parse(bytes: &[u8]) -> anyhow::Result<Manifest>; }
pub struct LocalState { pub installed_version: Option<String>, pub installed_fragments: Vec<String> } // read from install dir
pub fn local_state(install_dir: &Path) -> anyhow::Result<LocalState>;
pub enum Phase { Preparing, Checking, Downloading, Finalizing, Done }
pub struct Progress { pub phase: Phase, pub bytes_done: u64, pub bytes_total: u64, pub files_done: u64,
                      pub files_total: u64, pub speed_bps: u64 } // Serialize, Clone
pub struct UpdateOptions { pub release: String, pub install_dir: PathBuf, pub fragments: Vec<String>,
                           pub verify_all: bool, pub max_concurrency: usize, pub speed_limit_bps: Option<u64> }
/// Install or update (or repair when verify_all). Cancellable via token; progress via callback (≤ 4 Hz).
pub async fn run_update(opts: UpdateOptions, cancel: tokio_util::sync::CancellationToken /* or Arc<AtomicBool> */,
                        on_progress: impl Fn(Progress) + Send + Sync + 'static) -> anyhow::Result<()>;
pub async fn fetch_configuration(release: &str, version: &str, dest: &Path) -> anyhow::Result<()>; // configuration fragment only
```

### `game` (owner: game/zaap agent)
```rust
pub struct GameSettings { /* language, autoConnectType, connectionPort, hdReady, 4kReady, directxVersion — as JSON map */ }
pub fn render_zaap_config(install_dir: &Path, ctx: &RenderContext) -> anyhow::Result<ZaapConfig>; // minijinja; extends/block/if
pub struct ZaapConfig { pub executable: Option<String>, pub arguments: Vec<String>, pub env: BTreeMap<String,String>,
                        pub fragments: Vec<String> /* deduped, configuration first */, pub settings: Vec<SettingDef>, pub max_instances: Option<u32> }
pub fn running_game_pids(install_dir: &Path) -> Vec<u32>;            // any Dofus.exe under install dir
pub struct LaunchRequest { pub install_dir: PathBuf, pub release: String, pub account_id: Option<i64>, pub language: String }
pub struct GameSessions; // registry hash -> {instance_id, account_id, pid}
pub fn launch(req: &LaunchRequest, sessions: &GameSessions, settings: &GameSettings) -> anyhow::Result<u32 /* pid */>;
```

### `zaap` (owner: game/zaap agent)
```rust
pub trait Backend: Send + Sync + 'static {   // implemented by the app with AuthManager + settings
    fn create_game_token(&self, account_id: i64, game_id: i32) -> BoxFuture<'static, anyhow::Result<String>>;
    fn user_info_json(&self, account_id: i64) -> Option<String>;
    fn game_setting_get(&self, key: &str) -> Option<String>;      // JSON-encoded value
    fn game_setting_set(&self, key: &str, json_value: &str) -> anyhow::Result<()>;
    fn is_update_available(&self) -> bool;
    fn active_account_ids(&self) -> Vec<i64>;                      // for auth_getGameTokenWithWindowId
}
pub async fn serve(sessions: Arc<GameSessions>, backend: Arc<dyn Backend>, port: u16 /* 26116 */) -> anyhow::Result<()>;
```
Thrift: hand-written TBinaryProtocol (strict) over unframed buffered TCP; dispatch by method name; all 20 methods
(IDL in spec 02); pay*/notification methods return `ZaapError{OVERLAY_UNEXPECTED_ERROR=6003}` /
`SYSTEM_NOTIFICATION_UNEXPECTED_ERROR=7002`. Port busy → clear error "Ankama Launcher déjà ouvert".

### `extras`
`news(lang, count) -> Vec<NewsItem{id,title,date,url,image}>`, `server_status() -> Vec<Server{name,status,tags}>`
(filter tags for Dofus 3 servers).

## Tauri commands (app ↔ UI contract)

| Command | Args | Returns |
|---|---|---|
| `get_state` | – | `{ accounts: AccountInfo[], settings, release: {remoteVersion, installedVersion, installDir, installed, updateAvailable, serverBlocked, updateBlocked}, task: TaskState \| null, runningInstances: number }` |
| `login_start` | – | `LoginOutcome` (opens browser) |
| `login_cancel` | – | – |
| `shield_validate` | `{accountId, code}` | `AccountInfo` |
| `shield_resend` | `{accountId, sms}` | – |
| `account_set_main` / `account_set_active` / `account_logout` | `{accountId[, active]}` | – |
| `game_install` | `{installDir?}` | – (starts task) |
| `game_update` / `game_verify` / `task_cancel` | – | – |
| `game_play` | `{accountIds: number[]}` (one instance per account) | `number[]` pids |
| `settings_get` / `settings_set` | `{settings}` | settings |
| `game_settings_get` / `game_settings_set` | – / `{key, value}` | settings defs + values |
| `news_get` / `server_status_get` | `{lang}` | lists |
| `open_logs_dir` / `open_install_dir` | – | – |

Events: `task-progress` (Progress), `task-finished` `{ok, error?}`, `state-changed`, `game-exited` `{pid}`.

## Build order

1. Parallel: auth, cytrus, game+zaap (each with unit tests; cytrus integration test downloads only the
   `configuration` + `start-script` fragments into a temp dir).
2. App wiring (Tauri commands, state, tray-less single window), UI.
3. End-to-end: real Dofus 3 install into default dir, launch, verify the game reaches login via Thrift.
