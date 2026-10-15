//! Game launch (Zaap `Release.prototype.start`, main.js L34700-34816; env L98522-98570).
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use super::config::{RenderContext, ZAAP_START, ZaapConfig, render_zaap_config};
use super::process;
use super::sessions::{GameSession, GameSessions};
use super::settings::GameSettings;

use crate::zaap::ZAAP_PORT;
/// How long the watcher looks for the `Dofus.exe` started by `zaap-start.bat`.
const GAME_PID_DISCOVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct LaunchRequest {
    pub install_dir: PathBuf,
    pub release: String,
    /// `None` → `ZAAP_CAN_AUTH=false`, no `ACCOUNT_ID`.
    pub account_id: Option<i64>,
    /// Launcher language (`zaap.LANGUAGE`, default of the `language` game setting).
    pub language: String,
}

/// Returned by [`launch`]. `hash` is a secret (Debug redacts it).
#[derive(Clone)]
pub struct LaunchedGame {
    /// Pid of the spawned child (`cmd.exe` in `zaap-start` mode).
    pub pid: u32,
    pub instance_id: u32,
    pub hash: String,
}

impl std::fmt::Debug for LaunchedGame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchedGame")
            .field("pid", &self.pid)
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

/// Delivered to the `on_exit` callback once the game (and its launcher script) are gone.
#[derive(Clone, Debug)]
pub struct GameExit {
    pub pid: u32,
    pub game_pid: Option<u32>,
    pub instance_id: u32,
    pub account_id: Option<i64>,
    pub release: String,
    /// Exit code of the spawned child (for `zaap-start`, cmd.exe returns Dofus.exe's code).
    pub exit_code: Option<i32>,
    /// The game asked `release_restartOnExit`.
    pub restart_requested: bool,
}

/// `ZAAP_*` environment of an instance (spec 02 §1.4). `logs_path = None` → `"false"`.
pub fn zaap_env(session: &GameSession, logs_path: Option<&Path>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("ZAAP_PORT".into(), ZAAP_PORT.to_string());
    env.insert("ZAAP_GAME".into(), crate::DOFUS_GAME_UID.into());
    env.insert("ZAAP_RELEASE".into(), session.release.clone());
    env.insert("ZAAP_INSTANCE_ID".into(), session.instance_id.to_string());
    env.insert("ZAAP_HASH".into(), session.hash.clone());
    env.insert(
        "ZAAP_CAN_AUTH".into(),
        session.account_id.is_some().to_string(),
    );
    env.insert(
        "ZAAP_LOGS_PATH".into(),
        logs_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "false".into()),
    );
    if let Some(id) = session.account_id {
        env.insert("ACCOUNT_ID".into(), id.to_string());
    }
    env
}

/// Launch one game instance. Registers a session in `sessions`, spawns the game detached, and starts a
/// watcher thread that removes the session and calls `on_exit` when the game is gone.
pub fn launch(
    req: &LaunchRequest,
    sessions: &GameSessions,
    settings: &GameSettings,
    on_exit: impl FnOnce(GameExit) + Send + 'static,
) -> anyhow::Result<LaunchedGame> {
    // First compile (no env): instance limit and log folder policy.
    let base_ctx = RenderContext::new(&req.language, settings.clone());
    let pre = render_zaap_config(&req.install_dir, &base_ctx)?;
    if let Some(max) = pre.max_instances {
        let running = sessions
            .list()
            .iter()
            .filter(|s| s.release == req.release)
            .count();
        if running >= max as usize {
            bail!("nombre maximal d'instances atteint ({max})");
        }
    }
    let logs = if pre.has_log_button {
        let dir = crate::paths::game_logs_dir(&req.release);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Some(dir)
    } else {
        None
    };

    let session = sessions.create(&req.release, req.account_id);
    let result = (|| {
        let env = zaap_env(&session, logs.as_deref());
        let config = render_zaap_config(&req.install_dir, &base_ctx.clone().with_env(env.clone()))?;
        spawn(&req.install_dir, &config, &env)
    })();
    let (child, script) = match result {
        Ok(v) => v,
        Err(e) => {
            sessions.remove(&session.hash);
            return Err(e);
        }
    };
    let pid = child.id();
    sessions.set_pid(&session.hash, pid);
    if !script {
        sessions.set_game_pid(&session.hash, pid);
    }
    tracing::info!(pid, instance_id = session.instance_id, release = %req.release, "game spawned");

    let launched = LaunchedGame {
        pid,
        instance_id: session.instance_id,
        hash: session.hash.clone(),
    };
    let watcher = Watcher {
        child,
        script,
        install_dir: req.install_dir.clone(),
        sessions: sessions.clone(),
        session,
    };
    std::thread::Builder::new()
        .name(format!("dofus-watch-{pid}"))
        .spawn(move || watcher.run(on_exit))
        .context("spawning game watcher thread")?;
    Ok(launched)
}

