# 04 - Architecture, settings/local data, performance, Dofus-only scope

Target: Ankama Launcher ("Zaap") 3.15.5, build `52427f92` (2026-09-09), Electron **16.0.6** (Chromium 96), Vue 2.7 + Vuex 3.
Method: static reading only. The launcher was not run. Credential files were not opened.

Citation conventions:
- `main.js:N`, `renderer.js:N`, `preload.js:N` are line numbers in `re/pretty/*.js`.
- `chunk N` means the minified `re/app-extracted/N.bundle.js`. These have no useful line numbers, so they are cited with a grep-able snippet.
- `pkg` means `re/app-extracted/package.json`.
- Each claim carries a tag: **[O]** observed in code/files, **[D]** derived from observed code (mechanical consequence), **[I]** inferred (plausible, not proven), **[U]** unknown.

Sibling docs own the details for auth/HAAPI (`01`), the game launch/thrift protocol (`02`) and Cytrus updates (`03`). Here they appear only at the architecture level.

---

## 1. Architecture map

### 1.1 Process / composition model

- **[O]** Single main bundle. The webpack entry is `n(277).instance.setup()` (`main.js:157097`). It is the `Main` orchestrator class at `main.js:41738-42118`.
- **[O]** Services are singletons (`static get instance()`). Dependencies are injected through constructor default objects; the `instill` pattern is at `main.js:1386`.
- **[O]** Electron app switches set at load (`main.js:1507-1512`): `disable-renderer-backgrounding`, `ignore-gpu-blacklist`, `enable-gpu-rasterization`, `enable-native-gpu-memory-buffers`.
  - `disable-http2` is added when the auto-updater is configured (`main.js:8744`).
  - On win32, `high-dpi-support` and `force-device-scale-factor=1` are added when the overlay module loads (`main.js:100370-100373`).
- **[O]** Startup sequence (`Main.setup` at `main.js:41818`):
  1. DNS `verbatim`.
  2. buildConfig, Sentry, logger.
  3. Deep-link handler.
  4. Logger IPC, buildConfig IPC, beta-inscription IPC.
  5. Extra CA import.
  6. **V2 launcher probe.** TCP-connects to 127.0.0.1:26116-26120 with a 1.5 s timeout each (`main.js:147650-147700`).
  7. Protocol registration.
  8. `app.setup()` (`main.js:1587`). This takes the single-instance lock, sets userData to `%APPDATA%\zaap` and sets the Settings path.
  9. `source.json` merge.
- **[O]** `onAppReady` (`main.js:41887`):
  - Opens the main window, unless `--hidden` was passed or `.startHidden` exists.
  - Sets up Connectivity.
  - Installs global `webRequest` hooks that **strip `X-Frame-Options` and `Content-Security-Policy` from every response and force `Access-Control-Allow-Origin: *`** (`main.js:41925-41945`).
- **[O]** `bootstrapOnce` (`main.js:41996`):
  1. `checkUpdate()`, the launcher self-update. If an update was downloaded, the launcher restarts and the bootstrap is skipped.
  2. `primarySetups()` (`main.js:42013`).
  3. `delayedSetups()` (`main.js:42063`).
  4. After login, `delayedSetupsAfterLogin()` (`main.js:42093`) starts the script spawner, the **Thrift game service on 127.0.0.1:26116** (`main.js:1989`, `2009`) and the TextSocketServer.
  - When the app is opened hidden, the bootstrap is delayed by `BOOTSTRAP_DELAY = 18e4` (3 min) (`main.js:41797`, `41993`).

### 1.2 Main-process services (from the `Main` dependency list, `main.js:41740-41793`)

