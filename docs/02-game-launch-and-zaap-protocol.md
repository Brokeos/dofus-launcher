# 02 - Game launch and the launcher <-> game ("Zaap service") protocol

Target: Ankama Launcher ("zaap") 3.15.5, Windows. Source: `re/pretty/main.js` (prettified webpack main
bundle). All `L<n>` references are line numbers in that file.

Evidence levels used below:

- **[OBS]**: observed directly in code, with a line citation.
- **[INF]**: inferred from code structure or semantics (reasoning given). Not verified against a running game.
- **[UNK]**: unknown from static analysis of the launcher alone. Needs another source, usually the game's own files.

Nothing here was obtained by running the launcher or the game, or by making network requests.

---

## 0. TL;DR for a replacement launcher

1. Generate a session: `instanceId` (an incrementing integer starting at 1) and `hash` (a random UUIDv4).
2. Spawn the game from its install dir, with `cwd = installDir`. Use the executable and arguments from the game's
   `<installDir>/zaap.yml` (a nunjucks template rendered and then parsed as YAML). Set these env vars:
   `ZAAP_PORT=26116`, `ZAAP_GAME=<gameUid>`, `ZAAP_RELEASE=<release>`, `ZAAP_INSTANCE_ID=<n>`, `ZAAP_HASH=<uuid>`,
   `ZAAP_CAN_AUTH=true`, `ZAAP_LOGS_PATH=<dir>`, `ACCOUNT_ID=<accountId>`.
3. Listen on `127.0.0.1:26116` with an Apache Thrift server: plain TCP, **TBufferedTransport (unframed)** and
   **TBinaryProtocol**. Implement `ZaapService` (IDL in section 3).
4. The game calls `connect(gameName, releaseName, instanceId, hash)`. Reply with `hash`, which becomes the
   `gameSession` string for every later call.
5. The game calls `auth_getGameToken(gameSession, gameId)`. Reply with the `token` field returned by
   `GET https://haapi.ankama.com/json/Ankama/v5/Account/CreateToken?game=<gameId>[&certificate_id=..&certificate_hash=..]`,
   sent with header `APIKEY: <account api key>`.
6. Optionally, also listen on `127.0.0.1:26117`. This is a text-line variant of the same RPCs (section 2.6).

---

## 1. Game process spawn

### 1.1 Entry points [OBS]

- IPC `release.start` (L320, registered L24518) calls `releaseIpc.start` at L24657, which calls
  `release.startSeries(count)` (L24692).
  `startSeries` calls `release.start()` `count` times, one after another. Each call waits for the `series-launched`
  event (L34690-34703).
- IPC "start with account" calls `startWithAccount` at L24666, which calls `release.start(null, accountId)`.
- A restart after exit calls `release.start(previousPid)` (L34796). This reuses the old credentials (section 1.5).

### 1.2 `Release.prototype.start(pid = null, accountId = null)` (L34700-34816) [OBS]

Steps, in order:

1. `canStart()` (L34818-34830) throws in these cases: not installed, update running, **update available**, max
   instances reached, the game is being moved, or no teasing access.
   - `maxInstances` comes from `zaap.yml`. The default is `1` (L52449), and `true` means unlimited (L34614-34618).
2. `getLogsPath()` (L34620-34654):
   - If `zaap.yml` has `hasLogButton: true` (the default), the path is `<launcher logdir>/gamesLogs/<folderName>`.
     On darwin it is `<logdir>/<folderName>` instead.
   - `folderName` is the `gameUid`, or `gameUid-release` when the release is not `main` (L34655-34662).
     The folder is created.
   - Otherwise it returns `false`, so the env var becomes the string `"false"`.
3. `checkConfiguration(location, config)` (L100712-100737): if `zaap.yml` defines `checkConfiguration.script`, it
   runs `<script>.bat` (`.sh` on other OSes) in the install dir. A non-zero exit code aborts the launch. The error
   message is looked up in `checkConfiguration.results[code].message[lang]`.
4. `service.getEnvironmentForRelease(this, pid, accountId)` (L98522-98570) builds the session and env (section 1.4).
5. Pre-launch account checks: is the account errored or expired, user information (guest, full name, data-collect
   consent; L34843+), and cloud-save conflicts. If any check fails, the credentials are invalidated and the launch
   is aborted.
