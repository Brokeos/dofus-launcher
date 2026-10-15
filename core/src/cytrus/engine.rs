//! Install / update / repair engine. See the module docs in `mod.rs` for the semantics.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures::TryStreamExt;
use sha1::Digest;
use tokio_util::sync::CancellationToken;

use super::download::{self, ChunkSink, FetchCtx, RateLimiter, WantedChunk};
use super::manifest::{self, Hash, Manifest};
use super::state::{self, Sidecar, Stamp, TMP_SUFFIX, ZaapFile, ZaapManifest};
use super::{CytrusError, Phase, Progress, UpdateOptions, normalize_fragments, remote};

const DEFAULT_CONCURRENCY: usize = 6;
const STATE_SAVE_INTERVAL: Duration = Duration::from_secs(30);
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

// --- public entry points ----------------------------------------------------------------------

/// Install, update, or repair (`verify_all`) the selected fragments of `opts.release` into
/// `opts.install_dir`. Holds `<install>/.dofus-launcher.lock` for the whole run. Cancellation leaves a
/// resumable state (verified files are recorded; `.dl-tmp` files are resumed chunk-by-chunk next run).
pub async fn run_update(
    opts: UpdateOptions,
    cancel: CancellationToken,
    on_progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<()> {
    let progress = Arc::new(ProgressState::new(Box::new(on_progress)));
    let ticker = {
        let p = progress.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                p.tick(false);
            }
        })
    };
    let res = run_update_inner(&opts, &cancel, &progress).await;
    ticker.abort();
    let _ = ticker.await;
    if res.is_ok() {
        progress.set_phase(Phase::Done);
        progress.tick(true);
    }
    res
}

/// Download only the `configuration` fragment (zaap.yml…) of `release`/`version` into `dest`.
/// Files already present with the right hash are kept. No lock, no state files.
pub async fn fetch_configuration(release: &str, version: &str, dest: &Path) -> Result<()> {
    let manifest = Arc::new(remote::fetch_config_manifest(release, version).await?);
    tokio::fs::create_dir_all(dest).await?;
    let fragments = vec!["configuration".to_string()];
    let (targets, fragments) = collect_targets(&manifest, &fragments, dest)?;
    let job = Job {
        root: dest.to_path_buf(),
        manifest,
        fragments,
        targets,
        verify_all: false,
        concurrency: 2,
        speed_limit: None,
        persist: None,
    };
    let progress = Arc::new(ProgressState::new(Box::new(|_| {})));
    sync(Arc::new(job), CancellationToken::new(), progress).await
}

async fn run_update_inner(opts: &UpdateOptions, cancel: &CancellationToken, progress: &Arc<ProgressState>) -> Result<()> {
    progress.set_phase(Phase::Preparing);
    let root = opts.install_dir.clone();
    tokio::fs::create_dir_all(&root).await.with_context(|| format!("create {}", root.display()))?;
    let lock = InstallLock::acquire(&root)?;
    {
        let r = root.clone();
        let pids = tokio::task::spawn_blocking(move || running_pids_under(&r)).await?;
        if !pids.is_empty() {
            return Err(CytrusError::GameRunning(pids).into());
        }
    }
    let repo = remote::fetch_repo().await?;
    let version = remote::release_version(&repo, &opts.release)?;
    let cache_dir = crate::paths::data_dir().join("cache");
    let manifest = tokio::select! {
        _ = cancel.cancelled() => return Err(CytrusError::Cancelled.into()),
        m = remote::fetch_manifest(&opts.release, &version, &cache_dir) => Arc::new(m?),
    };
    state::write_json_if_changed(
        &root.join(state::RELEASE_INFOS),
        &serde_json::json!({"gameUid": crate::DOFUS_GAME_UID, "release": opts.release}),
    )?;
    state::write_json_if_changed(&root.join(state::MIGRATION), &serde_json::json!({"launcher": 1}))?;

    let wanted = normalize_fragments(&opts.fragments);
    let (targets, fragments) = collect_targets(&manifest, &wanted, &root)?;
    let (old_zaap, old_sidecar) = {
        let r = root.clone();
        tokio::task::spawn_blocking(move || (state::read_zaap_manifest(&r), state::read_sidecar(&r))).await?
    };
    let job = Job {
        root,
        manifest,
        fragments,
        targets,
        verify_all: opts.verify_all,
        concurrency: if opts.max_concurrency == 0 { DEFAULT_CONCURRENCY } else { opts.max_concurrency.min(32) },
        speed_limit: opts.speed_limit_bps.filter(|b| *b > 0),
        persist: Some(Persist { release: opts.release.clone(), version, old_zaap, old_sidecar }),
    };
    let res = sync(Arc::new(job), cancel.clone(), progress.clone()).await;
    drop(lock);
    res
}

// --- job description --------------------------------------------------------------------------

struct Persist {
    release: String,
    version: String,
    old_zaap: ZaapManifest,
    old_sidecar: Sidecar,
}

struct Job {
    root: PathBuf,
    manifest: Arc<Manifest>,
    /// Fragments actually found in the manifest (normalized order).
    fragments: Vec<String>,
    targets: Vec<Target>,
    verify_all: bool,
    concurrency: usize,
    speed_limit: Option<u64>,
    /// None: plain download into a directory (no state files, no cleanup).
    persist: Option<Persist>,
}

struct Target {
    rel: String,
    abs: PathBuf,
    frag: usize,
    file: usize,
}

impl Job {
    fn file(&self, t: &Target) -> &manifest::File {
        &self.manifest.fragments[t.frag].files[t.file]
    }
    fn sidecar(&self) -> Option<&Sidecar> {
        self.persist.as_ref().map(|p| &p.old_sidecar)
    }
}

