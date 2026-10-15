//! `zaap`: local `ZaapService` server the game talks to (127.0.0.1:26116, Thrift TBinaryProtocol over
//! unframed buffered TCP) plus the optional legacy text socket (127.0.0.1:26117).
//! See lib.rs, docs/05-implementation-plan.md and docs/02-game-launch-and-zaap-protocol.md §2.
//!
//! Deviations from the contract in docs/05:
//! - `serve` takes `GameSessions` by value (it is a cheap shared clone) instead of `Arc<GameSessions>`.
//! - [`ZaapServer::bind`] / [`ZaapServer::run`] are exposed so callers can bind first (port-busy error
//!   before launching anything), use port 0 in tests and stop the server with a `CancellationToken`.
//! - Bind failure is a typed [`ServerError::PortInUse`] (EADDRINUSE, or WSAEACCES when another process
//!   holds the port with SO_EXCLUSIVEADDRUSE).
//! - `Backend` gains default no-op hooks `on_restart_on_exit` / `on_exit_and_repair`; the restart flag is
//!   also recorded on the session (`GameSession::restart_on_exit`, reported in `GameExit`).
//!
//! Behaviour notes: every method except `connect` and `hasPremiumAccess` / `hasGameAccess` requires a
//! known `gameSession` (else `ZaapError{INVALID_GAME_SESSION=3}`). `hasPremiumAccess` → false,
//! `hasGameAccess` → `gameId == 1` for a known session. Messages may span reads and several may arrive in
//! one read; a malformed stream closes that connection. Text socket: `connect` by pid (Zaap's
//! `find-process` fallback) is not implemented; requests with the wrong arity or an unknown method are
//! ignored like Zaap does.
pub mod protocol;
pub mod service;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::game::GameSessions;
use protocol::{
    Message, MessageType, TStruct, TValue, app_exception, application_exception, decode_message,
    encode_message,
};
pub use service::{Outcome, Service, ZaapError, code};

/// Thrift port (`ZAAP_PORT`).
pub const ZAAP_PORT: u16 = 26116;
/// Text-socket port.
pub const TEXT_SOCKET_PORT: u16 = 26117;
/// Max bytes buffered for one incomplete message before the connection is dropped.
const MAX_BUFFER: usize = 32 * 1024 * 1024;

/// Implemented by the app (AuthManager + settings).
pub trait Backend: Send + Sync + 'static {
    fn create_game_token(
        &self,
        account_id: i64,
        game_id: i32,
    ) -> BoxFuture<'static, anyhow::Result<String>>;
    /// JSON object for `userInfo_get`.
    fn user_info_json(&self, account_id: i64) -> Option<String>;
    /// JSON-encoded value, `None` = unknown key.
    fn game_setting_get(&self, key: &str) -> Option<String>;
    fn game_setting_set(&self, key: &str, json_value: &str) -> anyhow::Result<()>;
    fn is_update_available(&self) -> bool;
    /// Active accounts in display order (`auth_getGameTokenWithWindowId`: windowId is 1-based).
    fn active_account_ids(&self) -> Vec<i64>;
    fn on_restart_on_exit(&self, _instance_id: u32) {}
    fn on_exit_and_repair(&self, _instance_id: u32, _restart_after_repair: bool) {}
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Le port {port} est déjà utilisé : fermez le launcher Ankama officiel")]
    PortInUse { port: u16 },
    #[error("serveur Zaap : {0}")]
    Io(#[from] std::io::Error),
}

async fn bind_loopback(port: u16) -> Result<TcpListener, ServerError> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied => {
                ServerError::PortInUse { port }
            }
            _ => ServerError::Io(e),
        })
}

/// Which wire format a server speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Thrift,
    Text,
}

pub struct ZaapServer {
    listener: TcpListener,
    service: Service,
    wire: Wire,
}