| Service (dep name) | Role | Started in | Dofus-only need |
|---|---|---|---|
| `app` (`main.js:1487`) | Electron app wrapper: windows, paths, exit guard, protocol, clear cache, wallpaper | setup | Keep (minimal) |
| `AppSettings` (`main.js:587-785`) | electron-settings wrapper, defaults, migration, settings IPC | onReady | Keep (replace backend) |
| `buildConfig` (`main.js:1060-1118`) | `package.json` + `static/source.json` merge | setup | Keep (hardcode) |
| `logger` (`main.js:57-160`) | winston, `application.log` | setup | Keep |
| `sentry` (`main.js:24930-25010`) | `@sentry/electron`; **inactive** because `pkg sentry.dsn=false` and `source.json` does not override it **[O]** | setup | Drop |
| `DeepLink` (`main.js:62895-62960`) | `zaap://` scheme, `second-instance`, `open-url` | setup | Optional |
| `launcherProbe` (`main.js:147650`) | Detects the legacy V2 launcher via the port probe | setup | Drop (or keep a 1-port check) |
| `Connectivity` (`main.js:6738-6820`) | Hidden 0x0 BrowserWindow reading `navigator.onLine`; `is-online` retry every 5 s while offline | onReady | Replace with a simple check |
| `AutoUpdater` (`main.js:8690-8870`) | electron-updater self-update | bootstrap | Drop (your own app) |
| `ZaapI18next` | i18n | primary | Keep (fr/en only) |
| `universe` / `UniverseManager` (`main.js:140425-140560`) | Downloads `desktop/universe/index.json` and per-universe file bundles; **provides `games.cytrusUrl`** (`main.js:140785-140816`) | primary | Keep logic, or hardcode the Dofus universe |
| `updateQueue`, `gameIpcManager`, `releaseIpcManager` (`main.js:24480-24600`), Registry/Repository (`main.js:140980-141140`) | Game list, install/update/repair/move, release state | primary | **Keep** (see 03) |
| `Tray` (`main.js:136840-136945`) | Tray icon with Open / Settings / Check updates / Exit | primary | Optional (nice-to-have) |
| `menu` | macOS-only app menu | primary | Drop |
| `Provider` | Third-party login provider window (`static/provider.html`, `main.js:38630`) | primary | Drop unless needed (see 01) |
| `Chat` / SpinConnector (`main.js:63150-63330`, `119740-119860`) | Social chat: socket to `zaap-chat.ankama.com:6337` (`tls.connect`-style signature **[I]**), 30 s heartbeat | primary (connects on `logged-in`) | **Drop** |
| `Auth` (`main.js:64789+`) | Login, API keys, multi-account | primary | Keep (see 01) |
| `User` (`main.js:2209+`) | Profile, nickname, newsletter, avatar | primary | Mostly drop |
| `Shop` (`main.js:64400`) | Shop articles and ogrines | primary | Drop |
| `Home` (`main.js:43755-43775`) | Widgets from `desktop/home/widgets.json` and `configs.json` (`pkg home.enabled=false`) | primary | Drop |
| `VideoIpcManager` (VOD SDK), `TowerIpcManager` (webtoons, `main.js:136950+`), `TwitchIPCManager` (`main.js:140372-140410`), `Twitch` (`main.js:45920-46050`) | Media features | primary | **Drop** |
| `Pip` (`main.js:33816-33860`) | Picture-in-picture video windows | primary | Drop |
| `goAnkama`, `overlay` (`main.js:7670-7730`), `emoji` | go.ankama.com links, in-game shop/payment overlay, emoji panel | primary | Overlay optional (only for in-game purchase) |
| `Kard`, `Event`, `Announcement` (`main.js:147600-147648`), `notificationManager` (`main.js:10890-11330`) | Codes/cards, event mini-games, announcements, toast window | primary/delayed | Drop (toasts optional) |
| `autoLaunch` (`main.js:135180-135230`) | `auto-launch` with `isHidden` (adds `--hidden`), toggled by `AUTO_LAUNCH` | delayed | Optional |
| `NewsIpcManager` (`main.js:69045`), `ChangelogIpcManager` | HAAPI `ankama.cms.items.*` news/changelog | delayed | Optional (changelog is useful) |
| `Terms` (`main.js:136734-136817`) | ToU version check/accept via `ankama.legal.*` | delayed | **Keep** (it may gate login) **[I]** |
| `disk` (`main.js:58626`) | `check-disk-space` → spawns **PowerShell `Get-CimInstance Win32_LogicalDisk`** (`main.js:109086`) | delayed / on demand | Keep (use the native `fs.statfs` or `GetDiskFreeSpaceEx`) |
| `service` (`main.js:1975-2060`) + `TextSocketServer` (`main.js:128880-128930`) | Local Thrift RPC server that the game uses for tokens/settings; per-connection PID lookup via `find-process` | after login | **Keep** (see 02) |
| `scriptSpawner` | Runs release-defined scripts (`SPAWN_SCRIPT`, a sync IPC) | after login | Keep if the Dofus release config uses it **[U]** |
| `unInstaller` (`main.js:137155-137300`) | `--uninstaller` mode; uses `tasklist`, `fkill`, `ps-list`/`fastlist.exe`, `process-exists` | `--uninstaller` only | Drop |
| KPI (`main.js:8220-8590`) | `ankama.game.sendEvents`, session start/end, CPU/GPU specs, flushes after 1 h or at the batch cap | various | Drop **[I: probably not required by servers]** |
| Device type (`main.js:136165-136200`) | PowerShell `Win32_SystemEnclosure` chassis query → LAPTOP/DESKTOP | KPI/device info | Drop |

### 1.3 Windows, webviews, tray

