//! End-to-end: Thrift server on an ephemeral port, mock Backend, hand-written client.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dofus_core::game::GameSessions;
use dofus_core::zaap::protocol::{
    Message, MessageType, TStruct, TValue, decode_message, encode_message,
};
use dofus_core::zaap::{Backend, ServerError, ZaapServer};
use futures::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Mock {
    token_calls: Mutex<Vec<(i64, i32)>>,
    set_calls: Mutex<Vec<(String, String)>>,
}

impl Backend for Mock {
    fn create_game_token(
        &self,
        account_id: i64,
        game_id: i32,
    ) -> BoxFuture<'static, anyhow::Result<String>> {
        self.token_calls.lock().unwrap().push((account_id, game_id));
        Box::pin(async move {
            if account_id == 666 {
                anyhow::bail!("haapi 500")
            } else {
                Ok(format!("token-{account_id}-{game_id}"))
            }
        })
    }
    fn user_info_json(&self, account_id: i64) -> Option<String> {
        Some(format!("{{\"id\":{account_id}}}"))
    }
    fn game_setting_get(&self, key: &str) -> Option<String> {
        match key {
            "language" => Some("\"fr\"".into()),
            "autoConnectType" => Some("1".into()),
            _ => None,
        }
    }
    fn game_setting_set(&self, key: &str, json_value: &str) -> anyhow::Result<()> {
        self.set_calls
            .lock()
            .unwrap()
            .push((key.into(), json_value.into()));
        Ok(())
    }
    fn is_update_available(&self) -> bool {
        true
    }
    fn active_account_ids(&self) -> Vec<i64> {
        vec![10, 20]
    }
}

struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
    seq: i32,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        Client {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: vec![],
            seq: 0,
        }
    }
    async fn read_message(&mut self) -> Message {
        loop {
            if let Some((m, used)) = decode_message(&self.buf).unwrap() {
                self.buf.drain(..used);
                return m;
            }
            let n =
                tokio::time::timeout(Duration::from_secs(5), self.stream.read_buf(&mut self.buf))
                    .await
                    .unwrap()
                    .unwrap();
            assert!(n > 0, "server closed connection");
        }
    }
    fn encode(&mut self, name: &str, args: TStruct) -> Vec<u8> {
        self.seq += 1;
        let mut out = vec![];
        encode_message(
            &Message {
                name: name.into(),
                kind: MessageType::Call,
                seqid: self.seq,
                body: args,
            },
            &mut out,
        );
        out
    }
    async fn call(&mut self, name: &str, args: TStruct) -> Message {
        let bytes = self.encode(name, args);
        self.stream.write_all(&bytes).await.unwrap();
        let m = self.read_message().await;
        assert_eq!((m.name.as_str(), m.seqid), (name, self.seq));
        m
    }
}

fn s(v: &str) -> TValue {
    TValue::string(v)
}
fn success_str(m: &Message) -> String {
    assert_eq!(m.kind, MessageType::Reply, "{m:?}");
    match m.body.get(0) {
        Some(TValue::Binary(b)) => String::from_utf8(b.clone()).unwrap(),
        other => panic!("no string success: {other:?}"),
    }
}
fn error_code(m: &Message) -> i32 {
    assert_eq!(m.kind, MessageType::Reply);
    m.body
        .get_struct(1)
        .and_then(|e| e.get_i32(1))
        .expect("ZaapError")
}

async fn start() -> (
    std::net::SocketAddr,
    GameSessions,
    Arc<Mock>,
    CancellationToken,
) {
    let sessions = GameSessions::new();
    let mock = Arc::new(Mock::default());
    let server = ZaapServer::bind(0, sessions.clone(), mock.clone())
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let token = CancellationToken::new();
    tokio::spawn(server.run(token.clone()));
    (addr, sessions, mock, token)
}