/// Spawn according to `config`. Returns the child and whether it is the `zaap-start` script.
fn spawn(
    install_dir: &Path,
    config: &ZaapConfig,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<(Child, bool)> {
    if config.executable == ZAAP_START {
        // Like Zaap's scriptSpawner: env = inherited + ZAAP_* (zaap.yml `env` is not merged on this path).
        let script = if cfg!(windows) {
            "zaap-start.bat"
        } else {
            "zaap-start.sh"
        };
        if !install_dir.join(script).is_file() {
            bail!("{script} introuvable dans {}", install_dir.display());
        }
        let mut cmd = script_command(&config.arguments)?;
        cmd.current_dir(install_dir)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().with_context(|| format!("launching {script}"))?;
        Ok((child, true))
    } else {
        let rel = Path::new(&config.executable);
        if !rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            bail!("zaap.yml: executable must be a relative path inside the install dir");
        }
        let exe = install_dir.join(rel);
        if !exe.is_file() {
            bail!("exécutable introuvable : {}", exe.display());
        }
        let mut cmd = Command::new(&exe);
        cmd.args(&config.arguments)
            .current_dir(install_dir)
            .envs(env)
            .envs(&config.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("launching {}", exe.display()))?;
        Ok((child, false))
    }
}

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `cmd.exe /d /v:off /s /c ".\zaap-start.bat <args>"`, run with cwd = install dir.
#[cfg(windows)]
fn script_command(args: &[String]) -> anyhow::Result<Command> {
    use std::os::windows::process::CommandExt;
    let comspec =
        std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
    let mut cmd = Command::new(comspec);
    cmd.raw_arg(format!(
        "/d /v:off /s /c \"{}\"",
        script_command_line(args)?
    ));
    // CREATE_NO_WINDOW gives cmd a hidden console (DETACHED_PROCESS would make CREATE_NO_WINDOW a no-op
    // and leave cmd console-less); new process group so our Ctrl+C never reaches the game.
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    Ok(cmd)
}

#[cfg(not(windows))]
fn script_command(args: &[String]) -> anyhow::Result<Command> {
    let mut cmd = Command::new("./zaap-start.sh");
    cmd.args(args);
    Ok(cmd)
}

/// `.\zaap-start.bat` followed by the arguments quoted for cmd + the MSVC CRT parser. `%*` in the
/// script forwards this text verbatim to `Dofus.exe`.
pub fn script_command_line(args: &[String]) -> anyhow::Result<String> {
    let mut line = String::from(r".\zaap-start.bat");
    for a in args {
        line.push(' ');
        line.push_str(&quote_cmd_arg(a)?);
    }
    Ok(line)
}

/// Quote one argument (libuv style: bare when safe, `""` when empty). Characters that cmd expands even
/// inside quotes (`%`, `"`) and control characters are refused rather than escaped.
pub fn quote_cmd_arg(arg: &str) -> anyhow::Result<String> {
    if arg.chars().any(|c| c == '"' || c == '%' || c.is_control()) {
        bail!("argument de lancement refusé (caractère interdit) : {arg:?}");
    }
    if arg.is_empty() {
        return Ok("\"\"".into());
    }
    if !arg.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t' | '&' | '|' | '<' | '>' | '^' | '(' | ')' | ',' | ';' | '=' | '!'
        )
    }) {
        return Ok(arg.to_string());
    }
    // Inside quotes only backslashes preceding the closing quote need doubling.
    let trailing = arg.len() - arg.trim_end_matches('\\').len();
    Ok(format!("\"{arg}{}\"", "\\".repeat(trailing)))
}

struct Watcher {
    child: Child,
    script: bool,
    install_dir: PathBuf,
    sessions: GameSessions,
    session: GameSession,
}