6. KPI event, then `configuration.compile(envInfo)` (L34738). This re-renders `zaap.yml` with `env` available
   (section 1.3).
7. Spawn (L34740-34762):
   - **Normal case**: `config.executable !== "zaap-start"`.
     ```js
     child_process.spawn(config.executable, config.arguments, {
       detached: true,
       stdio: "ignore",
       cwd: release.location,                       // game install dir
       env: { ...process.env, ...envInfo.env, ...config.env },  // zaap.yml `env` overrides ZAAP_*
     })
     ```
     Before spawning, it checks that `join(location, executable)` exists. If not, it throws `EXECUTABLE_NOT_FOUND`.
     The executable path is relative to the install dir.
   - **Script case**: `executable === "zaap-start"`. This is the default when `zaap.yml` has no `executable`
     (L788, L52446). It calls `scriptSpawner.spawn(location, "zaap-start", args, envInfo.env)` (L20924-20945),
     which runs `zaap-start.bat` on win32 (`./zaap-start.sh` elsewhere) with `cwd=location` and
     `env={...process.env, ...ZAAP env}`. This path is not detached and does not merge `config.env`.
8. After the spawn (L34763-34815):
   - `updateCredentials(..., id, child.pid)` stores the pid on the session.
   - The child is pushed to `runningInstances`.
   - `isLaunching=true`. It becomes `false` when the game calls `connect`, or after 1000 ms
     (`DEFAULT_LAUNCHING_WAIT_IN_MS`, L34199) unless `waitLaunchDuringStartSeries` is set.

### 1.3 Executable path and command-line arguments: the `zaap.yml` template

These are **not hardcoded in the launcher**. They come from the game's own `zaap.yml`, which ships with the game
(it is part of the downloaded "configuration" fragment).

- Path: `join(release.location, "zaap.yml")`, read in `configuration.load` (L52510-52522).
  `configuration.get(release, location)` is called from `loadConfiguration` (L34435-34441).
- Processing (L52564-52640) [OBS]:
  1. Render with nunjucks (`new Template(str, new Environment(new FileSystemLoader(location)))`).
  2. Parse the output with `js-yaml.load`.
  3. Repeat steps 1-2 until the output is stable, at most 5 iterations.
  4. Merge the result over the defaults at L52445-52458:
     ```yaml
     executable: zaap-start
     arguments: []
     env: {}
     fragments: [main, configuration]
     maxInstances: 1
     hooks: {}
     settings: []
     checkConfiguration: false
     licensesFolder: false
     waitLaunchDuringStartSeries: false
     saveFolder: null
     hasLogButton: true
     ```
- Template context, from `getConfigurationContext` (L52524-52545) [OBS]:
  ```js
  {
    os:  { platform: "windows"|"darwin"|"linux", arch, release, cores, homedir, tmpdir, ram },
    zaap: AppSettings.get(),          // all launcher settings (language etc.)
    env:  envInfo.env,                // ZAAP_* vars of the instance being launched (only at launch-time compile)
    game: { location, settings: gameSettings.get() }   // per-release settings (settings.json)
  }
  ```
- **Dofus 3 executable name and arguments: [UNK]** from launcher code. No game is installed on this machine (there is
  no `%APPDATA%/zaap` and no install under `%LOCALAPPDATA%/Ankama`).
  - [INF] The Unity build is very likely `Dofus.exe` in the install dir.
  - [INF] Community reports say the arguments include `--port=`, `--gameName=`, `--gameRelease=`, `--instanceId=`,
    `--hash=`, `--canLogin=` and language options. They would be rendered from `{{ env.ZAAP_* }}` and
    `{{ zaap.* }}` variables. This is not verified.
  - **Action**: install the game through any means and read `<install>/zaap.yml`. Static file read is enough. The
    replacement launcher should embed a small nunjucks-compatible renderer (or hand-render the few variables used),
    rather than hardcode the arguments.
- Default install path, from `getSuggestedInstallPath` (L34667-34670): `<app.getGamesPath()>/<FolderName>`. Here
  `FolderName` has a capitalised gameUid, for example `Dofus` or `Dofus-beta`. The Ankama base dir is
  `%LOCALAPPDATA%\Ankama` on win32 (L1879-1881). [INF] `getGamesPath()` resolves under it.

