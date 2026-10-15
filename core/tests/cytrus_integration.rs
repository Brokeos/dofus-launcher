//! Real-CDN integration test (downloads the dofus3 manifest, ~51 MB, cached in data_dir()/cache,
//! plus the tiny `configuration` and `start-script` fragments). Run with:
//! `cargo test --test cytrus_integration -- --ignored --nocapture`
use std::sync::{Arc, Mutex};

use dofus_core::cytrus::{self, Phase, Progress, UpdateOptions};
use tokio_util::sync::CancellationToken;

fn opts(dir: &std::path::Path, verify_all: bool) -> UpdateOptions {
    let mut o = UpdateOptions::new("dofus3", dir, vec!["start-script".into(), "configuration".into(), "start-script".into()]);
    o.verify_all = verify_all;
    o
}

async fn run(o: UpdateOptions) -> Vec<Progress> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let t = std::time::Instant::now();
    cytrus::run_update(o, CancellationToken::new(), move |p| e2.lock().unwrap().push(p)).await.expect("run_update");
    eprintln!("run took {:?}", t.elapsed());
    
    events.lock().unwrap().clone()
}

fn downloaded(events: &[Progress]) -> u64 {
    events.iter().map(|p| p.bytes_done).max().unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: real Ankama CDN"]
async fn install_noop_and_repair_small_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Dofus-dofus3");
    let remote = cytrus::resolve("dofus3").await.unwrap();
    eprintln!("remote: {remote:?}");

    // 1. Fresh install.
    let ev = run(opts(&root, false)).await;
    assert_eq!(ev.last().unwrap().phase, Phase::Done);
    let bytes1 = downloaded(&ev);
    eprintln!("install: {} events, {bytes1} bytes", ev.len());
    assert!(bytes1 > 0 && bytes1 < 1_000_000, "{bytes1}");
    for f in ["zaap.yml", "zaap-windows.yml", "zaap-start.bat", "manifest.json", ".release.infos.json", "migration.launcher.json"] {
        assert!(root.join(f).is_file(), "{f} missing");
    }
    let bat = std::fs::read_to_string(root.join("zaap-start.bat")).unwrap();
    assert!(bat.contains("Dofus.exe"));
    let st = cytrus::local_state(&root).unwrap();
    assert_eq!(st.installed_version.as_deref(), Some(remote.version.as_str()));
    assert_eq!(st.installed_fragments, vec!["configuration", "start-script"]);
    assert!(!std::fs::read_dir(&root).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".dl-tmp")));
    let zaap: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(zaap.as_object().unwrap().len(), 3);
    let infos: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join(".release.infos.json")).unwrap()).unwrap();
    assert_eq!(infos, serde_json::json!({"gameUid":"dofus","release":"dofus3"}));

    // 2. Second run: no-op, zero downloads.
    let ev = run(opts(&root, false)).await;
    assert_eq!(downloaded(&ev), 0);
    assert!(ev.iter().all(|p| p.bytes_total == 0));

    // 3. Corrupt zaap.yml (same size, mtime restored => invisible to the fast path), delete zaap-start.bat.
    let p = root.join("zaap.yml");
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    let mut data = std::fs::read(&p).unwrap();
    let good = data.clone();
    data[0] ^= 0x20;
    std::fs::write(&p, &data).unwrap();
    std::fs::File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap();
    std::fs::remove_file(root.join("zaap-start.bat")).unwrap();
    std::fs::write(root.join("user-note.txt"), b"keep me").unwrap();

    let ev = run(opts(&root, false)).await;
    assert!(root.join("zaap-start.bat").is_file(), "missing file restored by a normal update");
    assert!(downloaded(&ev) > 0);
    assert_eq!(std::fs::read(&p).unwrap(), data, "fast path trusts the (size, mtime) stamp");

    let ev = run(opts(&root, true)).await;
    assert_eq!(ev.last().unwrap().phase, Phase::Done);
    assert_eq!(std::fs::read(&p).unwrap(), good, "verify_all repairs the corrupted file");
    assert!(downloaded(&ev) > 0);
    assert!(root.join("user-note.txt").is_file(), "unknown user files are never deleted");

    // 4. A held lock refuses a concurrent run.
    let lock = std::fs::File::options().read(true).write(true).open(root.join(".dofus-launcher.lock")).unwrap();
    fs4::FileExt::try_lock(&lock).unwrap();
    let err = cytrus::run_update(opts(&root, false), CancellationToken::new(), |_| {}).await.unwrap_err();
    assert!(matches!(err.downcast_ref::<cytrus::CytrusError>(), Some(cytrus::CytrusError::Locked)), "{err:#}");
    drop(lock);

    // 5. fetch_configuration into a separate dir.
    let cfg = dir.path().join("cfg");
    cytrus::fetch_configuration("dofus3", &remote.version, &cfg).await.unwrap();
    assert!(cfg.join("zaap.yml").is_file());

    // 6. Release info.
    let info = cytrus::fetch_release_info("dofus3").await.unwrap();
    assert!(info.get("serverBlocked").is_some() && info.get("updateBlocked").is_some());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: real Ankama CDN"]
async fn parse_full_manifest_fast() {
    let remote = cytrus::resolve("dofus3").await.unwrap();
    let path = dofus_core::paths::data_dir().join("cache").join(format!("dofus3-{}.manifest", remote.version));
    if !path.exists() {
        eprintln!("manifest not cached yet; run install_noop_and_repair_small_fragments first");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let t = std::time::Instant::now();
    let m = cytrus::manifest::parse(&bytes).unwrap();
    let el = t.elapsed();
    let files: usize = m.fragments.iter().map(|f| f.files.len()).sum();
    let chunks: usize = m.fragments.iter().flat_map(|f| &f.bundles).map(|b| b.chunks.len()).sum();
    eprintln!("parsed {} bytes: {} fragments, {files} files, {chunks} bundle chunks in {el:?}", bytes.len(), m.fragments.len());
    assert_eq!(m.fragments.len(), 12);
    if !cfg!(debug_assertions) {
        assert!(el.as_millis() < 1000);
    }
}
