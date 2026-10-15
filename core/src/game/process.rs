//! Process snapshots: find `Dofus.exe` processes whose image lives under an install dir.
use std::path::Path;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub const GAME_EXE: &str = "Dofus.exe";

fn snapshot() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    sys
}

/// Lower-cased, `\`-separated, without `\\?\` prefix or trailing separator.
fn normalize(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or(s);
    let s = s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s);
    s.trim_end_matches('\\').to_lowercase()
}

/// `true` when `path` is inside `dir` (case-insensitive, Windows semantics).
pub fn path_is_inside(path: &Path, dir: &Path) -> bool {
    let (p, d) = (normalize(path), normalize(dir));
    // Also compare against the canonical dir, in case `dir` contains `..` or a junction.
    let canon = std::fs::canonicalize(dir).ok().map(|c| normalize(&c));
    let inside = |d: &str| {
        !d.is_empty() && p.len() > d.len() && p.starts_with(d) && p.as_bytes()[d.len()] == b'\\'
    };
    inside(&d) || canon.as_deref().is_some_and(inside)
}

fn is_game_process(proc_: &sysinfo::Process, install_dir: &Path) -> bool {
    proc_.exe().is_some_and(|exe| {
        exe.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(GAME_EXE))
            && path_is_inside(exe, install_dir)
    })
}

/// Pids of every running `Dofus.exe` whose executable is inside `install_dir`.
pub fn running_game_pids(install_dir: &Path) -> Vec<u32> {
    let sys = snapshot();
    let mut pids: Vec<u32> = sys
        .processes()
        .iter()
        .filter(|(_, p)| is_game_process(p, install_dir))
        .map(|(pid, _)| pid.as_u32())
        .collect();
    pids.sort_unstable();
    pids
}

/// The `Dofus.exe` started by `parent_pid` (the `cmd.exe` running `zaap-start.bat`).
pub fn find_game_child(install_dir: &Path, parent_pid: u32) -> Option<u32> {
    let sys = snapshot();
    let parent = Pid::from_u32(parent_pid);
    sys.processes()
        .iter()
        .find(|(_, p)| p.parent() == Some(parent) && is_game_process(p, install_dir))
        .map(|(pid, _)| pid.as_u32())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_is_case_insensitive_and_strict() {
        let dir = Path::new(r"C:\Users\X\AppData\Local\Ankama\Dofus-dofus3");
        assert!(path_is_inside(
            Path::new(r"c:\users\x\appdata\local\ankama\DOFUS-DOFUS3\Dofus.exe"),
            dir
        ));
        assert!(path_is_inside(
            Path::new(r"\\?\C:\Users\X\AppData\Local\Ankama\Dofus-dofus3\sub\Dofus.exe"),
            dir
        ));
        assert!(!path_is_inside(
            Path::new(r"C:\Users\X\AppData\Local\Ankama\Dofus-dofus3-old\Dofus.exe"),
            dir
        ));
        assert!(!path_is_inside(dir, dir));
    }

    #[test]
    fn no_game_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(running_game_pids(dir.path()).is_empty());
    }
}