### 1.4 Environment variables (L98555-98567) [OBS]

| Var | Value | Notes |
|---|---|---|
| `ZAAP_PORT` | `getLocalPort()`, which is `buildConfig.service.port` or **26116** (L1987-1990) | Production buildConfig has no `service` key (L72285+), so the value is 26116. |
| `ZAAP_GAME` | `release.gameUid` | The key in the cytrus registry, for example `dofus` (section 4). |
| `ZAAP_RELEASE` | `release.name` | `main`, `beta`, ... |
| `ZAAP_INSTANCE_ID` | module-level counter `n += 1` (L98517, L98535-98536) | Starts at 1. Global across **all** games for the launcher's lifetime. Never reused. |
| `ZAAP_HASH` | `uuidv4()` (L98537; `uuidv4` = `n(558).v4`, L1982) | Also the session key and the `gameSession` string. Treat as a secret. |
| `ZAAP_CAN_AUTH` | `!!accountId`, which becomes the string `"true"` or `"false"` | [INF] Tells the game whether it may call `auth_getGameToken`. |
| `ZAAP_LOGS_PATH` | logs dir, or `false` | The string `"false"` when `hasLogButton` is false. |
| `ACCOUNT_ID` | the selected Ankama accountId | [INF] Node omits `undefined` env values, so the var is absent when there is no account. |
| `...config.env` | the `env:` map from `zaap.yml` | Merged last, so it overrides the vars above. Not merged in the zaap-start path. |

The full parent `process.env` is inherited.

### 1.5 Session registry, multi-instance and multi-account (L98512-98685) [OBS]

`service.instances` is a map keyed by `hash`:
`{ id: instanceId, sessionKey: hash, release, accountId, pid?, gameSession? }`.

The account is chosen in `getEnvironmentForRelease` when no `accountId` is passed (L98541-98553):

- If `release.information.default.multiAccount` is true (from the cytrus release metadata):
  `accountId = Auth.getNextFreeAccountId(connectedIds)`. This picks the first logged-in api-key account that is not
  already running this gameUid+release (L2807-2830).
- Otherwise it uses the main account, but only if the main account is not already connected to this game+release.
  If it is, `accountId` stays undefined, so `ZAAP_CAN_AUTH=false`.

A restart via `start(pid)` (L98527-98533) looks the session up by pid and reuses the same `id`, `hash`, `accountId`
and release.

On child exit (L34768-34803):

1. Close the overlay for that pid.
2. `endGameSessionByPid`: if a premium `gameSession` exists, call Haapi `ankama.premiumGame.disconnect`.
3. `invalidateCredentials`, unless a restart was requested.
4. Remove the child from `runningInstances` and upload the cloud save.
5. Restart if `release_restartOnExit` was called. Otherwise run any pending update-on-exit.

Killing processes [OBS]:

- `stop(index, signal="SIGTERM")`, `stopInstance(pid)` and `stopAll()` call `child.kill()` (L34671-34688). On
  Windows this terminates the process.
- `auth_getGameToken*` failing with HTTP 401/403 calls `release.stop()`, which kills `runningInstances[0]`
  (L65568-65572, L65611-65615).
- The overlay "close game" action calls `stopInstance(pid)` (L7760-7762).

The launcher refuses to quit or self-update while any socket is connected to the service (`hasConnectedProcesses`,
L1889, L8815, L8841).

### 1.6 Development mode (internal builds only) [OBS]

When `buildConfig.internal` is set (it is false in production), the launcher watches `~/zaap-development.json`. It
registers the releases listed there and writes `<location>/credentials.json` containing
`{port, name, release, instanceId, hash}` (L98758-98830). [INF] This suggests games can also read their
credentials from that file instead of env vars.

---

## 2. Local Thrift server

### 2.1 Startup [OBS]

- `service.start` (L1993-2013) calls `thrift.createServer(ZaapService, handlers)`, then
  `listen(26116, "127.0.0.1")`.
- It starts only **after login**: `delayedSetupsAfterLogin` at L42092-42103 calls `service.start` and
  `textSocketStart`.
