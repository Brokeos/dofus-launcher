//! Loopback OAuth redirect receiver on 127.0.0.1:9001/authorized.
use super::AuthError;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_REQUEST: usize = 16 * 1024;

const SUCCESS_PAGE: &str = "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
<title>Dofus Launcher</title><style>body{font-family:system-ui,sans-serif;background:#1b1d1f;color:#eee;\
display:flex;align-items:center;justify-content:center;height:100vh;margin:0}</style></head>\
<body><h1>Connexion réussie, vous pouvez fermer cet onglet</h1></body></html>";

const FAILURE_PAGE: &str = "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
<title>Dofus Launcher</title></head><body><h1>Connexion refusée, retournez dans le launcher</h1></body></html>";

/// What the browser brought back.
#[derive(PartialEq, Eq)]
pub(crate) enum Redirect {
    Code(String),
    Error(String),
}

impl std::fmt::Debug for Redirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Redirect::Code(_) => f.write_str("Code(<redacted>)"),
            Redirect::Error(e) => write!(f, "Error({e})"),
        }
    }
}

pub(crate) async fn bind() -> Result<TcpListener, AuthError> {
    TcpListener::bind(super::pkce::LISTEN_ADDR).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied => AuthError::PortBusy,
        _ => AuthError::Storage(format!("écoute locale impossible : {e}")),
    })
}

/// Accept connections until one hits `/authorized` with `code` or `error`. Other paths get 404.
pub(crate) async fn wait_for_redirect(listener: &TcpListener) -> Redirect {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        match tokio::time::timeout(Duration::from_secs(10), handle(stream)).await {
            Ok(Some(r)) => return r,
            _ => continue,
        }
    }
}

async fn handle(mut stream: TcpStream) -> Option<Redirect> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let parsed = parse_request(&buf);
    let (status, body) = match &parsed {
        Some(Redirect::Code(_)) => ("200 OK", SUCCESS_PAGE),
        Some(Redirect::Error(_)) => ("200 OK", FAILURE_PAGE),
        None => ("404 Not Found", "Not Found"),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
    parsed
}

/// Parse `GET /authorized?code=..` (or `?error=..`) from a raw HTTP request head.
pub(crate) fn parse_request(raw: &[u8]) -> Option<Redirect> {
    let head = std::str::from_utf8(raw).ok()?;
    let mut parts = head.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1:9001{target}")).ok()?;
    if url.path() != super::pkce::REDIRECT_PATH {
        return None;
    }
    let mut error = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" if !v.is_empty() => return Some(Redirect::Code(v.into_owned())),
            "error_description" => error = Some(v.into_owned()),
            "error" if error.is_none() => error = Some(v.into_owned()),
            _ => {}
        }
    }
    error.map(Redirect::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_redirects() {
        assert_eq!(
            parse_request(b"GET /authorized?code=ab%2Bc HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(Redirect::Code("ab+c".into()))
        );
        assert_eq!(
            parse_request(b"GET /authorized?error=access_denied HTTP/1.1\r\n\r\n"),
            Some(Redirect::Error("access_denied".into()))
        );
        assert_eq!(parse_request(b"GET /favicon.ico HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_request(b"GET /authorized HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_request(b"POST /authorized?code=x HTTP/1.1\r\n\r\n"), None);
    }

    #[tokio::test]
    async fn serves_browser_and_returns_code() {
        // Ephemeral port: the real 9001 may be used by the official launcher.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").await.unwrap();
            let mut r = String::new();
            s.read_to_string(&mut r).await.unwrap();
            assert!(r.starts_with("HTTP/1.1 404"));
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(b"GET /authorized?code=XYZ HTTP/1.1\r\nHost: 127.0.0.1:9001\r\n\r\n").await.unwrap();
            let mut r = String::new();
            s.read_to_string(&mut r).await.unwrap();
            r
        });
        assert_eq!(wait_for_redirect(&listener).await, Redirect::Code("XYZ".into()));
        let page = client.await.unwrap();
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("Connexion réussie, vous pouvez fermer cet onglet"));
    }
}
