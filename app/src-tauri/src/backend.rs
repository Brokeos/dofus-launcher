//! `zaap::Backend` implementation: what the running game can ask the launcher.
use crate::state::{App, release};
use dofus_core::game::GameSettings;
use dofus_core::zaap::{Backend, ZaapServer, TEXT_SOCKET_PORT, ZAAP_PORT};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct AppBackend(pub App);

impl Backend for AppBackend {
    fn create_game_token(&self, account_id: i64, game_id: i32) -> BoxFuture<'static, anyhow::Result<String>> {
        let auth = self.0.auth.clone();
        tracing::info!(account_id, game_id, "game requested a login token");
        Box::pin(async move { auth.create_game_token(account_id, game_id).await })
    }

    fn user_info_json(&self, account_id: i64) -> Option<String> {
        self.0.auth.user_info_json(account_id)
    }

    fn game_setting_get(&self, key: &str) -> Option<String> {
        let config = self.0.current_config()?;
        config.settings_values.get(key).map(|v| v.to_string())
    }

    fn game_setting_set(&self, key: &str, json_value: &str) -> anyhow::Result<()> {
        let value: serde_json::Value = serde_json::from_str(json_value)?;
        let config = self.0.current_config().ok_or_else(|| anyhow::anyhow!("configuration indisponible"))?;
        let mut settings = GameSettings::load(release()).unwrap_or_default();
        settings.set_validated(&config.settings, key, value)?;
        settings.save(release())?;
        self.0.rerender_remote_config();
        Ok(())
    }

    fn is_update_available(&self) -> bool {
        self.0.release_state().update_available
    }

    fn active_account_ids(&self) -> Vec<i64> {
        self.0.auth.accounts().into_iter().filter(|a| a.active && !a.needs_login).map(|a| a.id).collect()
    }
}

/// Start the Thrift (26116) and text (26117) servers once. Fails clearly if the official launcher holds the port.
pub async fn ensure_zaap_server(app: &App) -> anyhow::Result<()> {
    let mut guard = app.zaap.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let backend: Arc<dyn Backend> = Arc::new(AppBackend(app.clone()));
    let server = ZaapServer::bind(ZAAP_PORT, app.sessions.clone(), backend.clone()).await?;
    let token = CancellationToken::new();
    tokio::spawn(server.run(token.clone()));
    match ZaapServer::bind_text(TEXT_SOCKET_PORT, app.sessions.clone(), backend).await {
        Ok(text) => {
            tokio::spawn(text.run(token.clone()));
        }
        Err(e) => tracing::warn!("text socket {TEXT_SOCKET_PORT} unavailable: {e:#}"),
    }
    tracing::info!("Zaap service listening on 127.0.0.1:{ZAAP_PORT}");
    *guard = Some(token);
    Ok(())
}