/// Validate a manifest path and turn it into a relative OS path (no `..`, no absolute, no drive/ADS).
pub(crate) fn safe_rel(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.starts_with('/') || name.contains('\\') || name.contains(':') || name.contains('\0') {
        bail!("unsafe path in manifest: {name:?}");
    }
    let mut p = PathBuf::new();
    for comp in name.split('/') {
        match comp {
            "" | "." => continue,
            ".." => bail!("unsafe path in manifest: {name:?}"),
            c => p.push(c),
        }
    }
    if p.as_os_str().is_empty() {
        bail!("empty path in manifest");
    }
    Ok(p)
}

fn path_key(rel: &str) -> String {
    if cfg!(windows) { rel.to_lowercase() } else { rel.to_string() }
}

fn collect_targets(manifest: &Manifest, wanted: &[String], root: &Path) -> Result<(Vec<Target>, Vec<String>)> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut used = Vec::new();
    for name in wanted {
        let Some(fi) = manifest.fragments.iter().position(|f| &f.name == name) else {
            tracing::warn!("fragment `{name}` not in manifest; skipped");
            continue;
        };
        used.push(name.clone());
        for (i, f) in manifest.fragments[fi].files.iter().enumerate() {
            let rel = safe_rel(&f.name)?;
            if state::is_protected(&f.name) {
                bail!("manifest file {:?} collides with a launcher state file", f.name);
            }
            if seen.insert(path_key(&f.name)) {
                targets.push(Target { rel: f.name.clone(), abs: root.join(rel), frag: fi, file: i });
            }
        }
    }
    Ok((targets, used))
}

fn tmp_path(abs: &Path) -> PathBuf {
    let mut s = abs.as_os_str().to_os_string();
    s.push(TMP_SUFFIX);
    PathBuf::from(s)
}

// --- the pipeline -----------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Check {
    Ok(Stamp),
    Build,
    Symlink,
}

struct RunState {
    zaap: ZaapManifest,
    stamps: BTreeMap<String, Stamp>,
    dirty: bool,
}

