//! Local persistence: `accounts.json` (non-secret metadata) and `secrets/<id>.bin`
//! (DPAPI CurrentUser-encrypted JSON `{key, refreshToken, refreshDate, certificate?}`).
use super::AuthError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Optional DPAPI entropy: binds blobs to this application (other CurrentUser apps need it too).
const DPAPI_ENTROPY: &[u8] = b"dofus-launcher/auth/v1";

/// Non-secret account metadata shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub id: i64,
    pub login: String,
    pub nickname: Option<String>,
    pub tag: Option<String>,
    pub security: Vec<String>,
    pub is_main: bool,
    pub active: bool,
    /// CreateToken answered 403 (Zaap `isErrored`).
    pub errored: bool,
    /// Session revoked/expired (401, failed restore) or secrets unreadable: browser login needed.
    #[serde(default)]
    pub needs_login: bool,
    /// Haapi account `type` (e.g. "ANKAMA").
    #[serde(default)]
    pub account_type: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// Persisted record: UI info + non-secret Haapi account object (from SignOnWithApiKey).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountRecord {
    pub info: AccountInfo,
    /// Raw `account` object of SignOnWithApiKey / Account/Account (profile fields only).
    #[serde(default)]
    pub haapi: serde_json::Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountsFile {
    version: u32,
    accounts: Vec<AccountRecord>,
}