impl Watcher {
    fn run(mut self, on_exit: impl FnOnce(GameExit)) {
        let pid = self.child.id();
        let mut game_pid = if self.script { None } else { Some(pid) };
        let mut game_handle: Option<ProcessHandle> = None;
        let mut status = None;
        if self.script {
            // zaap-start.bat runs Dofus.exe synchronously: cmd.exe is its parent and lives as long as it.
            let start = Instant::now();
            while start.elapsed() < GAME_PID_DISCOVERY {
                if let Ok(Some(s)) = self.child.try_wait() {
                    status = Some(s);
                    break;
                }
                if let Some(gp) = process::find_game_child(&self.install_dir, pid) {
                    game_handle = ProcessHandle::open(gp);
                    game_pid = Some(gp);
                    self.sessions.set_game_pid(&self.session.hash, gp);
                    tracing::info!(pid, game_pid = gp, "Dofus.exe identified");
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        let status = status.or_else(|| self.child.wait().ok());
        // If cmd.exe ended first (should not happen), keep waiting on the game itself.
        if let Some(h) = game_handle {
            h.wait();
        }
        let restart_requested = self
            .sessions
            .get(&self.session.hash)
            .is_some_and(|s| s.restart_on_exit);
        self.sessions.remove(&self.session.hash);
        tracing::info!(pid, ?game_pid, "game exited");
        on_exit(GameExit {
            pid,
            game_pid,
            instance_id: self.session.instance_id,
            account_id: self.session.account_id,
            release: self.session.release.clone(),
            exit_code: status.and_then(|s| s.code()),
            restart_requested,
        });
    }
}

/// An owned process handle: keeps the identity of the game process (no pid-reuse confusion).
struct ProcessHandle {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessHandle {
    fn open(pid: u32) -> Option<Self> {
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain FFI call; the returned handle is owned by `Self` and closed in Drop.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        }
        .ok()?;
        Some(ProcessHandle { handle })
    }
    fn wait(&self) {
        use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
        // SAFETY: `self.handle` is a valid process handle opened with SYNCHRONIZE.
        unsafe { WaitForSingleObject(self.handle, INFINITE) };
    }
}

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: handle owned by us, closed exactly once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
impl ProcessHandle {
    fn open(_pid: u32) -> Option<Self> {
        None
    }
    fn wait(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_cmd_arg("").unwrap(), "\"\"");
        assert_eq!(quote_cmd_arg("--port").unwrap(), "--port");
        assert_eq!(quote_cmd_arg("-force-d3d11").unwrap(), "-force-d3d11");
        assert_eq!(quote_cmd_arg("a b").unwrap(), "\"a b\"");
        assert_eq!(quote_cmd_arg(r"C:\a b\").unwrap(), "\"C:\\a b\\\\\"");
        assert_eq!(quote_cmd_arg("x&calc").unwrap(), "\"x&calc\"");
        assert!(quote_cmd_arg("%PATH%").is_err());
        assert!(quote_cmd_arg("a\"b").is_err());
        assert!(quote_cmd_arg("a\nb").is_err());
        let line = script_command_line(&["--hash".into(), "abc".into(), "".into()]).unwrap();
        assert_eq!(line, r#".\zaap-start.bat --hash abc """#);
    }

    #[test]
    fn env_vars_match_spec() {
        let sessions = GameSessions::new();
        let s = sessions.create("dofus3", Some(42));
        let env = zaap_env(&s, Some(Path::new(r"C:\logs")));
        assert_eq!(env["ZAAP_PORT"], "26116");
        assert_eq!(env["ZAAP_GAME"], "dofus");
        assert_eq!(env["ZAAP_RELEASE"], "dofus3");
        assert_eq!(env["ZAAP_INSTANCE_ID"], s.instance_id.to_string());
        assert_eq!(env["ZAAP_HASH"], s.hash);
        assert_eq!(env["ZAAP_CAN_AUTH"], "true");
        assert_eq!(env["ZAAP_LOGS_PATH"], r"C:\logs");
        assert_eq!(env["ACCOUNT_ID"], "42");
        let s2 = sessions.create("dofus3", None);
        let env2 = zaap_env(&s2, None);
        assert_eq!(env2["ZAAP_CAN_AUTH"], "false");
        assert_eq!(env2["ZAAP_LOGS_PATH"], "false");
        assert!(!env2.contains_key("ACCOUNT_ID"));
    }

    #[test]
    fn launch_fails_cleanly_without_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("zaap.yml"),
            "maxInstances: true\nhasLogButton: false\n",
        )
        .unwrap();
        let sessions = GameSessions::new();
        let req = LaunchRequest {
            install_dir: dir.path().into(),
            release: "test-nolaunch".into(),
            account_id: None,
            language: "fr".into(),
        };
        let err = launch(&req, &sessions, &GameSettings::default(), |_| {}).unwrap_err();
        assert!(err.to_string().contains("zaap-start"), "{err}");
        assert!(sessions.is_empty());
    }
}