async fn sync(job: Arc<Job>, cancel: CancellationToken, progress: Arc<ProgressState>) -> Result<()> {
    // 1. Check local files.
    progress.set_phase(Phase::Checking);
    progress.set_files(0, job.targets.len() as u64);
    let checks = {
        let (job, cancel, progress) = (job.clone(), cancel.clone(), progress.clone());
        tokio::task::spawn_blocking(move || check_all(&job, &cancel, &progress)).await??
    };

    // 2. Initial run state: previous entries of non-target files (kept until cleanup) + verified targets.
    let target_keys: HashSet<String> = job.targets.iter().map(|t| path_key(&t.rel)).collect();
    let mut rs = RunState { zaap: ZaapManifest::new(), stamps: BTreeMap::new(), dirty: true };
    if let Some(p) = &job.persist {
        for (k, v) in &p.old_zaap {
            if !target_keys.contains(&path_key(k)) {
                rs.zaap.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &p.old_sidecar.files {
            if !target_keys.contains(&path_key(k)) {
                rs.stamps.insert(k.clone(), *v);
            }
        }
    }
    for (t, c) in job.targets.iter().zip(&checks) {
        if let Check::Ok(s) = c {
            rs.zaap.insert(t.rel.clone(), ZaapFile::from_manifest(job.file(t)));
            rs.stamps.insert(t.rel.clone(), *s);
        }
    }
    let rs = Arc::new(Mutex::new(rs));

    let result = sync_inner(&job, &checks, &rs, &cancel, &progress).await;

    // Always record what is verified so far (resume after cancel/error is cheap).
    if let Some(p) = &job.persist {
        let (job2, rs2) = (job.clone(), rs.clone());
        let ok = result.is_ok();
        let release = p.release.clone();
        let save = tokio::task::spawn_blocking(move || -> Result<()> {
            let rs = rs2.lock().unwrap();
            let p = job2.persist.as_ref().unwrap();
            if !ok {
                state::write_zaap_manifest(&job2.root, &rs.zaap)?;
                let mut sc = p.old_sidecar.clone();
                sc.release = Some(release);
                sc.files = rs.stamps.clone();
                return state::write_sidecar(&job2.root, &sc);
            }
            Ok(())
        })
        .await?;
        if let Err(e) = save {
            tracing::error!("cannot save install state: {e:#}");
            if result.is_ok() {
                return Err(e);
            }
        }
    }
    result?;

    // Success: cleanup then final state.
    if job.persist.is_some() {
        progress.set_phase(Phase::Finalizing);
        let (job2, rs2) = (job.clone(), rs.clone());
        tokio::task::spawn_blocking(move || finish(&job2, &rs2)).await??;
    }
    Ok(())
}

async fn sync_inner(
    job: &Arc<Job>,
    checks: &[Check],
    rs: &Arc<Mutex<RunState>>,
    cancel: &CancellationToken,
    progress: &Arc<ProgressState>,
) -> Result<()> {
    // Chunk sources: stable verified targets first, then any previously known local file.
    let index = Arc::new(build_source_index(job, checks));
    let builds: Vec<BuildFile> = job
        .targets
        .iter()
        .enumerate()
        .filter(|(i, _)| matches!(checks[*i], Check::Build))
        .map(|(i, t)| {
            let f = job.file(t);
            BuildFile {
                target: i,
                tmp: tmp_path(&t.abs),
                size: f.size,
                hash: f.hash,
                remaining: AtomicUsize::new(0),
                finalized: AtomicBool::new(false),
            }
        })
        .collect();

    // Bundle locations of the chunks that may need downloading.
    let bundle_index = BundleIndex::new(&job.manifest, &job.fragments);

    // 3. Space check: bytes to download (before local reuse) + largest rebuilt file.
    {
        let mut need: HashSet<Hash> = HashSet::new();
        let mut bytes = 0u64;
        let mut largest = 0u64;
        for b in &builds {
            let f = job.file(&job.targets[b.target]);
            largest = largest.max(f.size);
            for c in f.effective_chunks().iter() {
                if c.size > 0 && !index.contains_key(&c.hash) && need.insert(c.hash) {
                    bytes += c.size;
                }
            }
        }
        if !builds.is_empty() {
            let needed = bytes + largest + SPACE_MARGIN;
            if let Ok(available) = fs4::available_space(&job.root)
                && available < needed {
                    return Err(CytrusError::InsufficientSpace { needed, available }.into());
                }
        }
    }

    // 4. Local phase: resume `.dl-tmp`, copy reusable local chunks, list remote needs.
    progress.set_files(0, builds.len() as u64);
    let needs: Vec<Vec<(Hash, u64)>> = {
        let (job, index, cancel, progress) = (job.clone(), index.clone(), cancel.clone(), progress.clone());
        let builds_meta: Vec<(usize, PathBuf)> = builds.iter().map(|b| (b.target, b.tmp.clone())).collect();
        tokio::task::spawn_blocking(move || -> Result<Vec<Vec<(Hash, u64)>>> {
            let out: Vec<Mutex<Vec<(Hash, u64)>>> = builds_meta.iter().map(|_| Mutex::new(Vec::new())).collect();
            par_for_each(&builds_meta, threads(), &cancel, |i, (ti, tmp), cache: &mut SrcCache| {
                let t = &job.targets[*ti];
                let n = prepare_build(job.file(t), tmp, &index, cache).with_context(|| format!("prepare {}", t.rel))?;
                *out[i].lock().unwrap() = n;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })?;
            Ok(out.into_iter().map(|m| m.into_inner().unwrap()).collect())
        })
        .await??
    };

    let mut dests: HashMap<Hash, Vec<(u32, u64)>> = HashMap::new();
    for (bi, (b, n)) in builds.iter().zip(&needs).enumerate() {
        b.remaining.store(n.len(), Ordering::Relaxed);
        for (h, off) in n {
            dests.entry(*h).or_default().push((bi as u32, *off));
        }
    }
    drop(needs);

    // Group missing chunks by bundle, then into multi-range requests.
    let mut by_bundle: HashMap<Hash, Vec<WantedChunk>> = HashMap::new();
    for h in dests.keys() {
        let (bundle, offset, size) = bundle_index.locate(h).with_context(|| format!("chunk {} is in no bundle", hex::encode(h)))?;
        by_bundle.entry(bundle).or_default().push(WantedChunk { hash: *h, offset, size });
    }
    let mut requests = Vec::new();
    for (bundle, chunks) in by_bundle {
        requests.extend(download::plan_requests(bundle, chunks, download::MAX_RANGE_HEADER, download::MAX_REQUEST_BYTES));
    }
    let bytes_total: u64 = requests.iter().map(|r| r.bytes()).sum();

    let assembler = Arc::new(Assembler {
        job: job.clone(),
        files: builds,
        dests,
        pool: Mutex::new(HashMap::new()),
        rs: rs.clone(),
        progress: progress.clone(),
    });

    progress.set_phase(Phase::Downloading);
    progress.set_files(0, assembler.files.len() as u64);
    progress.bytes_total.store(bytes_total, Ordering::Relaxed);
    progress.delivered.store(0, Ordering::Relaxed);

    // Files fully satisfied locally.
    {
        let a = assembler.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let ready: Vec<usize> = (0..a.files.len()).filter(|i| a.files[*i].remaining.load(Ordering::Relaxed) == 0).collect();
            par_for_each(&ready, threads(), &cancel, |_, &i, _: &mut ()| a.finalize(i))
        })
        .await??;
    }

    // 5. Download.
    if !requests.is_empty() {
        let saver = job.persist.as_ref().map(|_| {
            let (job, rs) = (job.clone(), rs.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(STATE_SAVE_INTERVAL).await;
                    let (job, rs) = (job.clone(), rs.clone());
                    let _ = tokio::task::spawn_blocking(move || save_progress(&job, &rs)).await;
                }
            })
        });
        let inner = cancel.child_token();
        let ctx = FetchCtx {
            client: crate::http::client().clone(),
            cancel: inner.clone(),
            limiter: job.speed_limit.map(|b| Arc::new(RateLimiter::new(b))),
            sink: assembler.clone(),
            received: progress.received.clone(),
            delivered: progress.delivered.clone(),
            multi_range_ok: Arc::new(AtomicBool::new(true)),
            url_for: remote::bundle_url,
        };
        // Bigger requests first: better tail latency.
        requests.sort_by_key(|r| std::cmp::Reverse(r.bytes()));
        let res = futures::stream::iter(requests.into_iter().map(Ok::<_, anyhow::Error>))
            .try_for_each_concurrent(job.concurrency, |r| {
                let ctx = &ctx;
                async move { download::fetch_request(ctx, r).await }
            })
            .await;
        inner.cancel();
        if let Some(s) = saver {
            s.abort();
        }
        if cancel.is_cancelled() {
            return Err(CytrusError::Cancelled.into());
        }
        res?;
    }

    // 6. Every rebuilt file must be finalized.
    let missing: Vec<&str> = assembler
        .files
        .iter()
        .filter(|b| !b.finalized.load(Ordering::Relaxed))
        .map(|b| job.targets[b.target].rel.as_str())
        .collect();
    if !missing.is_empty() {
        bail!("{} file(s) incomplete after download, e.g. {}", missing.len(), missing[0]);
    }

    // 7. Symlinks.
    for (t, c) in job.targets.iter().zip(checks) {
        if let Check::Symlink = c {
            let f = job.file(t);
            make_symlink(&t.abs, f.symlink.as_deref().unwrap_or_default())?;
            rs.lock().unwrap().zaap.insert(t.rel.clone(), ZaapFile::from_manifest(f));
        }
    }
    Ok(())
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8)
}