**[O]** The code creates at least 7 BrowserWindow kinds. The rea static graph reported only 2, so it undercounts this.

| Window | Site | Notes |
|---|---|---|
| Main | `main.js:1730-1858` | 1280x670, min 1024x670, frameless, `webviewTag:true`, preload `preload.js`, loads `index.html#loading`. Size and maximized state are persisted on **every `resize`** event. |
| Connectivity probe | `main.js:6779-6788` | Hidden 0x0 window with an inline `data:` page. It costs one extra renderer process just for `navigator.onLine`. |
| Notification manager | `main.js:10903-10941` | 333 px x full work-area height, transparent, `alwaysOnTop("pop-up-menu")`, click-through. Created lazily on the first toast, then hidden or shown. While active, a 200 ms cursor-poll runs (`main.js:11312`). |
| Twitch login | `main.js:46037` | 450x650, `twitch.tv/login`. |
| PiP | `main.js:86500-86540` | Video picture-in-picture. |
| In-game overlay | `main.js:100361-100470` | 70x70 transparent frameless window attached to the game PID. Registers a global `Alt+F4`. Driven by a **400 ms `active-win` poll** (`main.js:100574-100582`). Used by `payArticle`/`payCart` RPCs **[I]**. |
| Uninstaller | `main.js:137190-137200` | 870x480 transparent; `--uninstaller` only. |

- **[O]** Webviews: on `will-attach-webview` the launcher forces `nodeIntegration=false`, `preload=webview-preload.js` and **`allowRunningInsecureContent=true`** (`main.js:1708-1716`). The renderer uses a `<webview partition="private">` with `overlay-preload.js` (`renderer.js:23831-23880`). Twitch embeds (`embed.twitch.tv`) appear in chunks 6-19, 22, 32 and 44.
- **[O]** Tray: icon at `static/tray/icon.png`. The menu has Open, Settings (sends `zaap.settings.open`), "Check for updates" (launcher self-update) and Exit. Exit is disabled while games are connected (`main.js:136862-136945`).
- **[O]** Close behaviour (`main.js:1818-1845`):
  - `TRAY_AT_CLOSE === null` → shows the "tray at close?" popup.
  - Multi-account without "stay logged in" → shows a popup.
  - `exit()` refuses while games are connected to the service unless forced (`main.js:1878-1885`).

### 1.4 IPC

- **[O]** All channel names live in one constants module, `main.js:170-580`: **391 named channels**.
  - By prefix: CHAT 79, RELEASE 48, TOWER 41, ZAAP 20, VIDEO 18, USER 16, TWITCH 14, AUTH 13, WINDOW 11, NOTIFICATION 9, UNIVERSE 8, PIP 5, GAME 5, KARD 4, OVERLAY 4, TERMS 4, EVENT 9, and others.
  - **About 60% of the IPC surface (chat/tower/video/twitch/shop/kard/event/pip) is unrelated to launching Dofus** **[D]**.
- **[O]** Transport:
  - `ipcMain.handle`/`on` in each manager.
  - Main→renderer broadcast via `RemoteCommunication.send` (`main.js:1375-1383`), which goes to the main window, the uninstaller window and every PiP window.
  - The preload (`preload.js:157146-157185`) exposes the **raw `ipcRenderer` (send/sendSync/invoke/on...)** plus `__static`, `path`, `pathHelper`, `AppError`, `buildConfig`, `platform` and `i18nextBackend`. Separate preloads exist for notification, overlay and webview.
- **[O]** Sync IPC (it blocks the renderer):
  - `ZAAP_SETTINGS_GET_VALUE`, `ZAAP_GET_DEFAULT_LANGUAGE` and `ZAAP_GET_SUPPORTED_LANGUAGES` at i18n init (`renderer.js:17973-17975`).
  - `SPAWN_SCRIPT` (`renderer.js:21750`).
  - The settings handler answers through `e.returnValue = this.get(t)` (`main.js:697-699`).
- **[O]** The rea extract (`re/rea-ipc.txt`, 46 lines) lists only partially resolved handler sites (`o.ENVELOPE`, Sentry IPC, chat/release listeners). The constants module above is the authoritative list.
- Dofus-relevant channels:
  - `auth.*`, `release.*` (install/update/pause/resume/cancel/repair/start/uninstall/move/getFolderSize/gameSettings.update/open.logdir), `game.list|added|removed|updated`, `check.manifest*`
  - `zaap.settings.*`, `zaap.getVersion`, `zaap.quit`, `zaap.open.dialog`, `device.get.drive.info`, `get.server.status`
  - `terms.*`, `connectivity.*`, `changelog(s).get`, `mainProcess.ready`, `windows.main.ready`
  - `window.*`, `logger.open.logdir`

