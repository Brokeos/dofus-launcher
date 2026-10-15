//! Minimal Haapi + auth.ankama.com client (spec 01 sections 1, 2, 3, 4, 5).
//! Never retries: 429 is surfaced as `RateLimited`; timeouts surface as `Network`.
use super::AuthError;
use serde_json::Value;
use std::time::Duration;

pub const HAAPI_BASE: &str = "https://haapi.ankama.com/json/Ankama/v5/";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub(crate) struct Haapi {
    haapi_base: String,
    auth_base: String,
}

impl Default for Haapi {
    fn default() -> Self {
        Haapi { haapi_base: HAAPI_BASE.into(), auth_base: super::pkce::AUTH_BASE.into() }
    }
}

/// OAuth token response. `Debug` is redacted.
pub(crate) struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthTokens { <redacted> }")
    }
}

/// RefreshApiKey response (only the fields we use).
pub(crate) struct Refreshed {
    pub key: Option<String>,
    pub refresh_token: Option<String>,
}

/// SecurityCode outcome.
pub(crate) enum CodeSent {
    Sent { domain: Option<String> },
    Otp,
    NotNeeded,
}

impl Haapi {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.haapi_base)
    }

    async fn send(rb: reqwest::RequestBuilder) -> Result<Value, AuthError> {
        let resp = rb.timeout(REQUEST_TIMEOUT).send().await.map_err(net_err)?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(net_err)?;
        if status == 429 {
            return Err(AuthError::RateLimited);
        }
        if status >= 400 {
            return Err(error_from_body(status, &bytes));
        }
        parse_json(&bytes)
    }

    async fn get(&self, path: &str, key: &str, query: &[(&str, &str)]) -> Result<Value, AuthError> {
        let rb = crate::http::client().get(self.url(path)).header("APIKEY", key).query(query);
        Self::send(rb).await
    }

    async fn post(&self, path: &str, key: &str, form: &[(&str, &str)]) -> Result<Value, AuthError> {
        let rb = crate::http::client().post(self.url(path)).header("APIKEY", key).form(form);
        Self::send(rb).await
    }

    /// POST https://auth.ankama.com/token (authorization_code grant).
    pub async fn exchange_code(&self, code: &str, verifier: &str) -> Result<OAuthTokens, AuthError> {
        let client_id = crate::LAUNCHER_GAME_ID.to_string();
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", super::pkce::REDIRECT_URI),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier),
        ];
        let rb = crate::http::client().post(format!("{}token", self.auth_base)).form(&form);
        let body = match Self::send(rb).await {
            Ok(b) => b,
            Err(AuthError::Haapi { status, reason }) if (400..500).contains(&status) => {
                return Err(AuthError::OAuth(reason.unwrap_or_else(|| format!("HTTP {status}"))));
            }
            Err(e) => return Err(e),
        };
        let access_token = str_field(&body, "access_token")
            .ok_or_else(|| AuthError::InvalidResponse("access_token absent".into()))?;
        Ok(OAuthTokens { access_token, refresh_token: str_field(&body, "refresh_token") })
    }

    /// GET Account/Account -> `{id, login, security[], ...}`.
    pub async fn account(&self, key: &str) -> Result<Value, AuthError> {
        self.get("Account/Account", key, &[]).await
    }

    /// POST Account/SignOnWithApiKey game=102 -> `account` object (locked "2" => e-mail not validated).
    pub async fn sign_on(&self, key: &str) -> Result<Value, AuthError> {
        let game = crate::LAUNCHER_GAME_ID.to_string();
        let body = self.post("Account/SignOnWithApiKey", key, &[("game", &game)]).await?;
        let account = body.get("account").cloned().unwrap_or(Value::Null);
        if !account.is_object() {
            return Err(AuthError::InvalidResponse("SignOnWithApiKey sans compte".into()));
        }
        if scalar_string(account.get("locked")).as_deref() == Some("2") {
            return Err(AuthError::EmailNotValidated);
        }
        Ok(account)
    }

    /// POST Api/RefreshApiKey.
    pub async fn refresh(&self, key: &str, refresh_token: &str, lang: &str) -> Result<Refreshed, AuthError> {
        let form = [
            ("refresh_token", refresh_token),
            ("long_life_token", "true"),
            ("shop_key", "ZAAP"),
            ("payment_mode", "OK"),
            ("lang", lang),
        ];
        let body = self.post("Api/RefreshApiKey", key, &form).await?;
        Ok(Refreshed { key: str_field(&body, "key"), refresh_token: str_field(&body, "refresh_token") })
    }

    /// GET Api/DeleteApiKey.
    pub async fn delete_key(&self, key: &str) -> Result<(), AuthError> {
        self.get("Api/DeleteApiKey", key, &[]).await.map(|_| ())
    }

    /// GET Shield/SecurityCode?transportType=EMAIL|SMS.
    pub async fn security_code(&self, key: &str, sms: bool) -> Result<CodeSent, AuthError> {
        let transport = if sms { "SMS" } else { "EMAIL" };
        match self.get("Shield/SecurityCode", key, &[("transportType", transport)]).await {
            Ok(body) => Ok(CodeSent::Sent { domain: str_field(&body, "domain") }),
            Err(e) if e.reason() == Some("SECUREDBYOTP") => Ok(CodeSent::Otp),
            Err(e) if e.reason() == Some("NONEEDTOBESECURED") => Ok(CodeSent::NotNeeded),
            Err(e) => Err(e),
        }
    }

    /// GET Shield/ValidateCode or Shield/ValidateOtp -> certificate object `{id, encodedCertificate, ...}`.
    pub async fn validate(&self, key: &str, otp: bool, code: &str, hm1: &str, hm2: &str, name: &str) -> Result<Value, AuthError> {
        let path = if otp { "Shield/ValidateOtp" } else { "Shield/ValidateCode" };
        let game = crate::LAUNCHER_GAME_ID.to_string();
        let query = [("game_id", game.as_str()), ("code", code), ("hm1", hm1), ("hm2", hm2), ("name", name)];
        self.get(path, key, &query).await
    }

    /// GET Account/CreateToken?game=&certificate_id=&certificate_hash= -> token.
    pub async fn create_token(&self, key: &str, game: i32, cert: Option<(&str, &str)>) -> Result<String, AuthError> {
        let game = game.to_string();
        let (id, hash) = cert.unwrap_or(("", ""));
        let body = self
            .get("Account/CreateToken", key, &[("game", &game), ("certificate_id", id), ("certificate_hash", hash)])
            .await?;
        str_field(&body, "token").ok_or_else(|| AuthError::InvalidResponse("token absent".into()))
    }
}

