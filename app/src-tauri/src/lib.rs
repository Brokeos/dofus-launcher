//! Tauri shell: commands for the UI (contract in docs/05-implementation-plan.md), update task, game launch.
mod backend;
mod settings;
mod state;

use dofus_core::auth::{AccountInfo, AuthManager, LoginOutcome};
use dofus_core::cytrus::{self, CytrusError};
use dofus_core::extras::{self, NewsItem, ServerStatus};
use dofus_core::game::{self, GameSettings, LaunchRequest, SettingDef};
use dofus_core::paths;
use serde::Serialize;
use settings::{CloseOnLaunch, LauncherSettings};
use state::{App, Inner, ReleaseState, Task, TaskKind, TaskState, release};
use std::collections::BTreeMap;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;

type CmdResult<T> = Result<T, String>;

/// User-facing (French) message for an error; domain errors already carry French text.
fn ui_err(e: anyhow::Error) -> String {
    if let Some(c) = e.downcast_ref::<CytrusError>() {
        return match c {
            CytrusError::Locked => "Une autre mise à jour utilise déjà ce dossier".into(),
            CytrusError::GameRunning(_) => "Dofus est en cours d'exécution : fermez le jeu avant la mise à jour".into(),
            CytrusError::Cancelled => "Opération annulée".into(),
            CytrusError::InsufficientSpace { needed, available } => format!(
                "Espace disque insuffisant : {:.1} Go nécessaires, {:.1} Go disponibles",
                *needed as f64 / 1e9,
                *available as f64 / 1e9
            ),
            CytrusError::UnknownRelease(r) => format!("Version « {r} » introuvable sur les serveurs Ankama"),
            CytrusError::UnsupportedRepository(v) => format!("Format de mise à jour Ankama non supporté (v{v})"),
        };
    }
    tracing::warn!("command failed: {e:#}");
    e.to_string()
}

fn emit_state(app: &AppHandle) {
    let _ = app.emit("state-changed", ());
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStateDto {
    accounts: Vec<AccountInfo>,
    settings: LauncherSettings,
    release: ReleaseState,
    task: Option<TaskState>,
    running_instances: usize,
}

#[tauri::command]
async fn get_state(app: State<'_, App>) -> CmdResult<AppStateDto> {
    if let Err(e) = app.refresh_remote(false).await {
        tracing::warn!("remote refresh failed (offline?): {e:#}");
    }
    Ok(AppStateDto {
        accounts: app.auth.accounts(),
        settings: app.settings(),
        release: app.release_state(),
        task: app.task_state(),
        running_instances: app.sessions.len(),
    })
}

// ---- Accounts -------------------------------------------------------------------------------

#[tauri::command]
async fn login_start(handle: AppHandle, app: State<'_, App>) -> CmdResult<LoginOutcome> {
    let opener = handle.clone();
    let outcome = app
        .auth
        .login_with_browser(move |url| {
            if let Err(e) = opener.opener().open_url(url, None::<&str>) {
                tracing::error!("cannot open browser: {e}");
            }
        })
        .await
        .map_err(ui_err)?;
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.set_focus();
    }
    emit_state(&handle);
    Ok(outcome)
}

#[tauri::command]
async fn login_cancel(app: State<'_, App>) -> CmdResult<()> {
    app.auth.cancel_login().await;
    Ok(())
}

#[tauri::command]
async fn shield_validate(handle: AppHandle, app: State<'_, App>, account_id: i64, code: String) -> CmdResult<AccountInfo> {
    let info = app.auth.shield_validate(account_id, code.trim()).await.map_err(ui_err)?;
    emit_state(&handle);
    Ok(info)
}

#[tauri::command]
async fn shield_resend(app: State<'_, App>, account_id: i64, sms: bool) -> CmdResult<()> {
    app.auth.shield_resend(account_id, sms).await.map_err(ui_err)
}