### 1.5 Launcher self-update (electron-updater)

- **[O]** `resources/app-update.yml` contains `provider: generic`, `url: 0.0.0.0`, `updaterCacheDirName: zaap-updater` and publisherName `ANKAMA GAMES*`. **At runtime the URL is overridden** by `setFeedURL(autoupdaterUrl + "/" + environment)` = **`https://launcher.cdn.ankama.com/installers/production`** (`main.js:8785-8790`, `pkg app.autoupdaterUrl`, `environment:"production"`). electron-updater then fetches `latest.yml` from there **[I: standard generic-provider behaviour]**.
- **[O]** Behaviour:
  - `autoDownload=true`.
  - The check runs at bootstrap and **every 10 min** (`setInterval(..., 6e5)`, `main.js:8834`).
  - After a download:
    - If games are connected → the renderer gets a force-update prompt (`zaap.autoUpdater.force.asking`).
    - If the window is hidden → a `.startHidden` file is created and the launcher **silently runs `quitAndInstall`**.
    - Otherwise → a popup with a **10 s auto-restart countdown** (`renderer.js:25677-25712`).
  - (`main.js:8803-8828`)
- **[I]** These silent or forced self-restarts, together with Cytrus game-update queueing, are a likely source of the "frequent update/package conflicts" the user sees. A replacement launcher has no reason to self-update this way.

### 1.6 Auto-launch

- **[O]** `auto-launch@5.0.5` with `{name, isHidden:true, path: exe}`. It follows the `AUTO_LAUNCH` setting, default `false` (`main.js:135197-135220`, `620`). On Windows this is an HKCU `...\CurrentVersion\Run` entry with `--hidden` **[I: auto-launch library behaviour]**. When the app is opened hidden, the bootstrap is delayed 3 min.

---

## 2. Settings and local data

### 2.1 Paths