- The thrift lib is `n(1158)` (export block L129483-129527). `createMultiplexServer` (L130387-130428) uses plain
  `net.createServer` (TLS only if `options.tls`). With no options, the defaults are:
  - transport = `n(81)` = **TBufferedTransport**
  - protocol = `n(94)` = **TBinaryProtocol**

  (L129521-129524). An older bundled thrift copy (L93570-93617) has the same defaults.
- So the wire format is **raw TCP + unframed buffered + TBinaryProtocol**: a strict message header
  (`0x80010000 | type`), then the method name as a string, then the seqid.
  - [INF] Node thrift's reader also accepts non-strict headers.
  - The processor dispatches on the **method name**: `this["process_" + fname]` (L97066-97079). There are no method
    numbers in Thrift. An unknown method returns `TApplicationException(UNKNOWN_METHOD)`.
- Port: fixed 26116, with no fallback. Override only via `buildConfig.service.port`. If two launchers run, the second
  fails with EADDRINUSE, and the error is only logged (L2008).

### 2.2 Handler binding [OBS]

- `thriftDefinedApis` are the names `x` where `ZaapService.Client.prototype` has both `send_x` and `recv_x`
  (L1983-1985).
- Each name is loaded from the webpack context `./<name>.js` (map at L130876-130926, implementations at
  L65380-66208). The handler's `execute(...)` arity is checked against the client method (L2061-2093).
- RPC files that exist but are **not exposed**, because they are not in the IDL: `endGameSession`, `payArticle2`,
  `payArticleWithPid2`.
- Each wrapped handler is callback-style. Errors are encoded as follows (for example L97117-97140):
  - A `ZaapError` becomes a REPLY with the `error` (field 1) set.
  - Any other thrown or returned error (a string, or a Haapi HTTP error) becomes a
    `TApplicationException(UNKNOWN, err.message)` with message type EXCEPTION.

### 2.3 Reconstructed IDL [OBS]

Struct, field and type data come from service_types (L48530-49090) and from the `ZaapService_*_args/_result`
structs (L93950-96040). Fields with negative ids in the generated code (payArticle etc.) had **no explicit id** in
the original IDL. They are written without ids below, and Thrift auto-assigns -1, -2, and so on.