fn check_all(job: &Job, cancel: &CancellationToken, progress: &ProgressState) -> Result<Vec<Check>> {
    let out: Vec<Mutex<Check>> = job.targets.iter().map(|_| Mutex::new(Check::Build)).collect();
    par_for_each(&job.targets, threads(), cancel, |i, t, _: &mut ()| {
        *out[i].lock().unwrap() = check_target(job, t);
        progress.files_done.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })?;
    Ok(out.into_iter().map(|m| m.into_inner().unwrap()).collect())
}

fn check_target(job: &Job, t: &Target) -> Check {
    let f = job.file(t);
    if f.symlink.is_some() {
        return Check::Symlink;
    }
    let meta = match fs::metadata(&t.abs) {
        Ok(m) if m.is_file() => m,
        _ => return Check::Build,
    };
    if meta.len() != f.size {
        return Check::Build;
    }
    if !job.verify_all
        && let Some(s) = job.sidecar().and_then(|s| s.files.get(&t.rel))
            && s.matches(&meta, &f.hash) {
                return Check::Ok(*s);
            }
    match hash_file(&t.abs) {
        Ok(h) if h == f.hash => Check::Ok(Stamp::of(&meta, h)),
        Ok(_) => Check::Build,
        Err(e) => {
            tracing::warn!("cannot hash {}: {e}", t.abs.display());
            Check::Build
        }
    }
}

/// chunk hash -> (absolute path, offset) of a local copy.
type SourceIndex = HashMap<Hash, (Arc<PathBuf>, u64)>;

fn build_source_index(job: &Job, checks: &[Check]) -> SourceIndex {
    let mut index = SourceIndex::new();
    for (t, c) in job.targets.iter().zip(checks) {
        if let Check::Ok(_) = c {
            let p = Arc::new(t.abs.clone());
            for ch in job.file(t).effective_chunks().iter() {
                index.entry(ch.hash).or_insert_with(|| (p.clone(), ch.offset));
            }
        }
    }
    if let Some(p) = &job.persist {
        for (rel, e) in &p.old_zaap {
            let Ok(r) = safe_rel(rel) else { continue };
            let abs = job.root.join(r);
            match fs::metadata(&abs) {
                Ok(m) if m.is_file() && m.len() == e.size => {}
                _ => continue,
            }
            let p = Arc::new(abs);
            for ch in &e.chunks {
                let Ok(h) = hex::decode(&ch.hash) else { continue };
                let Ok(h) = <Hash>::try_from(h.as_slice()) else { continue };
                if ch.size > 0 && ch.offset + ch.size <= e.size {
                    index.entry(h).or_insert_with(|| (p.clone(), ch.offset));
                }
            }
        }
    }
    index
}

/// Per-thread cache of the last opened source file.
#[derive(Default)]
struct SrcCache {
    path: Option<Arc<PathBuf>>,
    file: Option<fs::File>,
    buf: Vec<u8>,
}

fn prepare_build(f: &manifest::File, tmp: &Path, index: &SourceIndex, cache: &mut SrcCache) -> Result<Vec<(Hash, u64)>> {
    if let Some(dir) = tmp.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let existing = match fs::metadata(tmp) {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => bail!("{} exists and is not a file", tmp.display()),
        Err(_) => 0,
    };
    let out = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(tmp)
        .with_context(|| format!("open {}", tmp.display()))?;
    if existing > f.size {
        out.set_len(f.size)?;
    }
    let mut needs = Vec::new();
    for c in f.effective_chunks().iter() {
        if c.size == 0 {
            continue;
        }
        let n = c.size as usize;
        cache.buf.resize(n, 0);
        // Resume: chunk already present (and verified) in the temp file.
        if existing >= c.offset + c.size && read_exact_at(&out, &mut cache.buf, c.offset).is_ok() && sha1_of(&cache.buf) == c.hash {
            continue;
        }
        // Reuse from a local file (verified).
        if let Some((src, off)) = index.get(&c.hash) {
            if cache.path.as_ref() != Some(src) {
                cache.file = fs::File::open(src.as_path()).ok();
                cache.path = Some(src.clone());
            }
            if let Some(sf) = &cache.file
                && read_exact_at(sf, &mut cache.buf, *off).is_ok() && sha1_of(&cache.buf) == c.hash {
                    write_all_at(&out, &cache.buf, c.offset).with_context(|| format!("write {}", tmp.display()))?;
                    continue;
                }
        }
        needs.push((c.hash, c.offset));
    }
    if cache.buf.capacity() > 4 * 1024 * 1024 {
        cache.buf = Vec::new();
    }
    Ok(needs)
}

struct BuildFile {
    target: usize,
    tmp: PathBuf,
    size: u64,
    hash: Hash,
    /// Remote chunk positions not written yet.
    remaining: AtomicUsize,
    finalized: AtomicBool,
}

struct Assembler {
    job: Arc<Job>,
    files: Vec<BuildFile>,
    dests: HashMap<Hash, Vec<(u32, u64)>>,
    pool: Mutex<HashMap<u32, Arc<fs::File>>>,
    rs: Arc<Mutex<RunState>>,
    progress: Arc<ProgressState>,
}

impl Assembler {
    fn handle(&self, fi: u32) -> Result<Arc<fs::File>> {
        let mut pool = self.pool.lock().unwrap();
        if let Some(h) = pool.get(&fi) {
            return Ok(h.clone());
        }
        let b = &self.files[fi as usize];
        let f = fs::OpenOptions::new().write(true).create(true).truncate(false).open(&b.tmp)
            .with_context(|| format!("open {}", b.tmp.display()))?;
        if pool.len() >= 128 {
            pool.clear();
        }
        let f = Arc::new(f);
        pool.insert(fi, f.clone());
        Ok(f)
    }