| Path (Windows) | What | Source |
|---|---|---|
| `%APPDATA%\zaap\` | userData (forced: `appData` + `name`="zaap") | `main.js:1609-1617` [O] |
| `%APPDATA%\zaap\Settings` (no extension, JSON) | Global settings (electron-settings 3.2.0) | `main.js:1617`, `43518-43562` [O] |
| `%APPDATA%\zaap\application.log` | winston log, 10 MB, 1 file, tailable | `main.js:98-118`, `122-148` [O] |
| `%APPDATA%\zaap\crash.json` | Crash marker, deleted after an update | `main.js:41841`, `41973` [O] |
| `%APPDATA%\zaap\gamesLogs\<Game>-<release>\` | Per-release game logs (passed as `ZAAP_LOGS_PATH`) | `main.js:34620-34640`, `98565` [O] |
| `%APPDATA%\zaap\source.json` | Copy of `resources/static/source.json` | `main.js:1079-1108` [O] |
| `%APPDATA%\zaap\.startHidden` | "Open hidden next time" flag (after a silent update) | `main.js:1559-1583` [O] |
| `%APPDATA%\zaap\repositories\repositories.json` and `repositories\<server>\<gameUid>\...` | Cytrus repository data and per-game/release state | `main.js:140988-141003`, `141110-141121` [O] |
| `<release dir>\release.json` | Release state: `location`, `version`, `installedFragments`, `isInstalling/Updating/Repairing/Moving/isDirty`, `_schemaVersion:1` | `main.js:34408`, `34509-34528` [O] |
| `<release dir>\settings.json` | Per-release game settings, incl. **`autoUpgrade`** (default `!internal` = true) | `main.js:35547-35584` [O] |
| `<install>\information.json`, `<install>\data\release.json`, `manifest.json`, `.release.infos.json`, `migration.launcher.json` | Installed-game metadata | `main.js:110226-110265`, `24560`, `6551-6552` [O] |
| `%APPDATA%\zaap\universe\index.json`, `universe\<Name>\...` | Remote universe/theme/service configs, cached for offline use | `main.js:140435-140460`, `140586` [O] |
| `%APPDATA%\zaap\terms\` | Cached ToU | `main.js:136817` [O] |
| `%APPDATA%\zaap\keydata`, `certificate`, `guest` | **Auth material. Not opened.** See 01. | `main.js:64826`, `64944-64947` [O] |
| `%APPDATA%\zaap\sentry\` | Sentry offline store (inactive) | `main.js:152480` [O] |
| `%APPDATA%\zaap\Cache`, `Code Cache`, `GPUCache`, `Local Storage`, ... | Chromium session data (cleared by `zaap.clear.cache`) | `main.js:1630-1638` [I: standard Electron layout] |
| `%LOCALAPPDATA%\zaap-updater\` | electron-updater pending download (`update-info.json`) | `app-update.yml`, `main.js:133538` [I: standard path] |
| `%LOCALAPPDATA%\Ankama\` | **Default games root** (`getGamesPath`) | `main.js:1866-1879` [O] |
| `%LOCALAPPDATA%\Ankama\Dofus-dofus3` | Suggested Dofus 3 install folder: `UpperFirst(gameUid)` + (`-<release>` unless release=`main`) | `main.js:34657-34672` [D, from `source.json` release id `dofus3`] |
| `~\zaap-development.json` | Dev-only release override config | `main.js:98695` [O] |

**[O]** `resources/static/source.json` contains `productName "Dofus 3.0"`, `source:"dofus"`, `sourceRelease:"dofus3"`, `favoriteReleases:[{id:"dofusdofus3",active:true}]` and `lastGameRoute "/Krosmoz/games/game/dofus/dofus3"`. So this install was made with the Dofus-branded installer.

**[U]** No `%APPDATA%\zaap` or `%LOCALAPPDATA%\Ankama` exists on the analysis machine, so the real on-disk values are unverified.

### 2.2 Global setting keys (`Settings` file)

Key list: `main.js:1124-1188`. Defaults: `main.js:619-674`. Schema version 2, with a migration that drops `WINDOWS_BOUNDS` (`main.js:770-779`).

| Key | Default | Relevance to Dofus-only |
|---|---|---|
| `LANGUAGE` | system locale (2 letters) → `en` if unsupported (en/fr/pt/es/de) | **Keep**. Game-dependent key (`GAME_DEPENDENT_KEYS`, `main.js:612`) |
| `KEEP_AT_GAME_LAUNCH` | `true` (the launcher stays open when the game starts; `false` → `window.hide`, `renderer.js:231077-231080`) | **Keep** ("close/hide on launch") |
| `TRAY_AT_CLOSE` / `TRAY_AT_MINIMIZE` | `null` (asks the user) | Keep if you have a tray |
| `AUTO_LAUNCH` | `false` | Optional |
| `GAME_PLAY_MODE` | `queue` (`queue` / `all`, update queue mode) | Keep (update concurrency) |
| `CYTRUS_LIMITED_SPEED` / `CYTRUS_DL_SPEED` / `CYTRUS_MAX_RAM` | `false` / 10 MiB/s / 128 MiB (bounds `main.js:897-900`) | **Keep** (see 03) |
| `UPDATE_FINISHED_NOTIFICATION` | `true` | Optional |
| `USER_ACCOUNTS`, `LAST_AUTHENTICATED_LOGIN`, `DEVICE_UID`, `REMEMBER_DEVICE`, `ACCEPTED_TERMS_VERSION` | - | Auth/ToU (see 01). **Contains account data; do not copy blindly.** |
| `GAMES_LIST`, `FAVORITE_RELEASES`, `TEMP_FAVORITE_RELEASES`, `LAST_GAME_ROUTE`, `UNIVERSE`, `THEME_BY_UNIVERSE`, `OS_ARCHITECTURE`, `OLD_DIRECTORY` | - | Partly (game list cache, arch) |
| `WINDOWS_SIZE`, `WINDOW_FULLSCREEN`, `INTERFACE_RATIO` (forced 1) | -, `true`, 1 | UI only |
| `ENABLE_ANIMATIONS`, `ENABLE_VIDEO`, `IMAGE_QUALITY`, `PRESELECT_PERFORMANCE` | `true`, `true`, `mq`, `mq` | Performance presets (`main.js:853-887`). Not needed if the UI is light. |
| `CHAT_*`, `FRIENDS_SHOW_ALL`, `REFUSE_FRIEND_INVITE`, `IS_USER_BUSY`, `VOD_*`, `WEBTOON*`, `TWITCH_LANG`, `LIVESTREAMS_NOTIFICATION`, `WIDGETS`, `LOCK_HOME`, `ANNOUNCEMENTS`, `EVENT_*`, `LOTTERY_CGU_ACCEPTED`, `FORCE_EVENT_SECONDARY_PAGE`, `BETA_INSCRIPTION_VOTED`, `TUTORIAL_MADE`, `FIRSTLAUNCH`, `LAST_ROUTE`, `LAST_READ_CHANGELOG`, `CONFIRM_ACCOUNT_PURCHASE`, `ENABLE_SWITCH_ACCOUNT_POPUP`, `ALWAYS_SHOW_MULTI_ACCOUNT_GAME_BUTTON`, `DISMISS_AUTHENTICATOR_ALERT` | - | Drop |

- Per-release `settings.json`: `autoUpgrade` (`main.js:35547-35580`), plus release-specific settings from the release configuration **[U: Dofus field list not enumerated here]**.
- "Release channel" is the **release name** (`dofus3`, and possibly `main`/beta variants exposed by the Cytrus repository) selected per game. There is no global channel key **[D]**.

---

## 3. Performance: likely causes of lag

### 3.1 Main process

| # | Mechanism | Interval / trigger | Site | Tag | Needed for Dofus-only? |
|---|---|---|---|---|---|
| P1 | **Synchronous settings I/O.** electron-settings 3 does `readFileSync` + JSON parse of the whole `Settings` file on every `get`, and read + `writeFileSync` on every `set`. There is also an `fs.watch` on the file, and each key watcher re-reads it. | every get/set | `main.js:43557-43600`, `43530-43536` | O | No. Use an in-memory store and debounced async writes. |
| P2 | **Window resize persists settings on every `resize` event** (2x `set` → 2 sync read/write cycles, plus an IPC broadcast `zaap.settings.updated`, plus a change emit) | each resize tick | `main.js:1812-1818`, `746-749` | O → D (main-thread stalls during resize) | No |
| P3 | Renderer **`sendSync`** for settings/i18n blocks the renderer until main has done the sync file read | startup, plus each call | `renderer.js:17973-17975`, `main.js:697` | O | No |
| P4 | Launcher self-update check | **10 min** + bootstrap | `main.js:8834` | O | No |
| P5 | Cytrus repository watch (game updates) | **120 s** | `main.js:141132`, `141384-141388` | O | Yes, but can be on demand at launch and every N minutes (see 03) |
| P6 | Release update `saveToDisk` loop during installs | interval during update **[U: value]** | `main.js:117270-117272` | O | Yes (during updates only) |
| P7 | Cytrus stats/queue timers | 1 s stats; 5 s backoff | `main.js:142030`, `143064` | O | Yes (during updates only) |
| P8 | Chat heartbeat on a persistent socket to `zaap-chat.ankama.com:6337`, plus initial fetch of channels/friends/invites/blocked lists | **30 s** (after login) | `main.js:63309-63312`, `63314-63330` | O | **No** |
| P9 | Overlay active-window poll (`active-win` native call) while an in-game overlay exists | **400 ms** | `main.js:100574-100582` | O | No (only for in-game purchases) |
| P10 | Notification window cursor poll while the mouse is over a toast | **200 ms** | `main.js:11312-11323` | O | No |
| P11 | Connectivity retry (`is-online` → `icanhazip.com` / `api.ipify.org` + DNS) while offline; uses a **hidden extra BrowserWindow** | **5 s** while offline | `main.js:6742`, `6779-6805`, `88872-88879` | O | Use a light check |
| P12 | `find-process` per game connection to the TextSocketServer: `netstat -ano` + **PowerShell `Get-CimInstance win32_process`** (WMIC fallback) | each game connect | `main.js:128908-128916`, `129071`, `129395-129410` | O → I (≈0.5-2 s CPU spike per launch) | Partly. The game-PID check is needed. Use `GetExtendedTcpTable`/native APIs instead. |
| P13 | `check-disk-space` → **PowerShell CIM** (wmic on old Windows) | on drive-info request / install / move | `main.js:109080-109095`, `35054`, `58626` | O | Yes, but use a native call |
| P14 | Device-type PowerShell query (`Win32_SystemEnclosure`) | KPI/device info | `main.js:136180-136186` | O | No |
| P15 | KPI batching (`ankama.game.sendEvents`), flush timer | 1 h debounce | `main.js:8433` | O | No **[I]** |
| P16 | node-cache TTLs (vod/news/shop/tower/user 300 s, gameList 60 s, kard 150 s) | on access | `main.js:113195-113204` | O | Keep the gameList cache concept |
| P17 | Sentry session flush / 60 s session persist | only when `dsn` is set; **dsn=false**, so inactive | `main.js:150235-150240`, `153362-153372`, `pkg` | O | No |
| P18 | `disable-renderer-backgrounding`: the hidden/minimized/occluded renderer keeps **full-rate timers and rAF**. Combined with `KEEP_AT_GAME_LAUNCH=true` by default, the whole Vue UI keeps running while Dofus is played. | permanent | `main.js:1507-1509`, `628` | O → I (main cause of "lag while in game") | No |
| P19 | `allowRunningInsecureContent`, CSP/XFO stripping, global `ACAO:*` | permanent | `main.js:1715`, `41925-41945` | O (security, not perf) | No. Avoid in a replacement. |

### 3.2 Renderer (Vue UI)

| # | Mechanism | Interval | Site | Tag | Needed? |
|---|---|---|---|---|---|
| R1 | Server status poll of `status.cdn.ankama.com/export.json` via IPC `get.server.status` | **120 s** while the game page is mounted | chunk 10 (and 6-19): `statusInterval=setInterval(this.refreshStatus.bind(this),12e4)`; main handler `main.js:24537-24549` | O | **Optional, useful.** Can poll less often. |
| R2 | Kickstarter JSON poll of `https://1099086437.rsc.cdn77.org/kickstarter.json?timestamp=` (cache-busted) | **120 s** while focused | chunk 10: `setInterval(this.fetchJSON,12e4)` | O | No |
| R3 | Carousel/pin auto-advance | 10 s | chunk 10: `setInterval(this.nextPin,1e4)` | O | No |
| R4 | Horizontal scroll offset recompute | **500 ms** | chunk 10: `setInterval(this.calcMaxOffsetUntilSet,500)` | O | No |
| R5 | News end timers / event countdowns | 1 s (several) | chunk 10, 20, 22; `renderer.js:14700` | O | No |
| R6 | Event "wave" animation (DOM nodes created) | 1.65 s + 233 ms | chunk 20: `addWave` | O | No |
| R7 | VOD remaining-time | 5 s | chunks 23 and 28 | O | No |
| R8 | PiP info push | `informationTimer` | chunk 52 | O | No |
| R9 | video.js timers (timeupdate/ABR/live tracker 30-250 ms) when VOD/Twitch players are mounted | 30-250 ms | chunk 0 | O | No |
| R10 | Background **videos** in game carousels (`ENABLE_VIDEO` default true), **lottie** (40 refs in the main renderer, more in chunks 20/21/24/33), gsap, `requestAnimationFrame` in every game chunk, Twitch iframes (`embed.twitch.tv`) | continuous | `renderer.js:27925-27931`, `87951`; chunks 6-19, 20, 21 | O → I (GPU/compositor load, made worse by P18) | No |
| R11 | Heavy CSS: `backdrop-filter` (6-7 per game chunk) and `filter: blur` (8-9 per game chunk) | continuous compositing | `*.styles.css` (chunks 6-19) | O → I | No |
| R12 | **14 nearly identical game-page chunks** (6-19, ~350 KB JS + ~229 KB CSS each, same components such as `friends-in-game` and `zaap-template-game-carousel`) | load/parse | `app-extracted/6..19.bundle.js` | O → I (memory/parse cost) | No |
| R13 | `ENABLE_ANIMATIONS=false` only short-circuits Vue `<transition>` hooks; it does **not** stop lottie/video/rAF loops | - | `renderer.js:230730-230775` | O → D | - |