/// Secret material of one account. `Debug` is redacted.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Secrets {
    pub key: String,
    pub refresh_token: Option<String>,
    /// Unix epoch milliseconds of the last successful RefreshApiKey.
    pub refresh_date: Option<i64>,
    /// Shield certificate object as returned by ValidateCode/ValidateOtp.
    pub certificate: Option<serde_json::Value>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("key", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("refresh_date", &self.refresh_date)
            .field("certificate", &self.certificate.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Secrets {
    /// `(certificate_id, encodedCertificate)` if a usable certificate is stored.
    pub fn certificate_parts(&self) -> Option<(String, String)> {
        let c = self.certificate.as_ref()?;
        let id = match c.get("id")? {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => return None,
        };
        let enc = c.get("encodedCertificate")?.as_str()?.to_string();
        Some((id, enc))
    }
}

pub(crate) struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir }
    }
    fn accounts_path(&self) -> PathBuf {
        self.dir.join("accounts.json")
    }
    fn secret_path(&self, id: i64) -> PathBuf {
        self.dir.join("secrets").join(format!("{id}.bin"))
    }

    pub fn load_accounts(&self) -> Result<Vec<AccountRecord>, AuthError> {
        match std::fs::read(self.accounts_path()) {
            Ok(bytes) => {
                let file: AccountsFile = serde_json::from_slice(&bytes)
                    .map_err(|e| AuthError::Storage(format!("accounts.json illisible : {e}")))?;
                Ok(file.accounts)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_accounts(&self, accounts: &[AccountRecord]) -> Result<(), AuthError> {
        let file = AccountsFile { version: 1, accounts: accounts.to_vec() };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| AuthError::Storage(e.to_string()))?;
        crate::paths::write_atomic(&self.accounts_path(), &bytes)?;
        Ok(())
    }

    /// `Ok(None)` when no file exists; `Err` when it exists but cannot be decrypted/parsed.
    pub fn load_secrets(&self, id: i64) -> Result<Option<Secrets>, AuthError> {
        let blob = match std::fs::read(self.secret_path(id)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let plain = dpapi::unprotect(&blob, DPAPI_ENTROPY)?;
        let secrets = serde_json::from_slice(&plain)
            .map_err(|_| AuthError::Storage(format!("secrets du compte {id} illisibles")))?;
        Ok(Some(secrets))
    }

    pub fn save_secrets(&self, id: i64, secrets: &Secrets) -> Result<(), AuthError> {
        let plain = serde_json::to_vec(secrets).map_err(|e| AuthError::Storage(e.to_string()))?;
        let blob = dpapi::protect(&plain, DPAPI_ENTROPY)?;
        crate::paths::write_atomic(&self.secret_path(id), &blob)?;
        Ok(())
    }

    pub fn delete_secrets(&self, id: i64) -> Result<(), AuthError> {
        match std::fs::remove_file(self.secret_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    #[allow(dead_code)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Windows DPAPI, CurrentUser scope, no UI.
pub(crate) mod dpapi {
    use super::AuthError;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    /// Copies the system-allocated output blob and frees it.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: DPAPI returned `cbData` valid bytes at `pbData`, allocated with LocalAlloc.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
            v
        }
    }

    pub fn protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, AuthError> {
        let input = blob(data);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input/entropy blobs borrow live slices (DPAPI does not write them); out is owned.
        unsafe {
            CryptProtectData(&input, windows::core::PCWSTR::null(), Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| AuthError::Crypto(format!("DPAPI : {}", e.message())))?;
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, AuthError> {
        let input = blob(data);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as above.
        unsafe {
            CryptUnprotectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| AuthError::Crypto(format!("DPAPI : {}", e.message())))?;
            Ok(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip() {
        let data = b"{\"key\":\"secret-value\"}";
        let blob = dpapi::protect(data, DPAPI_ENTROPY).unwrap();
        assert!(!blob.windows(12).any(|w| w == b"secret-value"));
        assert_eq!(dpapi::unprotect(&blob, DPAPI_ENTROPY).unwrap(), data);
        assert!(dpapi::unprotect(&blob, b"other-entropy").is_err());
        assert_eq!(dpapi::unprotect(&dpapi::protect(b"", DPAPI_ENTROPY).unwrap(), DPAPI_ENTROPY).unwrap(), b"");
    }

    #[test]
    fn secrets_round_trip_and_redaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        assert!(store.load_secrets(42).unwrap().is_none());
        let s = Secrets {
            key: "api-key-value".into(),
            refresh_token: Some("refresh-value".into()),
            refresh_date: Some(1_700_000_000_000),
            certificate: Some(serde_json::json!({"id": 123456789012_i64, "encodedCertificate": "QUJD"})),
        };
        store.save_secrets(42, &s).unwrap();
        let raw = std::fs::read(dir.path().join("secrets").join("42.bin")).unwrap();
        assert!(!raw.windows(13).any(|w| w == b"api-key-value"));
        let back = store.load_secrets(42).unwrap().unwrap();
        assert_eq!(back.key, "api-key-value");
        assert_eq!(back.certificate_parts(), Some(("123456789012".into(), "QUJD".into())));
        let dbg = format!("{back:?}");
        assert!(!dbg.contains("api-key-value") && !dbg.contains("refresh-value") && !dbg.contains("QUJD"));
        store.delete_secrets(42).unwrap();
        assert!(store.load_secrets(42).unwrap().is_none());
        store.delete_secrets(42).unwrap();
    }

    #[test]
    fn accounts_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        assert!(store.load_accounts().unwrap().is_empty());
        let rec = AccountRecord {
            info: AccountInfo {
                id: 9_007_199_254_740_993, // > 2^53: must survive exactly
                login: "joueur@example.com".into(),
                nickname: Some("Joueur".into()),
                tag: Some("1234".into()),
                security: vec!["SHIELD".into()],
                is_main: true,
                active: true,
                errored: false,
                needs_login: false,
                account_type: Some("ANKAMA".into()),
                avatar_url: None,
            },
            haapi: serde_json::json!({"id": 9_007_199_254_740_993_i64}),
        };
        store.save_accounts(std::slice::from_ref(&rec)).unwrap();
        let back = store.load_accounts().unwrap();
        assert_eq!(back[0].info, rec.info);
        assert_eq!(back[0].haapi["id"].as_i64(), Some(9_007_199_254_740_993));
    }
}