#[tauri::command]
fn account_set_main(handle: AppHandle, app: State<'_, App>, account_id: i64) -> CmdResult<()> {
    app.auth.set_main(account_id).map_err(ui_err)?;
    emit_state(&handle);
    Ok(())
}

#[tauri::command]
fn account_set_active(handle: AppHandle, app: State<'_, App>, account_id: i64, active: bool) -> CmdResult<()> {
    app.auth.set_active(account_id, active).map_err(ui_err)?;
    emit_state(&handle);
    Ok(())
}

#[tauri::command]
async fn account_logout(handle: AppHandle, app: State<'_, App>, account_id: i64) -> CmdResult<()> {
    app.auth.logout(account_id).await.map_err(ui_err)?;
    emit_state(&handle);
    Ok(())
}

// ---- Install / update / verify --------------------------------------------------------------

#[derive(Clone, Serialize)]
struct TaskFinished {
    ok: bool,
    error: Option<String>,
}

async fn start_task(handle: AppHandle, app: App, kind: TaskKind) -> CmdResult<()> {
    let remote = app.refresh_remote(true).await.map_err(ui_err)?;
    if remote.update_blocked && kind != TaskKind::Verify {
        return Err("Les mises à jour de Dofus sont temporairement bloquées par Ankama".into());
    }
    let dir = app.install_dir();
    let running = game::running_game_pids(&dir);
    if !running.is_empty() || !app.sessions.is_empty() {
        return Err("Dofus est en cours d'exécution : fermez le jeu avant la mise à jour".into());
    }
    let cancel = CancellationToken::new();
    {
        let mut task = app.task.lock().unwrap();
        if task.is_some() {
            return Err("Une opération est déjà en cours".into());
        }
        *task = Some(Task { kind, cancel: cancel.clone(), progress: None });
    }
    let opts = app.update_options(&dir, remote.config.fragments.clone(), kind == TaskKind::Verify);
    tracing::info!(?kind, dir = %dir.display(), version = %remote.version, "task started");
    emit_state(&handle);
    tauri::async_runtime::spawn(async move {
        let progress_app = app.clone();
        let progress_handle = handle.clone();
        let result = cytrus::run_update(opts, cancel, move |p| {
            if let Some(t) = progress_app.task.lock().unwrap().as_mut() {
                t.progress = Some(p.clone());
            }
            let _ = progress_handle.emit("task-progress", p);
        })
        .await;
        *app.task.lock().unwrap() = None;
        let finished = match result {
            Ok(()) => {
                tracing::info!(?kind, "task finished");
                TaskFinished { ok: true, error: None }
            }
            Err(e) => {
                let cancelled = matches!(e.downcast_ref::<CytrusError>(), Some(CytrusError::Cancelled));
                TaskFinished { ok: false, error: (!cancelled).then(|| ui_err(e)) }
            }
        };
        let _ = handle.emit("task-finished", finished);
        emit_state(&handle);
    });
    Ok(())
}

#[tauri::command]
async fn game_install(handle: AppHandle, app: State<'_, App>, install_dir: Option<String>) -> CmdResult<()> {
    if let Some(dir) = install_dir.filter(|d| !d.trim().is_empty()) {
        let mut s = app.settings();
        s.install_dir = dir.trim().to_string();
        let s = s.sanitized();
        s.save().map_err(ui_err)?;
        *app.settings.lock().unwrap() = s;
    }
    start_task(handle, app.inner().clone(), TaskKind::Install).await
}

#[tauri::command]
async fn game_update(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    start_task(handle, app.inner().clone(), TaskKind::Update).await
}

#[tauri::command]
async fn game_verify(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    start_task(handle, app.inner().clone(), TaskKind::Verify).await
}

#[tauri::command]
fn task_cancel(app: State<'_, App>) -> CmdResult<()> {
    if let Some(t) = app.task.lock().unwrap().as_ref() {
        t.cancel.cancel();
    }
    Ok(())
}

// ---- Play -----------------------------------------------------------------------------------