**Summary [I].** Ranked by likely impact:
1. P18 + R10/R11: the full animated Chromium UI keeps running behind the game at full rate.
2. P1/P2/P3: synchronous settings I/O on the main thread, plus sync IPC.
3. P12/P13/P14: PowerShell/CIM spawns around game launch and disk checks.
4. P8: the persistent chat socket.
5. Electron 16's old Chromium 96 compositor.

The 10 min self-update check plus silent `quitAndInstall` (P4, §1.5) and the 120 s Cytrus watch are the likely origins of update churn.

---

## 4. Dofus-only launcher scope

### 4.1 Must keep (MVP)

1. **Login/session** and API key refresh; the token handed to the game. See 01.
2. **ToU acceptance check** (`ankama.legal.terms` / `setTouVersion`). It may block login otherwise **[I]**.
3. **Game list / release resolution** for Dofus 3 only: `gameUid=dofus`, release `dofus3`. HAAPI `ankama.game.listWithApiKey` for entitlement **[I]**, plus the Cytrus repository. See 03.
4. **Install/update/repair** via Cytrus, honouring `CYTRUS_DL_SPEED`/`CYTRUS_MAX_RAM`. Persist `release.json` state compatible enough to reuse an existing install folder (`%LOCALAPPDATA%\Ankama\Dofus-dofus3` by default). See 03.
5. **Disk-space check** before install, done natively.
6. **Game launch** with env `ZAAP_PORT`, `ZAAP_GAME`, `ZAAP_RELEASE`, `ZAAP_INSTANCE_ID`, `ZAAP_HASH`, `ZAAP_CAN_AUTH`, `ZAAP_LOGS_PATH` (+`ACCOUNT_ID`) (`main.js:98557-98566`). Run the **local Thrift service on 127.0.0.1:26116** with the RPCs the game calls: `connect`, `auth_getGameToken*`, `settings_get/set`, `userInfo_get`, `zaapVersion_get`, `zaapMustUpdate_get`, `updater_isUpdateAvailable`, `release_restartOnExit`/`exitAndRepair`, `hasGameAccess`, `hasPremiumAccess`, `endGameSession`, and optionally `addNotification`/`payArticle*`/`payCart*`. These are the RPC module names from the webpack context in `main.js`. See 02.
   - **Port conflict:** the official launcher's V2 probe and your launcher both want port 26116. Only one may run at a time **[D]**.