    /// Verify the assembled temp file and move it over the target.
    fn finalize(&self, i: usize) -> Result<()> {
        self.pool.lock().unwrap().remove(&(i as u32));
        let b = &self.files[i];
        let t = &self.job.targets[b.target];
        let f = self.job.file(t);
        {
            let tf = fs::OpenOptions::new().write(true).create(true).truncate(false).open(&b.tmp)?;
            if tf.metadata()?.len() != b.size {
                tf.set_len(b.size)?;
            }
        }
        let h = hash_file(&b.tmp).with_context(|| format!("hash {}", b.tmp.display()))?;
        if h != b.hash {
            let _ = fs::remove_file(&b.tmp);
            bail!("{}: SHA-1 mismatch after assembly (expected {}, got {})", t.rel, hex::encode(b.hash), hex::encode(h));
        }
        set_executable(&b.tmp, f.executable);
        replace_file(&b.tmp, &t.abs).with_context(|| format!("replace {}", t.abs.display()))?;
        let meta = fs::metadata(&t.abs)?;
        {
            let mut rs = self.rs.lock().unwrap();
            rs.zaap.insert(t.rel.clone(), ZaapFile::from_manifest(f));
            rs.stamps.insert(t.rel.clone(), Stamp::of(&meta, b.hash));
            rs.dirty = true;
        }
        b.finalized.store(true, Ordering::Relaxed);
        self.progress.files_done.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl ChunkSink for Assembler {
    fn write(&self, hash: &Hash, data: &[u8]) -> Result<()> {
        let Some(dests) = self.dests.get(hash) else { return Ok(()) };
        for &(fi, off) in dests {
            let b = &self.files[fi as usize];
            if b.finalized.load(Ordering::Relaxed) {
                continue;
            }
            let h = self.handle(fi)?;
            write_all_at(&h, data, off).with_context(|| format!("write {}", b.tmp.display()))?;
            drop(h);
            if b.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.finalize(fi as usize)?;
            }
        }
        Ok(())
    }
}

struct BundleIndex<'m> {
    manifest: &'m Manifest,
    selected: HashMap<Hash, (Hash, u64, u64)>,
    all: std::sync::OnceLock<HashMap<Hash, (Hash, u64, u64)>>,
}

impl<'m> BundleIndex<'m> {
    fn new(manifest: &'m Manifest, fragments: &[String]) -> Self {
        let mut selected = HashMap::new();
        for f in manifest.fragments.iter().filter(|f| fragments.contains(&f.name)) {
            for b in &f.bundles {
                for c in &b.chunks {
                    selected.entry(c.hash).or_insert((b.hash, c.offset, c.size));
                }
            }
        }
        BundleIndex { manifest, selected, all: std::sync::OnceLock::new() }
    }
    fn locate(&self, h: &Hash) -> Option<(Hash, u64, u64)> {
        if let Some(v) = self.selected.get(h) {
            return Some(*v);
        }
        let all = self.all.get_or_init(|| {
            let mut m = HashMap::new();
            for f in &self.manifest.fragments {
                for b in &f.bundles {
                    for c in &b.chunks {
                        m.entry(c.hash).or_insert((b.hash, c.offset, c.size));
                    }
                }
            }
            m
        });
        all.get(h).copied()
    }
}

fn save_progress(job: &Job, rs: &Mutex<RunState>) {
    let Some(p) = &job.persist else { return };
    let mut rs = rs.lock().unwrap();
    if !rs.dirty {
        return;
    }
    let mut sc = p.old_sidecar.clone();
    sc.release = Some(p.release.clone());
    sc.files = rs.stamps.clone();
    let r = state::write_zaap_manifest(&job.root, &rs.zaap).and_then(|_| state::write_sidecar(&job.root, &sc));
    match r {
        Ok(()) => rs.dirty = false,
        Err(e) => tracing::warn!("periodic state save failed: {e:#}"),
    }
}

/// After a fully verified run: delete files that left the selection, write the final state.
fn finish(job: &Job, rs: &Mutex<RunState>) -> Result<()> {
    let p = job.persist.as_ref().unwrap();
    let target_keys: HashSet<String> = job.targets.iter().map(|t| path_key(&t.rel)).collect();
    let mut candidates: BTreeMap<String, ()> = BTreeMap::new();
    for k in p.old_zaap.keys().chain(p.old_sidecar.files.keys()) {
        if !target_keys.contains(&path_key(k)) && !state::is_protected(k) {
            candidates.insert(k.clone(), ());
        }
    }
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for rel in candidates.keys() {
        let Ok(r) = safe_rel(rel) else {
            tracing::warn!("not deleting unsafe path {rel:?}");
            continue;
        };
        let abs = job.root.join(&r);
        for path in [abs.clone(), tmp_path(&abs)] {
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() || m.file_type().is_symlink() => {
                    if let Err(e) = remove_file_force(&path) {
                        tracing::warn!("cannot delete {}: {e}", path.display());
                    }
                }
                _ => {}
            }
        }
        if let Some(d) = abs.parent() {
            dirs.insert(d.to_path_buf());
        }
    }
    // Remove directories emptied by the cleanup (deepest first, never the root).
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let mut cur = d;
        while cur.starts_with(&job.root) && cur != job.root {
            if fs::remove_dir(&cur).is_err() {
                break;
            }
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
    }

    let rs = rs.lock().unwrap();
    let mut zaap = ZaapManifest::new();
    let mut stamps = BTreeMap::new();
    for t in &job.targets {
        zaap.insert(t.rel.clone(), ZaapFile::from_manifest(job.file(t)));
        if let Some(s) = rs.stamps.get(&t.rel) {
            stamps.insert(t.rel.clone(), *s);
        }
    }
    state::write_zaap_manifest(&job.root, &zaap)?;
    let sc = Sidecar {
        schema: 1,
        release: Some(p.release.clone()),
        installed_version: Some(p.version.clone()),
        installed_fragments: job.fragments.clone(),
        files: stamps,
    };
    state::write_sidecar(&job.root, &sc)
}