#[derive(Clone, Serialize)]
struct GameExited {
    pid: u32,
}

#[tauri::command]
async fn game_play(handle: AppHandle, app: State<'_, App>, account_ids: Vec<i64>) -> CmdResult<Vec<u32>> {
    if app.task.lock().unwrap().is_some() {
        return Err("Attendez la fin de la mise à jour en cours".into());
    }
    if let Err(e) = app.refresh_remote(false).await {
        tracing::warn!("remote refresh before play failed, playing offline: {e:#}");
    }
    let rs = app.release_state();
    if !rs.installed {
        return Err("Dofus n'est pas installé".into());
    }
    if rs.update_available {
        return Err("Une mise à jour est nécessaire avant de jouer".into());
    }
    if rs.server_blocked {
        return Err("Les serveurs Dofus sont temporairement fermés par Ankama".into());
    }
    let accounts = app.auth.accounts();
    let ids: Vec<i64> = if account_ids.is_empty() {
        accounts.iter().filter(|a| a.is_main).map(|a| a.id).collect()
    } else {
        account_ids
    };
    if ids.is_empty() {
        return Err("Connectez un compte Ankama pour jouer".into());
    }
    for id in &ids {
        match accounts.iter().find(|a| a.id == *id) {
            None => return Err("Compte introuvable".into()),
            Some(a) if a.needs_login => {
                return Err(format!("Session expirée pour {}, reconnectez ce compte", a.nickname.as_deref().unwrap_or(&a.login)));
            }
            _ => {}
        }
    }
    backend::ensure_zaap_server(app.inner()).await.map_err(ui_err)?;

    let settings = app.settings();
    let game_settings = app.game_settings();
    let mut pids = Vec::new();
    for id in ids {
        let req = LaunchRequest {
            install_dir: app.install_dir(),
            release: release().to_string(),
            account_id: Some(id),
            language: settings.language.clone(),
        };
        let exit_handle = handle.clone();
        let exit_app = app.inner().clone();
        let launched = game::launch(&req, &app.sessions, &game_settings, move |exit| {
            tracing::info!(pid = exit.pid, code = ?exit.exit_code, "game exited");
            let _ = exit_handle.emit("game-exited", GameExited { pid: exit.pid });
            emit_state(&exit_handle);
            if exit_app.sessions.is_empty() {
                on_last_game_exit(&exit_handle, &exit_app);
            }
        })
        .map_err(ui_err)?;
        pids.push(launched.pid);
    }
    if let Some(w) = handle.get_webview_window("main") {
        match settings.close_on_launch {
            CloseOnLaunch::Minimize => {
                let _ = w.minimize();
            }
            CloseOnLaunch::Quit => {
                let _ = w.hide();
            }
            CloseOnLaunch::Keep => {}
        }
    }
    emit_state(&handle);
    Ok(pids)
}

/// When the window was hidden (quit-on-launch or closed while playing), exit with the last game.
fn on_last_game_exit(handle: &AppHandle, _app: &App) {
    let hidden = handle
        .get_webview_window("main")
        .map(|w| !w.is_visible().unwrap_or(true))
        .unwrap_or(true);
    if hidden {
        handle.exit(0);
    }
}

// ---- Settings -------------------------------------------------------------------------------

#[tauri::command]
fn settings_get(app: State<'_, App>) -> LauncherSettings {
    app.settings()
}

#[tauri::command]
fn settings_set(handle: AppHandle, app: State<'_, App>, settings: LauncherSettings) -> CmdResult<LauncherSettings> {
    if app.task.lock().unwrap().is_some() && settings.install_dir != app.settings().install_dir {
        return Err("Impossible de changer de dossier pendant une mise à jour".into());
    }
    let s = settings.sanitized();
    s.save().map_err(ui_err)?;
    app.auth.set_language(&s.language);
    *app.settings.lock().unwrap() = s.clone();
    app.rerender_remote_config();
    emit_state(&handle);
    Ok(s)
}