```thrift
// namespace/file name unknown; service name confirmed: "ZaapService" (struct prefixes L94004+)

enum ErrorCode {
  UNKNOWN = 1, UNAUTHORIZED = 2, INVALID_GAME_SESSION = 3,
  CONNECTION_FAILED = 1001, INVALID_CREDENTIALS = 1002,
  AUTH_NOT_LOGGED_IN = 2001, AUTH_BAN = 2002, AUTH_BLACKLIST = 2003, AUTH_LOCKED = 2004,
  AUTH_DELETED = 2005, AUTH_RESETANKAMA = 2006, AUTH_OTPTIMEFAILED = 2007, AUTH_SECURITYCARD = 2008,
  AUTH_BRUTEFORCE = 2009, AUTH_FAILED = 2010, AUTH_PARTNER = 2011, AUTH_MAILNOVALID = 2012,
  AUTH_BETACLOSED = 2013, AUTH_NOACCOUNT = 2014, AUTH_ACCOUNT_LINKED = 2015, AUTH_ACCOUNT_INVALID = 2016,
  AUTH_ACCOUNT_SHIELDED = 2017, AUTH_WRONG_WINDOW_ID = 2018, AUTH_MULTI_NOT_ACTIVATED = 2019,
  AUTH_NO_MORE_ACCOUNT_AVAILABLE = 2020,
  UPDATER_CODE_RANGE = 3001,
  SETTINGS_KEY_NOT_FOUND = 4001, SETTINGS_INVALID_VALUE = 4002,
  USER_INFO_UNAVAILABLE = 5001,
  OVERLAY_CLOSED = 6001, OVERLAY_WRONG_PARAMETERS = 6002, OVERLAY_UNEXPECTED_ERROR = 6003,
  OVERLAY_PERMISSION_DENIED = 6004,
  SYSTEM_NOTIFICATION_WRONG_PARAMETERS = 7001, SYSTEM_NOTIFICATION_UNEXPECTED_ERROR = 7002,
  GUEST_COMPLETION_ERROR = 8001, USER_NOT_GUEST = 8002
}                                                                  // L48535-48573

enum NotificationResultType { CLICK = 1, CLOSE = 2, FADE = 3, REPLY = 4, ACTION = 5 }  // L48574-48580

exception ZaapError {                                              // L48581-48637
  1: required i32 code,       // ErrorCode value
  2: optional string details
}

struct OverlayPosition {                                           // L48639-48702
  1: i32 posX = 25,
  2: i32 posY = 25,
  3: i32 width = 50,
  4: i32 height = 50
}

struct NotificationOptions {                                       // L48704-48897
  1: string title, 2: string subtitle, 3: string body, 4: bool silent, 5: string icon,
  6: bool hasReply, 7: string timeoutType, 8: string replyPlaceholder, 9: string sound,
  10: string urgency, 11: list<string> actions, 12: string closeButtonText, 13: string toastXml
}

struct NotificationParams {                                        // L48899-49004
  1: string id, 2: string body, 3: list<string> image, 4: string subTitle,
  5: string background, 6: i32 timeout
}

struct NotificationResult {                                        // L49006-49075
  1: string id, 2: i32 type /* NotificationResultType */, 3: string reply, 4: string actionIndex
}

service ZaapService {
  string connect(1: string gameName, 2: string releaseName, 3: i32 instanceId, 4: string hash)
      throws (1: ZaapError error),
  string auth_getGameToken(1: string gameSession, 2: i32 gameId) throws (1: ZaapError error),
  bool   updater_isUpdateAvailable(1: string gameSession) throws (1: ZaapError error),
  string settings_get(1: string gameSession, 2: string key) throws (1: ZaapError error),
  void   settings_set(1: string gameSession, 2: string key, 3: string value) throws (1: ZaapError error),
  string askForGuestComplete(1: string gameSession) throws (1: ZaapError error),
  string userInfo_get(1: string gameSession) throws (1: ZaapError error),
  void   release_restartOnExit(1: string gameSession) throws (1: ZaapError error),
  void   release_exitAndRepair(1: string gameSession, 2: bool restartAfterRepair) throws (1: ZaapError error),
  string zaapVersion_get(1: string gameSession) throws (1: ZaapError error),
  bool   zaapMustUpdate_get(1: string gameSession) throws (1: ZaapError error),
  bool   payArticle(1: string gameSession, string shopApiKey, i32 articleId, OverlayPosition pos)
      throws (1: ZaapError error),
  bool   payArticleWithPid(1: string gameSession, string shopApiKey, i32 articleId, i32 pid, OverlayPosition pos)
      throws (1: ZaapError error),
  bool   payCart(1: string gameSession, string accessToken, string shopKey, string cartId, OverlayPosition pos)
      throws (1: ZaapError error),
  bool   payCartWithPid(1: string gameSession, string accessToken, string shopKey, string cartId, i32 pid,
                        OverlayPosition pos) throws (1: ZaapError error),
  bool   hasPremiumAccess(1: string gameSession),                  // result has no error field
  bool   hasGameAccess(1: string gameSession, 2: i32 gameId),      // result has no error field
  string auth_getGameTokenWithWindowId(1: string gameSession, 2: i32 gameId, 3: i32 windowId)
      throws (1: ZaapError error),
  string system_addNotification(1: string gameSession, 2: NotificationOptions notificationOptions)
      throws (1: ZaapError error),
  NotificationResult addNotification(1: string gameSession, 2: NotificationParams notificationParams)
      throws (1: ZaapError error)
}
```

On the wire, a result is a struct with field `0 = success` and field `1 = error`. The processor order is
L97080-98440.

### 2.4 Method semantics: what the launcher answers [OBS unless marked]

`authorize(gameSession)` (L98608-98621) looks up `instances[gameSession].release`. It throws
`ZaapError{INVALID_GAME_SESSION}` if the session is unknown, and `UNAUTHORIZED` if the session has no release.