// --- filesystem helpers -----------------------------------------------------------------------

fn sha1_of(b: &[u8]) -> Hash {
    sha1::Sha1::digest(b).into()
}

pub(crate) fn hash_file(p: &Path) -> io::Result<Hash> {
    let mut f = fs::File::open(p)?;
    let mut h = sha1::Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

#[cfg(windows)]
fn read_exact_at(f: &fs::File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(f: &fs::File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(f: &fs::File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}

#[cfg(unix)]
fn write_all_at(f: &fs::File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}

fn clear_readonly(p: &Path) {
    if let Ok(m) = fs::metadata(p) {
        let mut perm = m.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = fs::set_permissions(p, perm);
        }
    }
}

/// Rename `tmp` over `dst` (MoveFileEx REPLACE_EXISTING on Windows), retrying transient failures
/// (antivirus scans, read-only attribute).
fn replace_file(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..6u64 {
        match fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    clear_readonly(dst);
                }
                if fs::metadata(dst).map(|m| m.is_dir()).unwrap_or(false) {
                    return Err(io::Error::other(format!("{} is a directory", dst.display())));
                }
                last = Some(e);
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    Err(last.unwrap())
}

fn remove_file_force(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            clear_readonly(p);
            fs::remove_file(p)
        }
        r => r,
    }
}

#[cfg(unix)]
fn set_executable(p: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }));
}

#[cfg(not(unix))]
fn set_executable(_: &Path, _: bool) {}

fn make_symlink(abs: &Path, target: &str) -> Result<()> {
    #[cfg(unix)]
    {
        if let Some(d) = abs.parent() {
            fs::create_dir_all(d)?;
        }
        if fs::read_link(abs).ok().as_deref() == Some(Path::new(target)) {
            return Ok(());
        }
        let _ = fs::remove_file(abs);
        std::os::unix::fs::symlink(target, abs).with_context(|| format!("symlink {}", abs.display()))?;
    }
    #[cfg(not(unix))]
    tracing::warn!("symlink {} -> {target} skipped on this platform", abs.display());
    Ok(())
}

/// Run `f` over `items` on `threads` OS threads; stops at the first error or on cancellation.
fn par_for_each<T: Sync, S: Default>(
    items: &[T],
    threads: usize,
    cancel: &CancellationToken,
    f: impl Fn(usize, &T, &mut S) -> Result<()> + Sync,
) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let err: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.min(items.len()).max(1) {
            s.spawn(|| {
                let mut st = S::default();
                loop {
                    if cancel.is_cancelled() || failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    if let Err(e) = f(i, &items[i], &mut st) {
                        failed.store(true, Ordering::Relaxed);
                        err.lock().unwrap().get_or_insert(e);
                        break;
                    }
                }
            });
        }
    });
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }
    if cancel.is_cancelled() {
        return Err(CytrusError::Cancelled.into());
    }
    Ok(())
}

// --- lock & running game ----------------------------------------------------------------------

struct InstallLock {
    _file: fs::File,
}

impl InstallLock {
    fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(state::LOCK_FILE);
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(InstallLock { _file: file }),
            Err(fs4::TryLockError::WouldBlock) => Err(CytrusError::Locked.into()),
            Err(fs4::TryLockError::Error(e)) => Err(anyhow!(e).context(format!("lock {}", path.display()))),
        }
    }
}

fn normalize_for_prefix(p: &Path) -> String {
    let p = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut s = p.to_string_lossy().replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    } else {
        s = p.to_string_lossy().into_owned();
    }
    s
}

/// PIDs of processes whose executable image lives under `root` (rule 5).
pub(crate) fn running_pids_under(root: &Path) -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let mut prefix = normalize_for_prefix(root);
    if !prefix.ends_with(sep) {
        prefix.push(sep);
    }
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet));
    let me = std::process::id();
    sys.processes()
        .iter()
        .filter(|(pid, _)| pid.as_u32() != me)
        .filter_map(|(pid, p)| {
            let exe = p.exe()?;
            let mut s = exe.to_string_lossy().into_owned();
            if cfg!(windows) {
                s = s.replace('/', "\\").to_lowercase();
                if let Some(rest) = s.strip_prefix(r"\\?\") {
                    s = rest.to_string();
                }
            }
            s.starts_with(&prefix).then_some(pid.as_u32())
        })
        .collect()
}

// --- progress ---------------------------------------------------------------------------------

type ProgressCb = Box<dyn Fn(Progress) + Send + Sync>;

struct ProgressState {
    cb: ProgressCb,
    phase: Mutex<Phase>,
    files_done: AtomicU64,
    files_total: AtomicU64,
    bytes_total: AtomicU64,
    delivered: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
    emit: Mutex<EmitState>,
}

struct EmitState {
    last: Option<Progress>,
    last_t: Instant,
    last_recv: u64,
    speed: f64,
}