fn net_err(e: reqwest::Error) -> AuthError {
    // Strip the URL: it may carry query secrets (hm1/hm2, codes) – never log them.
    AuthError::Network(e.without_url().to_string())
}

/// Success bodies: JSON or empty. serde_json keeps every integer up to u64 exact (no f64 rounding),
/// which covers account ids (i64); larger values (session ids) fall back to f64 and are unused.
fn parse_json(bytes: &[u8]) -> Result<Value, AuthError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidResponse("JSON invalide".into()))
}

pub(crate) fn error_from_body(status: u16, bytes: &[u8]) -> AuthError {
    let body: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let reason = ["reason", "message", "error"]
        .iter()
        .find_map(|k| str_field(&body, k))
        .filter(|s| !s.is_empty());
    if reason.as_deref() == Some("ANONYMOUS_IP_FORBIDDEN") {
        return AuthError::AnonymousIpForbidden;
    }
    AuthError::Haapi { status, reason }
}

pub(crate) fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Number or string scalar rendered as a string.
pub(crate) fn scalar_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Account id as i64 from a number or digit string.
pub(crate) fn id_field(v: &Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_mapping() {
        assert!(matches!(
            error_from_body(403, br#"{"reason":"ANONYMOUS_IP_FORBIDDEN"}"#),
            AuthError::AnonymousIpForbidden
        ));
        let e = error_from_body(403, br#"{"message":"SECUREDBYOTP"}"#);
        assert_eq!(e.reason(), Some("SECUREDBYOTP"));
        assert_eq!(e.status(), Some(403));
        let e = error_from_body(500, b"<html>");
        assert!(matches!(e, AuthError::Haapi { status: 500, reason: None }));
        assert_eq!(
            error_from_body(400, br#"{"reason":"CODEBADCODE"}"#).to_string(),
            "Code de sécurité incorrect"
        );
    }

    #[test]
    fn big_ids_are_exact() {
        let v = parse_json(br#"{"id": 9007199254740993, "s": "123"}"#).unwrap();
        assert_eq!(id_field(&v, "id"), Some(9_007_199_254_740_993));
        assert_eq!(id_field(&v, "s"), Some(123));
        assert_eq!(parse_json(b"  ").unwrap(), Value::Null);
    }
}