| Method | Implementation | Answer |
|---|---|---|
| `connect` (L65622-65640) | `getCredentials(gameName, releaseName, instanceId, hash, processPid)` returns `instances[hash]`. **Only `hash` is checked.** The other args are ignored (L98593-98596). Over Thrift `processPid` is `false`. | Sets `release.isLaunching = false` and returns `sessionKey` (the same string as `hash`). This string is the `gameSession` for all other calls. Unknown hash: `ZaapError{INVALID_CREDENTIALS=1002}`. |
| `auth_getGameToken(gs, gameId)` (L65553-65576) | `accountId = instances[gs].accountId`, then `Auth.createToken(gameId, accountId)` (L2742-2799). | Returns the token string (see below). On HTTP 401 it logs that account out. On 403 it marks the account errored and notifies the UI. On 401/403 it also stops `runningInstances[0]` of the release. A missing account rejects with a plain string, which reaches the game as a TApplicationException. |
| `auth_getGameTokenWithWindowId(gs, gameId, windowId)` (L65580-65619) | `windowId` must parse as non-zero. If the release is not multi-account and `windowId > 1`, it returns `AUTH_MULTI_NOT_ACTIVATED`. `accountId = getNthAccountId(windowId)`, which is the `windowId`-th (1-based) `active` entry in `USER_ACCOUNTS` (L2801-2806). If there is none, it returns `AUTH_NO_MORE_ACCOUNT_AVAILABLE`. | Returns the token from `createToken(gameId, accountId)`. |
| `updater_isUpdateAvailable` (L66171-66180) | `release.isUpdateAvailable()` (L34593-34606) | bool |
| `settings_get(gs, key)` (L66032-66053) | `release.gameSettings.get()[key]`. These settings come from `zaap.yml` `settings:` plus `autoUpgrade`, stored in `<repo>/<gameUid>/<release>/settings.json` (L35576, L35548). | `JSON.stringify(value)`. Unknown key: `SETTINGS_KEY_NOT_FOUND`. |
| `settings_set(gs, key, value)` (L66055-66075) | `gameSettings.set({[key]: JSON.parse(value)})` | void. Errors: `SETTINGS_KEY_NOT_FOUND`, `SETTINGS_INVALID_VALUE`. |
| `userInfo_get` (L66182-66198) | Finds the `USER_ACCOUNTS` entry whose `id === accountId`. | The JSON string of that stored account object. Missing: `USER_INFO_UNAVAILABLE`. [INF] Fields come from the launcher's account store (login, nickname, type...). See the auth doc. |
| `askForGuestComplete` (L65483-65547) | Only for `type === "GUEST"` accounts (otherwise `USER_NOT_GUEST`). Opens the UI guest-completion flow and waits for `GUEST_VALIDATE`. | A new `createToken(gameId, …)` result, or `GUEST_COMPLETION_ERROR`. [INF] The second argument passed is the IPC payload, not the accountId. This looks like an upstream bug. Not needed for normal accounts. |
| `release_restartOnExit` (L66018-66029) | Pushes the instanceId to `instancesToRestart`. | void. On exit, the launcher relaunches with the same hash/session. |
| `release_exitAndRepair(gs, restart)` (L66005-66015) | `stopAllAndRepair`: kills all instances, repairs, and relaunches if `restart` (L35156-35180). | void |
| `zaapVersion_get` (L66213-66221) | `app.getVersion()` | `JSON.stringify("3.15.5")`, which is a quoted string. |
| `zaapMustUpdate_get` (L66200-66209) | | `!AutoUpdater.hasChecked \|\| AutoUpdater.updateAvailable` |
| `hasPremiumAccess` (L65680-65690, L98623-98643) | Checks teasing access, then Haapi `ankama.premiumGame.connect`, which is `POST json/Ankama/v5/Game/Premium/Session/Connect` with `game_id=release.gameId` and header APIKEY (L114392-114397). Stores the result as `gameSession` and disconnects it on exit (L114399-114404). | true. On failure, a ZaapError is passed, but the result struct has no error field (see 2.5). |
| `hasGameAccess(gs, gameId)` (L65662-65676, L98644-98650) | Is `gameId` in `AppSettings.getAccount(accountId).gameList[].id`? | bool. Errors: `UNKNOWN`. |
| `payArticle*`, `payCart*` (L65693-66000) | Open a store overlay window (shop-api / store overlay URLs) over the game pid. | bool "order confirmed", or an `OVERLAY_*` error. A minimal launcher can return `ZaapError{OVERLAY_UNEXPECTED_ERROR}`. [INF] The game should handle that gracefully. |
| `system_addNotification` (L66077-66156) | Native OS notification. | `"click"`, `"close"`, the reply text, or the action text. |
| `addNotification` (L65380-65468) | Launcher-styled notification. | `NotificationResult{id, type}`. |

