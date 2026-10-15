# 03 — Cytrus v6: install / update / repair pipeline

Scope: how Ankama Launcher (Zaap 3.15.5, `@Zaap/cytrus-client` 0.1.6 bundled) resolves, downloads,
verifies and records Dofus game files, and what a replacement launcher must do.

Legend: **[O]** observed (code line cite in `re/pretty/main.js`, or a live CDN response sample) ·
**[D]** derived (follows directly from observed code or data) · **[I]** inferred · **[U]** unknown.

Samples (all unauthenticated public GETs, about 2.5 MB total) are in `re/samples/`. The tools are in `re/tools/`:
`cytrus_manifest_dump.py` (full local parse) and `cytrus_sparse_probe.py` (parses a remote manifest through
budgeted HTTP Range requests). The schema is in `docs/cytrus-manifest.fbs`.

---

## 1. Endpoints and formats

### 1.1 Discovery chain
| Step | URL | Notes |
|---|---|---|
| Universe list | `https://launcher.cdn.ankama.com/desktop/universe/index.json` | [O] L140443 (`distantUrl` L72300). Sample: `samples/universe-index.json`. Each universe lists `files.{config,icon,…}` = `{name, hash}`; hash = SHA-1 of the file [O] (sha1 of downloaded config = `affa947d…`, which matches the listed hash). |
| Universe config | `https://launcher.cdn.ankama.com/desktop/universe/{path}/index.json` (Krosmoz `path`=`ankama`) | [O] sample `samples/universe-ankama-config.json`: `games.cytrusUrl = "https://cytrus.cdn.ankama.com"`, `maxRetry 3`, `retryDelay 1000`. Consumed at L140784-140791. |
| Repository | `{cytrusUrl}/cytrus.json` | [O] L141448-141460. Has to be `version === 6`, otherwise it throws and stops watching. |

A replacement launcher can hard-code `https://cytrus.cdn.ankama.com` [D]. To be safe, it can also re-read the universe config.

### 1.2 `cytrus.json` (v6) [O] (`samples/cytrus.json`, 3.5 KB, `Last-Modified` header present)
```json
{ "version": 6, "name": "production",
  "games": { "dofus": {
      "name": "Dofus", "order": 1, "gameId": 1,
      "assets":   { "meta": { "<release>": "<sha1 of assets tar>" , ... } },
      "platforms": { "windows": { "<release>": "<version>" , ... }, "darwin": {...}, "linux": {...} } } } }
```
- Platform key is `windows` for win32, otherwise `process.platform` (L20888-20891) [O].
- Dofus releases on 2026-10-15 [O]: `main` = `6.0_2.73.3.14`, `dofus3` = `6.0_3.7.10.10`, `beta` = `6.0_3.7.3.3`,
  `experimental` = `6.0_3.6.12.20`. `assets.meta` also has `*_topush` keys that have no platform entry.
  [I] These keys are staging keys and should be ignored.
