//! OAuth2 PKCE helpers (spec 01 section 2.1).
use base64::Engine;
use rand::RngExt;
use sha2::{Digest, Sha256};

pub const AUTH_BASE: &str = "https://auth.ankama.com/";
pub const REDIRECT_URI: &str = "http://127.0.0.1:9001/authorized";
pub const LISTEN_ADDR: &str = "127.0.0.1:9001";
pub const REDIRECT_PATH: &str = "/authorized";
pub const ORIGIN_TRACKER: &str = "https://www.ankama-launcher.com/launcher";

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Random code verifier (43..=127 chars of the RFC 7636 unreserved set) from the thread CSPRNG.
pub fn code_verifier() -> String {
    let mut rng = rand::rng();
    let len = rng.random_range(43..=127usize);
    (0..len).map(|_| UNRESERVED[rng.random_range(0..UNRESERVED.len())] as char).collect()
}

/// S256 challenge: base64url(sha256(verifier)) without padding.
pub fn code_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `https://auth.ankama.com/login/ankama?code_challenge=..&redirect_uri=..&client_id=102&direct=true&origin_tracker=..`
/// Same parameter order as Zaap; values are URL-encoded.
pub fn authorize_url(challenge: &str) -> String {
    let client_id = crate::LAUNCHER_GAME_ID.to_string();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("code_challenge", challenge)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("client_id", &client_id)
        .append_pair("direct", "true")
        .append_pair("origin_tracker", ORIGIN_TRACKER)
        .finish();
    format!("{AUTH_BASE}login/ankama?{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape() {
        for _ in 0..200 {
            let v = code_verifier();
            assert!((43..=127).contains(&v.len()));
            assert!(v.bytes().all(|b| UNRESERVED.contains(&b)));
        }
        assert_ne!(code_verifier(), code_verifier());
    }

    #[test]
    fn authorize_url_is_encoded_and_ordered() {
        let url = authorize_url("abc-_DEF");
        assert_eq!(
            url,
            "https://auth.ankama.com/login/ankama?code_challenge=abc-_DEF\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A9001%2Fauthorized&client_id=102&direct=true\
             &origin_tracker=https%3A%2F%2Fwww.ankama-launcher.com%2Flauncher"
        );
        let parsed = url::Url::parse(&url).unwrap();
        let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs[1], ("redirect_uri".into(), REDIRECT_URI.into()));
        assert_eq!(pairs[4], ("origin_tracker".into(), ORIGIN_TRACKER.into()));
    }
}