#### `Auth.createToken(gameId, accountId)`: how the game token is produced (L2742-2799, L113136-113156) [OBS]

1. Find the api-key record for `accountId` in `Auth.apiKeys`. If none, reject with
   `"Trying to connect an account not registered in apiKeys"`.
2. `refreshApiKey(apiKey)`. See the auth/Haapi doc.
3. If a stored certificate exists for `apiKey.login`:
   - `certificate_id = cert.id`
   - `certificate_hash = sha256(hm1 + AES-256-ECB-decrypt(base64(cert.encodedCertificate), key=hm2))`
     (L25459-25469; hm encoders at L64939-64942). See the auth doc.
4. `GET https://haapi.ankama.com/json/Ankama/v5/Account/CreateToken?game=<gameId>&certificate_id=<id>&certificate_hash=<hash>`,
   with header `APIKEY: <apiKey.key>`. The URL comes from `haapiUrl` (L72298) plus the path at L3258.
   - [INF] The query is built with Node `querystring`. Without a certificate, the keys are still sent with empty
     values.
5. Return `response.body.token`. That string is the `success` value of `auth_getGameToken`.

`gameId` is the value **sent by the game** (field 2). The launcher passes it through without validation.

### 2.5 Edge cases worth reproducing (or not)

- A non-`ZaapError` failure reaches the game as an EXCEPTION message (`TApplicationException` type 0, UNKNOWN).
- `hasPremiumAccess` / `hasGameAccess` errors: the processor builds `new Result(zaapError)`. That struct has no
  `error` field, so the game receives an empty REPLY. [INF] The Thrift client reports this as "unknown result".
- The `message` set in `new ZaapError({code, message})` (settings handlers) is dropped. Only `code` and `details`
  are serialised.
- The server processes several messages per TCP chunk. It rolls back on a partial read (`TBufferedTransport.receiver`
  plus `rollbackPosition`, L130397-130413). The connection stays open, so one socket per game instance carries many
  calls.

### 2.6 Text-socket variant (port 26117) [OBS]

`TextSocketServer` (L128839-128975):

- Listens on `127.0.0.1:26117` (`buildConfig.service.portTextSocket` overrides it). It exposes the **same RPC
  handlers** (`thriftDefinedApis`).
- On accept it sends `connected\0`. It then tries to identify the game pid from the socket's remote port
  (`find-process`, L128899-128915). It sets `processPid` to the pid or ppid that has registered credentials, so
  `connect` can fall back to a lookup by pid.
- Request: `"<method> <arg1> <arg2> ...\0"`. Arguments are separated by spaces and are strings. The trailing `\0`
  is stripped from the last argument. The argument count must equal `execute` arity - 1; if it does not, the request
  is silently ignored (warning only).
- Reply: `"<method> <result>\0"`, or `"<method>\0"` for void/empty, or `"<method> error <code|statusCode>\0"`.
- [INF] This is a legacy channel, probably for Flash/AIR clients (Dofus 2 / Retro).
- [UNK] Which current client uses it. Dofus 3 / Unity is expected to use Thrift on 26116 (see section 5). A
  replacement launcher should implement 26116 first, and add 26117 only if a client turns out to need it.

---

## 3. Games, ids and releases

- **Registry** [OBS]:
  - The game list comes from the cytrus repository data: `{ name, games: { <gameUid>: { gameId, name, order,
    assets: { meta }, platforms: { windows: { <release>: <version> } } } } }` (L141021-141050).
  - `gameUid` is the object key. It becomes `ZAAP_GAME`, folder names, and the cytrus update id.
  - `gameId` is the numeric id, used for `premiumGame.connect` and KPIs.
  - The release names are the keys of `platforms.windows`. They become `ZAAP_RELEASE`. `main` is sorted first
    (L141213-141222).
  - The platform key is `"windows"` on win32 (L20888-20891).
  - Per-release metadata: `assets.meta` becomes `release.information`. The launch-relevant fields of `.default`
    are `multiAccount`, `teasing`, `earlyAccess`, `guestAllowed`, `needName`, `askDataCollect` and `url`
    (L98541, L34591, L34832, L34852-34870).