- The repository `name` (`production`) becomes a directory under `%APPDATA%\zaap\repositories\` (L141064-141073) [O].
- Version = `"6.0_" + gameVersion`. A version is accepted only if it `startsWith(6)` (L34927-34930, check L117414-117419) [O].
  The UI shows `split("_")[1]` (L35385) [O].
- Releases that are visible = keys of `platforms[localPlatform]` (L141030-141033) [O].
  The repository data is cached in `%APPDATA%\zaap\repositories\repositories.json`, keyed by the server URL (L140998-141121) [O].

### 1.3 Polling [O]
- `watch(120000)`: `checkForReleaseUpdates` runs immediately and then every 120 s (L141389-141396, L141130-141133).
- An update is emitted only if `Last-Modified` is newer than the cached one **and** the body differs (deep-equal) (L141418-141429).
- HTTP client: axios. Timeout is 4 s. Keep-alive agents. There is one random server from a list of 1 (L141370-141378).
  `fetch` retries **only** on `ECONNRESET/ETIMEDOUT/ECONNREFUSED/ECONNABORTED/EHOSTUNREACH/ENOTFOUND`,
  with `minTimeout 250`, `maxTimeout 2000`, `retries 1800` (L141401-141413). HTTP 4xx/5xx are not retried at this layer.

### 1.4 Per-release files (`{base}/{game}/…`)
| Resource | URL | Format |
|---|---|---|
| Size meta | `{game}/releases/{release}/{platform}/{version}.meta` | [O] JSON `[{name, totalSize}]` per fragment (L141468-141470). Samples `samples/dofus-*.meta`. |
| Config manifest | `{game}/releases/{release}/{platform}/{version}.config` | [O] FlatBuffers `Manifest` with only `configuration` (L141461-141467, L142082-142085). 272-400 B. |
| Full manifest | `{game}/releases/{release}/{platform}/{version}.manifest` | [O] FlatBuffers `Manifest` (L142078-142093). Uncompressed: `application/octet-stream`, no `Content-Encoding` even with `Accept-Encoding: gzip, br`. dofus3 = 51.4 MB, main = 11.4 MB. |
| Bundle blob | `{game}/bundles/{bundleHash[0:2]}/{bundleHash}` | [O] L142300-142304. Raw concatenated chunks, **no compression** (the chunk SHA-1 matches the raw bytes). Range requests are supported. |
| Release assets | `{game}/hashes/{assetsHash[0:2]}/{assetsHash}` | [O] L141433-141446. POSIX tar (`cover.jpg, home_cover.jpg, icon.png, logo.png, notification.svg, release.json, shortcut.ico`), extracted to `…/{release}/data/`. Sample `samples/assets-dofus3/release.json`. |

### 1.5 Manifest schema (FlatBuffers) [O + verified]
See `docs/cytrus-manifest.fbs`. `Manifest{fragments:[Fragment]}` → `Fragment{name, files:[File], bundles:[Bundle]}`.
`File{name, size:long, hash:[byte], chunks:[Chunk], executable:bool, symlink:string}`.
`Bundle{hash:[byte], chunks:[Chunk]}`. `Chunk{hash:[byte], size:long, offset:long}`.
- Code: Chunk L41359-41445, Bundle L69470-69562, File L69567-69693, Fragment L69698-69792, Manifest L142643-142705.
  JS models: L41447-41469, L69796-69866, L142706-142804. `toHexString`/`calculLong` (int64 = low + high·2^32) at L41471-41477.
- `File.chunks` empty ⇒ the file is a single chunk `{hash: file.hash, offset 0, size}` (L69823-69825) [O].
- `Chunk.offset` means the file offset in `File.chunks` and the blob offset in `Bundle.chunks` [O] (L142310-142320 vs L142352-142360).
- **Bundle hash = SHA-1(concatenation of the raw 20-byte chunk hashes, in order)** [O on 2 samples: config bundle `dcc5681b…`, start-script bundle `2254158f…`].
  It is not the content hash. [I] This holds in general.
- Chunking [O from probe]: variable-size chunks of about 15-85 KB. [I] The chunking is content-defined. Small files have no chunk list.
  A Dofus 3 bundle can hold hundreds of chunks (e.g. 706).
- Dofus 3 windows `6.0_3.7.10.10` has 12 fragments [O]: `audio, bones, configuration, data, i18n, map_common,
  map_textures_2x, map_textures_4x, picto, skins, start-script, win32_x64` (meta). Probed: `start-script` = 1 file `zaap-start.bat`.
  `skins` = 5599 files / 79 bundles. `win32_x64` = 78 files / 26 bundles (includes `Dofus.exe`, `D3D12/…`). The `executable` flag is false on Windows files.
  [U] Symlinks and `executable=true` were not seen (they are probably darwin/linux only).

### 1.6 Fragment selection (zaap.yml) [O]
- The `configuration` fragment holds `zaap.yml` (+ `zaap-windows.yml` for dofus3). These are **Nunjucks templates rendered and then parsed as YAML**
  (L52506-52617). The render context is `{os:{platform,arch,release,cores,homedir,tmpdir,ram}, zaap:<launcher settings>, env, game:{location, settings}}` (L52532-52552).
  The template is recompiled until it is stable, at most 5 iterations (L52586-52604).
- Defaults (L52441-52454): `fragments:["main","configuration"]`, `maxInstances:1`, `executable:"zaap-start"`, …;
  `configuration` is always added at the front (L52608-52612).
- Dofus 3 result [D] (`samples/bundle-dofus3-config/zaap*.yml`): `["configuration","win32_x64","start-script","bones","skins","audio","data","i18n","picto","map_common", (+"map_textures_2x" if hdReady) (+"map_textures_4x" if 4kReady), "start-script"]`.
  `maxInstances: true` (unlimited). The `arguments` come from the template (the launch spec is out of scope; see the start doc).
  `start-script` → `zaap-start.bat` runs `Dofus.exe -logFile "%ZAAP_LOGS_PATH%\dofus.%ACCOUNT_ID%.log" %*` [O sample].
- Duplicate fragment names (e.g. `start-script` appears twice) are processed twice by the launcher [D] (L142106-142145). A reimplementation should de-duplicate them.

---

## 2. Download / update algorithm (`CytrusUpdate`, module L141803-142638)

### 2.1 Config phase (every install/update/repair) [O]
1. `canCreateUpdate` (L117381-117421) checks: a location is set, the client is online, the release is **not running (only processes this launcher spawned)**,
   no update is running, the release is not moving, and the version starts with 6.
2. `loadConfigurationInTempFolder` (L117199-117241): `tmp.dir()` in `%TEMP%`, then a separate `CytrusUpdate` with `fragments:["configuration"]`
   downloads `.config` + bundle, renders `zaap.yml` and returns the fragment list. This is **not** placed in the global update queue.
   [D] The temp dir is never removed: `tmp.dir` cleanup is a non-recursive `rmdirSync` on exit (L117600-117613, L117528-117531).
3. In `instantiateAndInitUpdate` (L117317-117364): writes `{location}/.release.infos.json` = `{"gameUid","release"}` and
   `{location}/migration.launcher.json` = `{"launcher":1}` (L6551-6552, L34572-34582). Then it creates the real update with
   `fragments`, `forceRepair: type===REPAIR`, `ignoredFiles:[those 2 files]`, `dlSpeed` (0 = unlimited unless the "limited" setting is on, default 10 MiB),
   `maxRAM` (default 128 MiB; allowed range 64-512 MiB) (L663-665, L890-893).

### 2.2 Local state: `{location}/manifest.json` [O]
- Opened `r+`/`w+` at init (L141953-141959). It is a JSON object keyed by relative path:
  `{"<path>": {"name","size","hash","chunks":[{"hash","offset","size"}],"executable","symlink"}}` (the `done` field is stripped) (L142560-142571).
- Rewritten with `truncate(0)` + `write` **in place**, throttled to one write per 2 s, and forced at clean (L142560-142571). It is **not atomic** [D].
- On init it builds `localFiles` and `chunksLocation` (chunk hash → [(path, offset, size)]). Every local file is first marked in `filesToDelete` (L141960-141994).
- If parsing fails and the dir contains anything besides `manifest.json`/ignored files, then `needRepair = true` (L141994-142004).

### 2.3 Planning (`preprocessFragment`, L142147-142201) [O]
For each manifest file of each selected fragment:
- remove it from `filesToDelete`;
- symlink → recreate if the target differs;
- executable flag differs → chmod task;
- **`local.hash === manifest.hash` ⇒ skip** (it trusts `manifest.json` and does not check the disk);
- otherwise, for each chunk: if the chunk already exists at the same path+offset → done. If the hash exists anywhere locally → it is reused from the local copy
  (it is read and SHA-1 checked, L142221-142237). Otherwise → `chunksToDL[hash]`.
- `removeDeletedFiles` (L142509-142531) deletes everything that is still in `filesToDelete`, i.e. files of fragments that are no longer selected,
  and it keeps any chunks still needed. Empty dirs are then removed.

### 2.4 Fetch (`processFragment` → `manageBundle` → `downloadBundles`) [O]
- For each bundle, keep only the chunks in `chunksToDL` (L142213-142218).
- If the bundle is larger than the free memory/speed budget, split it (`splitBundleToSize`, L142778-142786).
- Split into requests so the `Range` header is ≤ 6144 chars (L142759-142776). Header: `Range: bytes=a-b, c-d, …` (L142790-142797).
- The response is either `multipart/byteranges` (streamed parser L142799-142932) or a single body. For a single body: if there is 1 chunk, the whole body is that chunk;
  otherwise it is sliced by bundle offsets. **Note:** the slicing assumes a full 200 body (L142324-142331) [D].
  - [O] CDN check: a 2-range request returned `206 multipart/byteranges; boundary=…` with `Content-Range: bytes 0-2350/7879` (`samples/bundle-dofus3-config/multirange.*`).
    A 1-range request returned a plain `206`.
- **Every chunk is SHA-1 checked on receive** (L142277-142283). On mismatch it throws. The task retries `retry:3` (L142269-142271, L142987-142997).
- Download queue (L142965-143079): `maxPending 8`. A task still running after 1 s reduces concurrency (min 2), and it climbs back by 1 every 5 s.
  `dlSpeed` is a cap on **bytes in flight per ~1 s window**, not a true rate limiter. The RAM gate is `chunksInMemory + pending + size < maxRAM` (L141907-141915).
- Process queue: concurrency 2. Per-file id lock (`runningIdentifiers`) prevents two writers on one file (L143081-143149).

### 2.5 Write (`processFile`, L142333-142420; `writeChunksDataToFile`, L142477-142503) [O]
- **Writes in place into the final file** (`open r+`, or `w+` with mode 0o744/0o644). There is **no temp file and no rename** [O].
- Contiguous chunks are coalesced with `writev` up to `maxSliceSize` 2 MiB. Holes are padded with zero buffers.
- Before a region is overwritten, chunks still needed elsewhere are pulled into RAM (`saveChunk`).
- After the last chunk: `truncate(size)`, close, then **SHA-1 of the whole file**. On mismatch it throws `File(x) error Hash not corresponding` (L142405-142419).
- After each slice, `localFiles` is updated and `manifest.json` is saved (throttled). [D] This gives **chunk-granular resume**: the next run trusts the recorded chunks.

### 2.6 End / record [O]
- End is reached when the queues are idle, `nbByteProcessed ≥ nbByteToProcess` and the file counts match (L141928-141950).
- `onUpdateCompleted` (L117301-117316): `installedFragments = configuration.fragments`, `version = currentUpdate.version`,
  `isDirty=false`, licenses, `release.json` saved.
- `%APPDATA%\zaap\repositories\production\dofus\{release}\release.json` (L34519-34540; plain `writeFile`, non-atomic) stores:
  `{id,gameUid,gameId,gameOrder,gameName,name,location,version,repositoryVersion,installedFragments,isInstalling,isUpdating,isRepairing,isMoving,isDirty,_schemaVersion:1}`.
  The same dir has `information.json` (`{_schemaVersion:1, version:<assetsHash>}`, L110224-110232), `data/` (assets) and `settings.json` (game settings, L35584).
  During an update `release.json` is re-saved every 2 s (L117257-117259).

### 2.7 Disk space [O]
`hasEnoughSpaceToInstall` = Σ `.meta.totalSize` over the selected fragments (+configuration) `<` drive free space (L35386-35415).
`update()` and `repair()` use the **full installation size, not the delta** (`checkSpace`, L35123-35135). An unknown or NaN drive size → allowed.

---

## 3. Repair, uninstall, move

- **Repair** (L35156-35173, `forceRepair`): ignores `manifest.json`. Walks the dir and marks every file for deletion (L142006-142020).
  For each manifest file: `lstat` + **full-file SHA-1**. If it matches, the file is recorded as valid (L142454-142476). Then the normal plan/download runs.
  Anything not in the selected fragments is deleted, **including any user files placed in the game dir** (except the 2 ignored files) [D].
- Triggers [O]: user action. `isDirty` → the renderer's Play/Update button dispatches `repairRelease` (renderer bundle, `handleUpdate`).
  `onUpdateError` with `LocalHashesError` → auto repair (L117292-117297).
  Install into a dir whose `.release.infos.json` matches → REPAIR instead of INSTALL (L34995-35000).
- **isDirty is set by** [O]: cancel/stop (L117270-117277), any update error (L117278-117299), a config load failure (L34446), and at startup if
  `release.json` still says `isRepairing`/`isUpdating`/(`isInstalling` && location) (L34292-34297) or `migration.launcher.json ≠ {launcher:1}` (L34298-34307).
- **Uninstall** (L35226-35270): refuses while tracked instances are running. Cancels the update, runs the `pre_uninstall` hook, does `rm -rf location`
  (EBUSY → `isOpenedByExternalProcess`), then resets `release.json`.
  The full launcher uninstaller kills the game by process name first (`tasklist`, L137248-137299). This is the only place that checks processes by name.
- **Move** (L35016-35122): refuses if not installed, moving, same folder, updating, or running. Checks permissions and "installable".
  Space check: same drive → the largest single file; different drive → the folder size. Then `fs-extra.move(overwrite)`.
  `isMoving` is persisted, so an interrupted move resumes at the next start (L34308-34315).

---

## 4. Concurrency, locking, and likely "update conflict" causes

Mechanisms [O]:
- Single launcher instance: `requestSingleInstanceLock` (L1600-1607). There is **no on-disk lock** on the game dir, `manifest.json` or `release.json`.
- Global `updateQueue` (L15993-16207): only one update runs at a time. Others wait; pause/resume/stash.
  **The whole queue is paused while the launcher self-updater is busy (`!hasChecked || isDownloading || updateAvailable`) or offline** (L16074-16079, L8696-8698).
- "Running" = `runningInstances`, i.e. only children spawned by *this* launcher process in *this* session (L34605-34607, L34807).
- A new repository version during install/update/repair → cancel and restart (L117133-117197). If the game is running with auto-upgrade, `updateOnExit` is set (L117187-117196, L34796-34806).
- App quit → `pause()` and then `stop()` on every update (L16009-16014).

Likely causes of what users report [I unless marked]:
1. **Quitting or crashing mid-update forces a full repair.** `stop` → `isDirty=true` [O L117270-117277]. A crash leaves `isUpdating:true` in `release.json` → dirty [O].
   A dirty release makes the Play/Update button run a **forced repair = SHA-1 of every file (≈ 15-35 GB for Dofus 3)**. This happens even though chunk-level resume data exists. This is the main source of "lag" and long "verifying".
2. **Non-atomic state files.** `manifest.json` is rewritten with truncate+write every 2 s, and `release.json` with plain writeFile every 2 s [O].
   A crash or power loss in that window leaves a corrupt file → `needRepair` / lost state [D].
3. **Updating while the game runs.** A game started outside this launcher session, or still running after a launcher restart, is invisible [O].
   The in-place `r+` writes then hit EBUSY on Windows (→ error → dirty) or overwrite files the game has mapped [D].
4. **Trusting `manifest.json` over the disk.** Files that were changed or deleted outside the launcher (AV quarantine, user mods) are skipped as up to date [O L142165-142172].
   The result is a "broken game" until a manual repair.
5. **Game update blocked by the launcher self-update.** If a Zaap update is available but not installed, the queue stays paused. Meanwhile `canStart` refuses `UPDATE_AVAILABLE` [O].
6. **Space check uses the full size**, so updates fail with "not enough space" even when the delta is small [O].
7. **Restart storms.** Each new `cytrus.json` version restarts the running update (cancel → `isDirty`). With staged pushes this can repeat [O/I].
8. **Error-path leaks.** If `instantiateAndInitUpdate` swallows an error and returns `undefined`, `createUpdate` then crashes on `a.on` after starting the 2 s save interval and with flags set (L117245-117262, L117362-117364).
   The autoUpgrade path only logs it (L117191-117195), so `isUpdating` can stay `true` → dirty at the next start [D].
9. A second tool (another launcher, or a second Zaap profile/`internal` build) writing the same dir: there is no lock, only the `.release.infos.json` identity check [O].
10. `cytrus.json` is served by a multi-CDN (CloudFront + CDN77 headers in the sample). Polling relies on `Last-Modified` ordering, so an edge serving older data causes delays [I].

---

## 5. Pre-launch checks (`canStart`, L34819-34830; `start`, L34698-34811)
[O] Checked in this order:
- installed (`version` set);
- no update running;
- `!isUpdateAvailable`, i.e. every configured fragment is in `installedFragments` **and** `repositoryVersion === version` (L34592-34603);
- `maxInstances` (Dofus: `true` = unlimited);
- not moving;
- teasing access.

Then:
- the logs path is created;
- the optional `checkConfiguration` script from zaap.yml runs and must exit 0 (L100706-100737);
- account/session checks (out of scope);
- the executable exists (or `zaap-start` script mode).

The renderer also hides Play on `information.default.serverBlocked`/`updateBlocked`, and on an OS version below `minimumOsRequired` (L35477-35487; assets `release.json`).

---

## 6. Implementation spec for the replacement launcher

**Resolve**
1. `GET {base}/cytrus.json`. Require `version==6`. Store the ETag/Last-Modified. Poll at most every 2-5 min, or only on demand.
2. `version = games.dofus.platforms.windows[release]` (default release `dofus3` for Dofus 3. [I] `main` currently points at a 2.x build).
3. `GET .meta` (sizes) and `GET .config`. Fetch the config bundle with a Range request and SHA-1 check each chunk. Render `zaap.yml`.
   A full Nunjucks engine is not required: the Dofus 3 template only uses `extends`, `block` and `if game.settings.X` [O]. Then compute the fragment list and de-duplicate it.
4. `GET .manifest` (≤ ~60 MB, no compression). Parse it with FlatBuffers (`docs/cytrus-manifest.fbs`). Cache it keyed by version.

**Plan** (per selected fragment)
5. Local index = your own state DB, written atomically (write tmp → fsync → rename). Keep Zaap's `manifest.json` format if you want to share the dir with the official launcher.
6. Fast path: `(size, mtime)` recorded at the last verified write. If they differ, rehash that file. **Never default to hashing everything.** Offer an explicit "verify" action.
7. Diff by file hash. For changed files, diff by chunk. Reuse local chunks by hash (from any file). Download the rest.

**Fetch**
8. Group the needed chunks by bundle. Build multi-range requests (header ≤ 6 KB; merge adjacent ranges). Handle both `206 multipart/byteranges` and single `206`/`200`.
   SHA-1 check each chunk. Retry with backoff on network errors **and** on 5xx/416. Use 4-8 parallel requests and keep memory bounded.

**Write**
9. Assemble each changed file into `path.cytrus-tmp`, preferably in the same dir. Fill it from local chunks and downloaded chunks, SHA-1 check the whole file, then `rename` over the target (Windows: `MoveFileEx REPLACE_EXISTING`).
   Delete files that are no longer referenced after all writes succeed. Set the version in state **only after** everything is verified.
10. Resume = temp files plus the per-chunk progress in the state DB. An interrupted run is *not* "dirty". Only missing or failed files are redone.

**Safety**
11. Take an exclusive lock file in the install dir (`CreateFile` with no sharing, held for the whole update).
    Before updating, detect running `Dofus.exe` from that dir by image path, using a process snapshot and not only the launcher's own children. Refuse or wait if found.
    [I] Also refuse if Zaap is running and has the same dir open.
12. Space check = bytes to download + the largest temp file overhead, not the full install size.
13. Pre-Play: all selected fragments installed, version equals the remote version (or let the user play offline on a stale version), no lock held, and `Dofus.exe` exists.
    Show the `serverBlocked`/`updateBlocked` flags from the assets `release.json` (`{base}/dofus/hashes/xx/<assets.meta[release]>` tar).
14. Keep `.release.infos.json` (`{"gameUid":"dofus","release":"dofus3"}`) and `migration.launcher.json` (`{"launcher":1}`) if you want Zaap to adopt the dir without a forced repair (L34298-34307, L6566-6582).

---

## 7. Unknowns / not verified
- [U] Whether the full main/dofus3 `.manifest` files have any `symlink` or `executable` entries. Only 4/12 dofus3 fragments were probed, and the full 51 MB file was not downloaded.
- [U] The chunking algorithm and its parameters (not needed for downloading).
- [U] Bundle-hash derivation was confirmed on 2 bundles only.
- [U] CDN behaviour with very many ranges, or with ranges that are not in order (the CDN might coalesce them or return 200). The client code tolerates a 200 full body.
- [U] Older (pre-v6) Cytrus: the bundle contains no v5 code path. Non-`6.*` versions are rejected (`CYTRUS_VERSION_NOT_HANDLED` 5020, L52410).
- [U] Renderer-side retry/repair UX beyond `handleUpdate`, which was only skimmed.
- None of this was verified against a real install dir (`%APPDATA%\zaap\…`, `manifest.json`). The formats above come from code only.