impl ProgressState {
    fn new(cb: ProgressCb) -> Self {
        ProgressState {
            cb,
            phase: Mutex::new(Phase::Preparing),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            delivered: Arc::new(AtomicU64::new(0)),
            received: Arc::new(AtomicU64::new(0)),
            emit: Mutex::new(EmitState { last: None, last_t: Instant::now(), last_recv: 0, speed: 0.0 }),
        }
    }
    fn set_phase(&self, p: Phase) {
        *self.phase.lock().unwrap() = p;
    }
    fn set_files(&self, done: u64, total: u64) {
        self.files_done.store(done, Ordering::Relaxed);
        self.files_total.store(total, Ordering::Relaxed);
    }
    /// Called at most every 250 ms by the ticker (and once, forced, at the end).
    fn tick(&self, force: bool) {
        let mut e = self.emit.lock().unwrap();
        let now = Instant::now();
        let recv = self.received.load(Ordering::Relaxed);
        let dt = now.duration_since(e.last_t).as_secs_f64();
        if dt > 0.0 {
            let inst = (recv - e.last_recv) as f64 / dt;
            e.speed = if e.speed == 0.0 { inst } else { 0.7 * e.speed + 0.3 * inst };
            e.last_t = now;
            e.last_recv = recv;
        }
        let phase = *self.phase.lock().unwrap();
        let p = Progress {
            phase,
            bytes_done: self.delivered.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            files_done: self.files_done.load(Ordering::Relaxed),
            files_total: self.files_total.load(Ordering::Relaxed),
            speed_bps: if phase == Phase::Downloading { e.speed as u64 } else { 0 },
        };
        if force || e.last.as_ref() != Some(&p) {
            e.last = Some(p.clone());
            drop(e);
            (self.cb)(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cytrus::manifest::{Bundle, Chunk, File, Fragment};

    fn sha(b: &[u8]) -> Hash {
        sha1_of(b)
    }

    /// Build a manifest of one fragment from (name, content, chunk size).
    fn manifest_of(files: &[(&str, &[u8], usize)]) -> (Manifest, Vec<u8>) {
        let mut blob = Vec::new();
        let mut bchunks = Vec::new();
        let mut mfiles = Vec::new();
        let mut seen = HashSet::new();
        for (name, data, cs) in files {
            let mut chunks = Vec::new();
            for (i, piece) in data.chunks(*cs).enumerate() {
                let h = sha(piece);
                chunks.push(Chunk { hash: h, size: piece.len() as u64, offset: (i * cs) as u64 });
                if seen.insert(h) {
                    bchunks.push(Chunk { hash: h, size: piece.len() as u64, offset: blob.len() as u64 });
                    blob.extend_from_slice(piece);
                }
            }
            if chunks.len() == 1 {
                chunks.clear();
            }
            mfiles.push(File { name: name.to_string(), size: data.len() as u64, hash: sha(data), chunks, executable: false, symlink: None });
        }
        let m = Manifest {
            fragments: vec![Fragment { name: "configuration".into(), files: mfiles, bundles: vec![Bundle { hash: [7; 20], chunks: bchunks }] }],
        };
        (m, blob)
    }

    fn job_for(m: Manifest, root: &Path, verify_all: bool, old: Option<(ZaapManifest, Sidecar)>) -> Job {
        let m = Arc::new(m);
        let (targets, fragments) = collect_targets(&m, &["configuration".to_string()], root).unwrap();
        let persist = old.map(|(z, s)| Persist { release: "r".into(), version: "v".into(), old_zaap: z, old_sidecar: s });
        Job { root: root.to_path_buf(), manifest: m, fragments, targets, verify_all, concurrency: 2, speed_limit: None, persist }
    }

    fn checks(job: &Job) -> Vec<&'static str> {
        let p = ProgressState::new(Box::new(|_| {}));
        check_all(job, &CancellationToken::new(), &p)
            .unwrap()
            .iter()
            .map(|c| match c {
                Check::Ok(_) => "ok",
                Check::Build => "build",
                Check::Symlink => "link",
            })
            .collect()
    }

    #[test]
    fn safe_rel_rejects_escapes() {
        assert!(safe_rel("a/b.txt").is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "C:/x", "a\\b", "a/b:ads"] {
            assert!(safe_rel(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn planner_diffs_missing_wrong_and_trusted_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (m, _) = manifest_of(&[("a.txt", b"hello", 64), ("sub/b.txt", b"world!", 64), ("c.txt", b"same", 64)]);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"hello").unwrap(); // correct, no stamp -> hashed -> ok
        fs::write(root.join("sub/b.txt"), b"WORLD!").unwrap(); // same size, wrong content -> build
        // c.txt missing -> build
        let job = job_for(m.clone(), root, false, Some((ZaapManifest::new(), Sidecar::default())));
        assert_eq!(checks(&job), vec!["ok", "build", "build"]);

        // Stamp fast path: a stamp that matches size+mtime is trusted even if content changed.
        fs::write(root.join("sub/b.txt"), b"world!").unwrap();
        let meta = fs::metadata(root.join("sub/b.txt")).unwrap();
        fs::write(root.join("c.txt"), b"same").unwrap();
        let mut sc = Sidecar::default();
        sc.files.insert("sub/b.txt".into(), Stamp::of(&meta, m.fragments[0].files[1].hash));
        let job = job_for(m.clone(), root, false, Some((ZaapManifest::new(), sc.clone())));
        assert_eq!(checks(&job), vec!["ok", "ok", "ok"]);

        // Same-size corruption that keeps the mtime is caught only by verify_all.
        let f = fs::OpenOptions::new().write(true).open(root.join("sub/b.txt")).unwrap();
        write_all_at(&f, b"W", 0).unwrap();
        f.set_modified(meta.modified().unwrap()).unwrap();
        drop(f);
        let job = job_for(m.clone(), root, false, Some((ZaapManifest::new(), sc.clone())));
        assert_eq!(checks(&job), vec!["ok", "ok", "ok"]);
        let job = job_for(m, root, true, Some((ZaapManifest::new(), sc)));
        assert_eq!(checks(&job), vec!["ok", "build", "ok"]);
    }

    #[test]
    fn local_phase_reuses_chunks_and_resumes_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let old: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect(); // 4000 bytes
        let mut new = old.clone();
        new[3999] ^= 0xff; // last 100-byte chunk changes
        let (m_old, _) = manifest_of(&[("big.bin", &old, 100)]);
        let (m_new, _) = manifest_of(&[("big.bin", &new, 100)]);
        fs::write(root.join("big.bin"), &old).unwrap();
        let mut zaap = ZaapManifest::new();
        zaap.insert("big.bin".into(), ZaapFile::from_manifest(&m_old.fragments[0].files[0]));
        let job = job_for(m_new, root, false, Some((zaap, Sidecar::default())));
        let c = check_all(&job, &CancellationToken::new(), &ProgressState::new(Box::new(|_| {}))).unwrap();
        assert!(matches!(c[0], Check::Build));
        let index = build_source_index(&job, &c);
        let t = &job.targets[0];
        let tmp = tmp_path(&t.abs);
        let needs = prepare_build(job.file(t), &tmp, &index, &mut SrcCache::default()).unwrap();
        assert_eq!(needs.len(), 1, "only the changed chunk is remote");
        assert_eq!(needs[0].1, 3900);
        // Simulate the download of the last chunk, then a fresh "resume" pass needs nothing.
        let f = fs::OpenOptions::new().write(true).open(&tmp).unwrap();
        write_all_at(&f, &new[3900..], 3900).unwrap();
        drop(f);
        let needs = prepare_build(job.file(t), &tmp, &SourceIndex::new(), &mut SrcCache::default()).unwrap();
        assert!(needs.is_empty());
        assert_eq!(hash_file(&tmp).unwrap(), sha(&new));
    }

    #[test]
    fn sink_assembles_finalizes_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        let (m, blob) = manifest_of(&[("x/y.bin", &data, 1000), ("dup.bin", &data, 1000), ("empty", b"", 10)]);
        let job = Arc::new(job_for(m, root, false, Some((ZaapManifest::new(), Sidecar::default()))));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // Run the local phase + assembler by hand (no network): feed every bundle chunk to the sink.
        let progress = Arc::new(ProgressState::new(Box::new(|_| {})));
        let c = check_all(&job, &CancellationToken::new(), &progress).unwrap();
        let rs = Arc::new(Mutex::new(RunState { zaap: ZaapManifest::new(), stamps: BTreeMap::new(), dirty: false }));
        let mut files = Vec::new();
        let mut dests: HashMap<Hash, Vec<(u32, u64)>> = HashMap::new();
        for (i, t) in job.targets.iter().enumerate() {
            assert!(matches!(c[i], Check::Build));
            let tmp = tmp_path(&t.abs);
            let needs = prepare_build(job.file(t), &tmp, &SourceIndex::new(), &mut SrcCache::default()).unwrap();
            let bi = files.len() as u32;
            for (h, off) in &needs {
                dests.entry(*h).or_default().push((bi, *off));
            }
            let f = job.file(t);
            files.push(BuildFile { target: i, tmp, size: f.size, hash: f.hash, remaining: AtomicUsize::new(needs.len()), finalized: AtomicBool::new(false) });
        }
        let a = Assembler { job: job.clone(), files, dests, pool: Mutex::new(HashMap::new()), rs: rs.clone(), progress };
        a.finalize(2).unwrap(); // empty file has no chunk
        for ch in &job.manifest.fragments[0].bundles[0].chunks {
            a.write(&ch.hash, &blob[ch.offset as usize..(ch.offset + ch.size) as usize]).unwrap();
        }
        assert!(a.files.iter().all(|b| b.finalized.load(Ordering::Relaxed)));
        assert_eq!(fs::read(root.join("x/y.bin")).unwrap(), data);
        assert_eq!(fs::read(root.join("dup.bin")).unwrap(), data);
        assert_eq!(fs::read(root.join("empty")).unwrap(), b"");
        assert!(!tmp_path(&root.join("x/y.bin")).exists());
        assert_eq!(rs.lock().unwrap().stamps.len(), 3);
        // finish(): writes final state; deletes files known to the old state only.
        fs::write(root.join("old.bin"), b"o").unwrap();
        fs::write(root.join("user-screenshot.png"), b"u").unwrap();
        fs::write(root.join("game.log"), b"l").unwrap();
        let mut z = ZaapManifest::new();
        for n in ["old.bin", "game.log"] {
            z.insert(n.into(), ZaapFile { name: n.into(), size: 1, hash: String::new(), chunks: vec![], executable: false, symlink: None });
        }
        let mut job2 = job_for((*job.manifest).clone(), root, false, Some((z, Sidecar::default())));
        job2.persist.as_mut().unwrap().version = "6.0_1".into();
        finish(&job2, &rs).unwrap();
        assert!(!root.join("old.bin").exists());
        assert!(root.join("user-screenshot.png").exists());
        assert!(root.join("game.log").exists());
        let sc = state::read_sidecar(root);
        assert_eq!(sc.installed_version.as_deref(), Some("6.0_1"));
        assert_eq!(sc.files.len(), 3);
        assert_eq!(state::read_zaap_manifest(root).len(), 3);
        drop(rt);
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let l = InstallLock::acquire(dir.path()).unwrap();
        let e = InstallLock::acquire(dir.path()).err().unwrap();
        assert!(matches!(e.downcast_ref::<CytrusError>(), Some(CytrusError::Locked)));
        drop(l);
        InstallLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn no_process_runs_from_a_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(running_pids_under(dir.path()).is_empty());
        // Our own test binary lives under target/: excluded as "self".
        let exe = std::env::current_exe().unwrap();
        assert!(running_pids_under(exe.parent().unwrap()).is_empty());
    }

    #[test]
    fn progress_is_deduplicated() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let p = ProgressState::new(Box::new(move |_| {
            n2.fetch_add(1, Ordering::Relaxed);
        }));
        p.tick(false);
        p.tick(false);
        assert_eq!(n.load(Ordering::Relaxed), 1);
        p.files_done.store(1, Ordering::Relaxed);
        p.tick(false);
        p.tick(true);
        assert_eq!(n.load(Ordering::Relaxed), 3);
    }
}