- Local state [OBS]: `<userData>/repositories/<repoName>/<gameUid>/<release>/release.json` and `settings.json`
  (L140990-141003, L34407, L35576). On Windows, `<userData>` is `%APPDATA%\zaap`.
- **Dofus ids**:
  - [OBS] `GAMES_NAME` maps 1 to "Dofus" (L981).
  - [OBS] `ACCOUNT_DETAILS_OPTIONS.ABODOFUS.gameId = 1` (L965).
  - [OBS] Retro is special-cased to KPI gameId 101 (`"Retro" === r.name`, L8220). [INF] Retro's registry gameId
    differs, or is shared.
  - [OBS] `GO_SHOP_TO` keys `dofus` and `retro` (L927-928).
  - [OBS] The launcher's own `ZAAP_GAME_ID` is 102 (L787). The chat gameId is 99 (L940).
  - [INF] The Dofus 3 gameUid is `dofus` with gameId `1`, and the releases are `main` (plus possibly `beta`). Retro's
    gameUid is likely `retro`. **Verify** against the live cytrus registry or the installed `release.json`.
  - [INF] For `auth_getGameToken`, the game sends its own `gameId` (expected `1` for Dofus). The launcher only
    forwards it.
- KPI release id convention: `releaseId = gameId * (release !== "main" ? -1 : 1)` (L8221-8223). This is not part
  of the game protocol.

---

## 4. Minimal replacement-launcher sequence (Dofus 3)

1. Obtain an api key for the account (auth doc). Keep it in memory or an OS keystore. Never log it.
2. Resolve `installDir`. Read and render `installDir/zaap.yml` with
   `{os, zaap:{language,...}, env:{ZAAP_*}, game:{location, settings}}` to get `executable`, `arguments` and `env`.
3. Start the Thrift server (TBufferedTransport + TBinaryProtocol) on `127.0.0.1:26116` **before** spawning. The
   official launcher has it up from login onward.
4. Spawn: `spawn(join(installDir, executable), arguments, {cwd: installDir, detached: true, stdio: "ignore", env})`
   with the env from section 1.4. Use one `hash`/`instanceId` per process and map hash to `{accountId, pid}`.
5. Implement at least `connect`, `auth_getGameToken`, `auth_getGameTokenWithWindowId`, `settings_get`/`settings_set`
   (backed by a local settings map), `userInfo_get`, `updater_isUpdateAvailable` (false), `zaapVersion_get`
   (`"\"3.15.5\""`), `zaapMustUpdate_get` (false), `hasGameAccess`, `hasPremiumAccess`, and
   `release_restartOnExit`. Stub the pay and notification methods with ZaapErrors.
6. Multi-account: launch each instance with a different `ACCOUNT_ID` and hash. Tokens are per session
   (`accountId` bound to the hash). This is cleaner than the `windowId` path.

---

## 5. Open questions [UNK]

1. The exact Dofus 3 (and Retro) `zaap.yml`: executable, arguments, `env`, `settings`, `maxInstances`, `hooks`,
   `checkConfiguration`. Read it from an installed game.
2. Which RPCs Dofus 3 actually calls, and in what order. Expected: `connect`, then `auth_getGameToken`,
   `settings_get`, `userInfo_get`. Confirm by decompiling the Unity client (look for the `ZaapService` client in
   `GameAssembly`/IL2CPP metadata) or the Dofus 2 AIR client.
3. Whether Dofus 3 uses the Thrift port (26116) or the text socket (26117). Also whether it reads env vars, command
   line arguments, or both. (`credentials.json` is dev-only.)
4. The exact `settings_get` keys Dofus requests. They come from `zaap.yml` `settings:`.
5. The cytrus registry values for Dofus (gameUid, gameId, release names). They are fetched at runtime and not
   present locally.
6. Whether CreateToken without a certificate (empty `certificate_*` params) is accepted for accounts that have Shield
   enabled. See the auth doc.