7. Settings: language, keep-open/hide-on-launch, download limits, install path, auto-upgrade per release.
8. Logs (`application.log` equivalent) and the game logs folder.

### 4.2 Can drop

Chat/friends/presence (and the TCP connection to `zaap-chat.ankama.com:6337`), shop/ogrines and the in-game payment overlay (unless you want in-game purchases), VOD (`vod-api.ankama.com`, bradmax analytics), Tower/webtoons (`tower-api.ankama.com`), Twitch (embeds, drops, login window, `launcher.cdn.ankama.com/twitch`), PiP, Home widgets, news carousel/kickstarter/events/lottery/kards/announcements/beta inscription, wallpapers (`windows-wallpaper.exe`), emoji panel, Allskreen, avatars/newsletter, multi-universe theming, KPI/telemetry, Sentry, launcher self-updater, uninstaller mode (ps-list/fastlist/fkill/tasklist), V2 probe, the hidden connectivity window, the notification overlay window (use OS toasts), and the Electron webview with CSP stripping.

### 4.3 Network endpoints

| Endpoint | Purpose | Status |
|---|---|---|
| `https://haapi.ankama.com/` | Account, API keys, game list, ToU, game session, `cms.items` (news) | **Mandatory** (auth/session/entitlement); the news methods are optional |
| `https://auth.ankama.com/` | Login (see 01) | **Mandatory** |
| Cytrus CDN (`games.cytrusUrl` comes from the universe config, `main.js:140785-140816`; see 03) | Game manifests/fragments | **Mandatory** |
| `https://launcher.cdn.ankama.com/desktop/universe/index.json` and per-universe files | Universe config that **supplies `cytrusUrl`** and service configs | Mandatory in the official design; **optional if you hardcode the Dofus Cytrus URL** [D] |
| `https://game-service.ankama.com/` (`games.cloudSaveUrl`) | Cloud saves | Optional / unknown for Dofus 3 **[U]** |
| `https://status.cdn.ankama.com/export.json` | Server status (polled every 120 s on the game page) | Optional (nice) |
| `https://launcher.cdn.ankama.com/installers/production` (generic `latest.yml`) | Launcher self-update | Drop |
| `https://launcher.cdn.ankama.com/desktop/home/{widgets,configs}.json` | Home widgets | Drop |
| `zaap-chat.ankama.com:6337`, `https://launcher.cdn.ankama.com/configs/useractivities.json` | Chat | Drop |
| `https://shop-api.ankama.com`, `https://store.ankama.com/{lang}/overlay/auth`, `pay.ankama.*` | Shop/payment | Drop (keep only for in-game purchase overlay) |
| `https://vod-api.ankama.com`, `bradmax.com` analytics, `https://tower-api.ankama.com`, `https://www.allskreen.com` | Media | Drop |
| `https://launcher.cdn.ankama.com/twitch`, `embed.twitch.tv`, `player.twitch.tv`, `www.twitch.tv/login` | Twitch | Drop |
| `https://1099086437.rsc.cdn77.org/kickstarter.json`, `launcher.cdn.ankama.com/event/...` | Campaign/event content | Drop |
| `https://go.ankama.com/` | Redirect links (support, account pages) | Optional (open in browser) |
| `https://icanhazip.com/`, `https://api.ipify.org/`, `api6.ipify.org` | `is-online` connectivity probe | Drop (replace) |
| `127.0.0.1:26116` (+ TextSocketServer) | Local game↔launcher RPC | **Mandatory (local)** |

The rea endpoint dump (`re/rea-endpoints.txt`, 158 lines) is mostly routes/keys, such as the Vue routes `/actual/games` and `/pip/:universe/:type/:id`. It contains no URL that is missing from the table above **[O]**.

### 4.4 Other observations

- **[O, from rea]** `app.asar.unpacked` ships `ps-list/vendor/fastlist-0.3.0-{x64,x86}.exe`, `process-exists/node_modules/ps-list/fastlist.exe` and `wallpaper/source/windows-wallpaper.exe`. rea reports that these contradict the asar integrity metadata. Not independently verified here. All of them serve features this launcher drops (uninstaller, wallpaper).
- **[O]** The bundle ships `electron-devtools-installer` and `unzip-crx-3`, and `protocol-generated-nodejs` comes from an internal `ankama.lan` Nexus URL (`pkg`). There is dev and internal residue in production.
- **[U]** Real runtime frequencies of the renderer polls depend on which route is mounted. The overlay's trigger path from the game RPCs is inferred, not traced end to end.
