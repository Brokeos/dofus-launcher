//! Our own data folder (never the official `%APPDATA%\zaap`, to avoid fighting Zaap).
use std::path::PathBuf;

/// `%APPDATA%\dofus-launcher`
pub fn data_dir() -> PathBuf {
    dirs::config_dir().expect("APPDATA").join("dofus-launcher")
}
/// `%APPDATA%\dofus-launcher\logs`
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
/// Game logs folder passed as `ZAAP_LOGS_PATH` (`logs\gamesLogs\dofus-<release>`).
pub fn game_logs_dir(release: &str) -> PathBuf {
    let folder = if release == "main" { crate::DOFUS_GAME_UID.to_string() } else { format!("{}-{release}", crate::DOFUS_GAME_UID) };
    logs_dir().join("gamesLogs").join(folder)
}
/// Default install dir, same as Zaap: `%LOCALAPPDATA%\Ankama\Dofus-<release>` (`Dofus` for main).
pub fn default_install_dir(release: &str) -> PathBuf {
    let base = dirs::data_local_dir().expect("LOCALAPPDATA").join("Ankama");
    if release == "main" { base.join("Dofus") } else { base.join(format!("Dofus-{release}")) }
}

/// Atomic write: tmp file in the same dir, fsync, rename over target.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}