#[derive(Serialize)]
struct GameSettingsDto {
    defs: Vec<SettingDef>,
    values: BTreeMap<String, serde_json::Value>,
}

fn game_settings_dto(app: &App) -> CmdResult<GameSettingsDto> {
    let config = app.current_config().ok_or("Configuration du jeu indisponible (hors ligne ?)")?;
    Ok(GameSettingsDto { defs: config.settings, values: config.settings_values })
}

#[tauri::command]
async fn game_settings_get(app: State<'_, App>) -> CmdResult<GameSettingsDto> {
    if let Err(e) = app.refresh_remote(false).await {
        tracing::warn!("remote refresh failed: {e:#}");
    }
    game_settings_dto(app.inner())
}

#[tauri::command]
fn game_settings_set(handle: AppHandle, app: State<'_, App>, key: String, value: serde_json::Value) -> CmdResult<GameSettingsDto> {
    let config = app.current_config().ok_or("Configuration du jeu indisponible (hors ligne ?)")?;
    let mut gs = GameSettings::load(release()).unwrap_or_default();
    gs.set_validated(&config.settings, &key, value).map_err(|e| ui_err(e.into()))?;
    gs.save(release()).map_err(ui_err)?;
    app.rerender_remote_config();
    emit_state(&handle);
    game_settings_dto(app.inner())
}

// ---- Extras / misc --------------------------------------------------------------------------

#[tauri::command]
async fn news_get(lang: String) -> CmdResult<Vec<NewsItem>> {
    extras::news(&lang, 6).await.map_err(|e| ui_err(e.context("Actualités indisponibles")))
}

#[tauri::command]
async fn server_status_get(lang: String) -> CmdResult<Vec<ServerStatus>> {
    extras::server_status(&lang).await.map_err(|e| ui_err(e.context("Statut des serveurs indisponible")))
}

#[tauri::command]
fn open_logs_dir(handle: AppHandle) -> CmdResult<()> {
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    handle.opener().open_path(dir.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_install_dir(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    let dir = app.install_dir();
    if !dir.is_dir() {
        return Err("Le dossier d'installation n'existe pas encore".into());
    }
    handle.opener().open_path(dir.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_folder(handle: AppHandle, initial: Option<String>) -> CmdResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = handle.dialog().file().set_title("Dossier d'installation de Dofus");
    if let Some(dir) = initial.map(std::path::PathBuf::from).filter(|p| p.exists()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder(move |p| {
        let _ = tx.send(p.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string()));
    });
    rx.await.map_err(|e| e.to_string())
}

// ---- Setup ----------------------------------------------------------------------------------

fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let appender = tracing_appender::rolling::daily(dir, "launcher.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,dofus_core=debug,hyper=warn,reqwest=warn".into()),
        )
        .init();
    guard
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _log_guard = init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "launcher starting");
    let auth = AuthManager::load().expect("loading accounts");
    let app_state = Inner::new(auth);
    app_state.auth.set_language(&app_state.settings().language);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state.clone())
        .setup(move |app| {
            let handle = app.handle().clone();
            let st = app_state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = st.auth.restore_sessions().await {
                    tracing::warn!("restoring sessions: {e:#}");
                }
                if let Err(e) = st.refresh_remote(true).await {
                    tracing::warn!("initial remote refresh: {e:#}");
                }
                emit_state(&handle);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing while games run would kill the token service the game relies on: hide instead.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.state::<App>();
                if !app.sessions.is_empty() {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            login_start,
            login_cancel,
            shield_validate,
            shield_resend,
            account_set_main,
            account_set_active,
            account_logout,
            game_install,
            game_update,
            game_verify,
            task_cancel,
            game_play,
            settings_get,
            settings_set,
            game_settings_get,
            game_settings_set,
            news_get,
            server_status_get,
            open_logs_dir,
            open_install_dir,
            pick_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