impl ZaapServer {
    /// Bind the Thrift server on `127.0.0.1:port` (0 = ephemeral).
    pub async fn bind(
        port: u16,
        sessions: GameSessions,
        backend: Arc<dyn Backend>,
    ) -> Result<Self, ServerError> {
        Ok(ZaapServer {
            listener: bind_loopback(port).await?,
            service: Service { sessions, backend },
            wire: Wire::Thrift,
        })
    }
    /// Bind the legacy text-socket server (same handlers).
    pub async fn bind_text(
        port: u16,
        sessions: GameSessions,
        backend: Arc<dyn Backend>,
    ) -> Result<Self, ServerError> {
        Ok(ZaapServer {
            listener: bind_loopback(port).await?,
            service: Service { sessions, backend },
            wire: Wire::Text,
        })
    }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    /// Accept loop; returns when `shutdown` is cancelled (open connections are closed too).
    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            let accepted = tokio::select! {
                _ = shutdown.cancelled() => return,
                r = self.listener.accept() => r,
            };
            match accepted {
                Ok((stream, peer)) => {
                    let service = self.service.clone();
                    let token = shutdown.clone();
                    let wire = self.wire;
                    tokio::spawn(async move {
                        let _ = stream.set_nodelay(true);
                        let r = tokio::select! {
                            _ = token.cancelled() => Ok(()),
                            r = async {
                                match wire {
                                    Wire::Thrift => handle_thrift(stream, &service).await,
                                    Wire::Text => handle_text(stream, &service).await,
                                }
                            } => r,
                        };
                        if let Err(e) = r {
                            tracing::debug!(%peer, "zaap connection ended: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("zaap accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }
}

/// Bind `127.0.0.1:port` and serve Thrift forever.
pub async fn serve(
    sessions: GameSessions,
    backend: Arc<dyn Backend>,
    port: u16,
) -> anyhow::Result<()> {
    ZaapServer::bind(port, sessions, backend)
        .await?
        .run(CancellationToken::new())
        .await;
    Ok(())
}

async fn handle_thrift(mut stream: TcpStream, service: &Service) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let mut consumed = 0;
        loop {
            match decode_message(&buf[consumed..]) {
                Ok(Some((msg, used))) => {
                    consumed += used;
                    if let Some(reply) = process_message(service, msg).await {
                        encode_message(&reply, &mut out);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("zaap: malformed thrift message, closing connection: {e}");
                    if !out.is_empty() {
                        stream.write_all(&out).await?;
                    }
                    return Ok(());
                }
            }
        }
        buf.drain(..consumed);
        if !out.is_empty() {
            stream.write_all(&out).await?;
            out.clear();
        }
        if buf.len() > MAX_BUFFER {
            tracing::warn!("zaap: message too large, closing connection");
            return Ok(());
        }
        if stream.read_buf(&mut buf).await? == 0 {
            return Ok(());
        }
    }
}

/// Handle one decoded message; returns the message to send back, if any.
pub async fn process_message(service: &Service, msg: Message) -> Option<Message> {
    let reply = |kind, body| {
        Some(Message {
            name: msg.name.clone(),
            kind,
            seqid: msg.seqid,
            body,
        })
    };
    match msg.kind {
        MessageType::Call | MessageType::Oneway => {}
        _ => {
            let body =
                application_exception(app_exception::INVALID_MESSAGE_TYPE, "Invalid message type");
            return reply(MessageType::Exception, body);
        }
    }
    let outcome = service.call(&msg.name, &msg.body).await;
    if msg.kind == MessageType::Oneway {
        return None;
    }
    match outcome {
        None => {
            tracing::warn!(method = %msg.name, "zaap: unknown method");
            let body = application_exception(
                app_exception::UNKNOWN_METHOD,
                &format!("Unknown function {}", msg.name),
            );
            reply(MessageType::Exception, body)
        }
        Some(out) => match service::result_struct(&msg.name, &out) {
            Some(body) => reply(MessageType::Reply, body),
            None => {
                let text = match &out {
                    Outcome::AppException(m) => m.clone(),
                    _ => String::new(),
                };
                reply(
                    MessageType::Exception,
                    application_exception(app_exception::UNKNOWN, &text),
                )
            }
        },
    }
}

// ---------------------------------------------------------------- text socket (26117)

async fn handle_text(mut stream: TcpStream, service: &Service) -> std::io::Result<()> {
    stream.write_all(b"connected\0").await?;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        while let Some(end) = buf.iter().position(|&b| b == 0) {
            let line: Vec<u8> = buf.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]).into_owned();
            if let Some(reply) = text_request(service, &line).await {
                stream.write_all(reply.as_bytes()).await?;
            }
        }
        if buf.len() > MAX_BUFFER {
            return Ok(());
        }
        if stream.read_buf(&mut buf).await? == 0 {
            return Ok(());
        }
    }
}

/// `"<method> <arg>..."` → `"<method>[ <result>]\0"` or `"<method> error <code>\0"`.
pub async fn text_request(service: &Service, line: &str) -> Option<String> {
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let args: Vec<&str> = parts.collect();
    let params = service::method_params(&method)?;
    if args.len() != params.len() {
        tracing::warn!(method = %method, "text socket: wrong argument count, ignored");
        return None;
    }
    let mut body = TStruct::new();
    for ((id, t), raw) in params.iter().zip(&args) {
        let v = match *t {
            protocol::ttype::STRING => Some(TValue::string(*raw)),
            protocol::ttype::I32 => raw.parse().ok().map(TValue::I32),
            protocol::ttype::BOOL => Some(TValue::Bool(*raw == "true")),
            _ => None,
        };
        if let Some(v) = v {
            body.fields.push((*id, v));
        }
    }
    let out = service.call(&method, &body).await?;
    Some(match out {
        Outcome::Success(None) => format!("{method}\0"),
        Outcome::Success(Some(v)) => {
            let text = match v {
                TValue::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                TValue::Bool(b) => b.to_string(),
                TValue::I32(i) => i.to_string(),
                other => format!("{other:?}"),
            };
            format!("{method} {text}\0")
        }
        Outcome::Error(e) => format!("{method} error {}\0", e.code),
        Outcome::AppException(_) => format!("{method} error {}\0", code::UNKNOWN),
    })
}