#[tokio::test]
async fn thrift_end_to_end() {
    let (addr, sessions, mock, token) = start().await;
    let session = sessions.create("dofus3", Some(42));
    let mut c = Client::connect(addr).await;

    // connect, written byte-for-byte by hand (strict header, fields 1..4) and sent one byte at a time.
    let mut raw: Vec<u8> = vec![0x80, 0x01, 0x00, 0x01, 0, 0, 0, 7];
    raw.extend_from_slice(b"connect");
    raw.extend_from_slice(&[0, 0, 0, 1]);
    for (id, val) in [(1u8, "dofus"), (2, "dofus3")] {
        raw.extend_from_slice(&[11, 0, id]);
        raw.extend_from_slice(&(val.len() as u32).to_be_bytes());
        raw.extend_from_slice(val.as_bytes());
    }
    raw.extend_from_slice(&[8, 0, 3, 0, 0, 0, session.instance_id as u8]);
    raw.extend_from_slice(&[11, 0, 4]);
    raw.extend_from_slice(&(session.hash.len() as u32).to_be_bytes());
    raw.extend_from_slice(session.hash.as_bytes());
    raw.push(0);
    for b in &raw {
        c.stream.write_all(std::slice::from_ref(b)).await.unwrap();
        c.stream.flush().await.unwrap();
    }
    c.seq = 1;
    let m = c.read_message().await;
    assert_eq!(
        (m.name.as_str(), m.kind, m.seqid),
        ("connect", MessageType::Reply, 1)
    );
    let gs = success_str(&m);
    assert_eq!(gs, session.hash);
    assert!(sessions.get(&gs).unwrap().connected);

    // Unknown hash.
    let m = c.call("connect", TStruct::new().with(4, s("nope"))).await;
    assert_eq!(error_code(&m), 1002);

    // Game token uses the session's account; gameId forwarded.
    let m = c
        .call(
            "auth_getGameToken",
            TStruct::new().with(1, s(&gs)).with(2, TValue::I32(1)),
        )
        .await;
    assert_eq!(success_str(&m), "token-42-1");
    assert_eq!(mock.token_calls.lock().unwrap().as_slice(), &[(42, 1)]);
    let m = c
        .call(
            "auth_getGameToken",
            TStruct::new().with(1, s("bad")).with(2, TValue::I32(1)),
        )
        .await;
    assert_eq!(error_code(&m), 3);

    // WithWindowId: Nth active account.
    let m = c
        .call(
            "auth_getGameTokenWithWindowId",
            TStruct::new()
                .with(1, s(&gs))
                .with(2, TValue::I32(1))
                .with(3, TValue::I32(2)),
        )
        .await;
    assert_eq!(success_str(&m), "token-20-1");
    let m = c
        .call(
            "auth_getGameTokenWithWindowId",
            TStruct::new()
                .with(1, s(&gs))
                .with(2, TValue::I32(1))
                .with(3, TValue::I32(3)),
        )
        .await;
    assert_eq!(error_code(&m), 2020);

    // Settings.
    let m = c
        .call(
            "settings_get",
            TStruct::new().with(1, s(&gs)).with(2, s("language")),
        )
        .await;
    assert_eq!(success_str(&m), "\"fr\"");
    let m = c
        .call(
            "settings_get",
            TStruct::new().with(1, s(&gs)).with(2, s("missing")),
        )
        .await;
    assert_eq!(error_code(&m), 4001);
    let m = c
        .call(
            "settings_set",
            TStruct::new()
                .with(1, s(&gs))
                .with(2, s("autoConnectType"))
                .with(3, s("2")),
        )
        .await;
    assert_eq!((m.kind, m.body.fields.len()), (MessageType::Reply, 0));
    let m = c
        .call(
            "settings_set",
            TStruct::new()
                .with(1, s(&gs))
                .with(2, s("autoConnectType"))
                .with(3, s("{bad")),
        )
        .await;
    assert_eq!(error_code(&m), 4002);
    assert_eq!(
        mock.set_calls.lock().unwrap().as_slice(),
        &[("autoConnectType".to_string(), "2".to_string())]
    );

    // Misc.
    let m = c
        .call("zaapVersion_get", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(success_str(&m), "\"3.15.5\"");
    let m = c
        .call("zaapMustUpdate_get", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(m.body.get_bool(0), Some(false));
    let m = c
        .call("updater_isUpdateAvailable", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(m.body.get_bool(0), Some(true));
    let m = c
        .call(
            "hasGameAccess",
            TStruct::new().with(1, s(&gs)).with(2, TValue::I32(1)),
        )
        .await;
    assert_eq!(m.body.get_bool(0), Some(true));
    let m = c
        .call("hasPremiumAccess", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(m.body.get_bool(0), Some(false));
    let m = c.call("userInfo_get", TStruct::new().with(1, s(&gs))).await;
    assert_eq!(success_str(&m), "{\"id\":42}");
    let pos = TStruct::new().with(1, TValue::I32(25));
    let m = c
        .call(
            "payArticle",
            TStruct::new()
                .with(1, s(&gs))
                .with(-1, s("k"))
                .with(-2, TValue::I32(3))
                .with(-3, TValue::Struct(pos)),
        )
        .await;
    assert_eq!(error_code(&m), 6003);
    let m = c
        .call(
            "system_addNotification",
            TStruct::new()
                .with(1, s(&gs))
                .with(2, TValue::Struct(TStruct::new())),
        )
        .await;
    assert_eq!(error_code(&m), 7002);
    let m = c
        .call("release_restartOnExit", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(m.kind, MessageType::Reply);
    assert!(sessions.get(&gs).unwrap().restart_on_exit);

    // Unknown method → TApplicationException(UNKNOWN_METHOD); connection stays usable.
    let m = c.call("doesNotExist", TStruct::new().with(1, s(&gs))).await;
    assert_eq!(m.kind, MessageType::Exception);
    assert_eq!(m.body.get_i32(2), Some(1));
    assert_eq!(m.body.get_str(1), Some("Unknown function doesNotExist"));

    // Backend failure → TApplicationException(UNKNOWN).
    let bad = sessions.create("dofus3", Some(666));
    let m = c
        .call(
            "auth_getGameToken",
            TStruct::new().with(1, s(&bad.hash)).with(2, TValue::I32(1)),
        )
        .await;
    assert_eq!(
        (m.kind, m.body.get_i32(2)),
        (MessageType::Exception, Some(0))
    );

    // Several messages in one write → answered in order.
    let mut batch = c.encode("zaapVersion_get", TStruct::new().with(1, s(&gs)));
    batch.extend(c.encode("zaapMustUpdate_get", TStruct::new().with(1, s(&gs))));
    c.stream.write_all(&batch).await.unwrap();
    let a = c.read_message().await;
    let b = c.read_message().await;
    assert_eq!(
        (a.name.as_str(), b.name.as_str(), b.seqid),
        ("zaapVersion_get", "zaapMustUpdate_get", c.seq)
    );

    // Malformed stream closes only that connection.
    let mut bad_conn = TcpStream::connect(addr).await.unwrap();
    bad_conn
        .write_all(&[0x80, 0x07, 0, 1, 0, 0, 0, 0])
        .await
        .unwrap();
    let mut sink = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), bad_conn.read(&mut sink))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    let m = c
        .call("zaapVersion_get", TStruct::new().with(1, s(&gs)))
        .await;
    assert_eq!(success_str(&m), "\"3.15.5\"");

    token.cancel();
}

#[tokio::test]
async fn port_in_use_is_typed() {
    let (addr, sessions, mock, token) = start().await;
    match ZaapServer::bind(addr.port(), sessions, mock).await {
        Err(ServerError::PortInUse { port }) => {
            assert_eq!(port, addr.port());
            assert!(
                ServerError::PortInUse { port }
                    .to_string()
                    .contains("launcher Ankama")
            );
        }
        other => panic!("expected PortInUse, got {:?}", other.err()),
    }
    token.cancel();
}

#[tokio::test]
async fn text_socket() {
    let sessions = GameSessions::new();
    let session = sessions.create("dofus3", Some(7));
    let server = ZaapServer::bind_text(0, sessions.clone(), Arc::new(Mock::default()))
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let token = CancellationToken::new();
    tokio::spawn(server.run(token.clone()));
    let mut c = TcpStream::connect(addr).await.unwrap();
    let mut got = Vec::new();
    let req = format!(
        "connect dofus dofus3 1 {}\0auth_getGameToken {} 1\0settings_get {} nope\0",
        session.hash, session.hash, session.hash
    );
    c.write_all(req.as_bytes()).await.unwrap();
    read_until_nuls(&mut c, &mut got, 4).await;
    let text = String::from_utf8(got).unwrap();
    let parts: Vec<&str> = text.split('\0').collect();
    assert_eq!(parts[0], "connected");
    assert_eq!(parts[1], format!("connect {}", session.hash));
    assert_eq!(parts[2], "auth_getGameToken token-7-1");
    assert_eq!(parts[3], "settings_get error 4001");
    token.cancel();
}

async fn read_until_nuls(c: &mut TcpStream, got: &mut Vec<u8>, n: usize) {
    while got.iter().filter(|&&b| b == 0).count() < n {
        let mut b = [0u8; 256];
        let k = tokio::time::timeout(Duration::from_secs(5), c.read(&mut b))
            .await
            .unwrap()
            .unwrap();
        assert!(k > 0);
        got.extend_from_slice(&b[..k]);
    }
}
