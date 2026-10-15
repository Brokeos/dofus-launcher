//! Shield machine fingerprint (hm1/hm2) and certificate hash, byte-compatible with Zaap 3.15.5
//! (spec 01 sections 3.4 and 3.5) so certificates issued to the official launcher stay valid.
use aes::cipher::{BlockModeDecrypt, KeyInit, block_padding::Pkcs7};
use base64::Engine;
use sha2::{Digest, Sha256};

/// Inputs of `createHmEncoders`, in Zaap order (arch, platform, machineId, username, osVersion, ram).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub arch: String,
    pub platform: String,
    /// `sha256_hex(MachineGuid)`; `None` when the registry read fails (Zaap omits the part).
    pub machine_id: Option<String>,
    pub username: String,
    pub os_version: String,
    pub ram_mib: String,
}

impl Fingerprint {
    /// Current machine, as Node would report it from the x64 Zaap build on Windows.
    pub fn current() -> Self {
        Fingerprint {
            arch: "x64".into(),
            platform: "win32".into(),
            machine_id: machine_id(),
            username: username(),
            os_version: os_version(),
            ram_mib: ram_power_of_two_mib(total_memory_bytes()),
        }
    }

    /// `(hm1, hm2)`: hm1 = sha256_hex(parts.join(""))[0..32], hm2 = reverse(hm1).
    pub fn hm_pair(&self) -> (String, String) {
        let joined = [
            self.arch.as_str(),
            self.platform.as_str(),
            self.machine_id.as_deref().unwrap_or(""),
            self.username.as_str(),
            self.os_version.as_str(),
            self.ram_mib.as_str(),
        ]
        .concat();
        let hm1 = sha256_hex(joined.as_bytes())[..32].to_string();
        let hm2 = hm1.chars().rev().collect();
        (hm1, hm2)
    }
}

/// `(hm1, hm2)` of the current machine.
pub fn hm_pair() -> (String, String) {
    Fingerprint::current().hm_pair()
}

/// `sha256_hex(hm1 + utf8(AES-256-ECB-decrypt(key = utf8(hm2), base64(enc_b64), PKCS#7)))`.
pub fn certificate_hash(enc_b64: &str, hm1: &str, hm2: &str) -> anyhow::Result<String> {
    Ok(certificate_hash_inner(enc_b64, hm1, hm2)?)
}

pub(crate) fn certificate_hash_inner(enc_b64: &str, hm1: &str, hm2: &str) -> Result<String, super::AuthError> {
    use super::AuthError::Crypto;
    let mut data = base64::engine::general_purpose::STANDARD
        .decode(enc_b64.trim())
        .map_err(|_| Crypto("certificat Shield illisible (base64)".into()))?;
    let dec = ecb::Decryptor::<aes::Aes256>::new_from_slice(hm2.as_bytes())
        .map_err(|_| Crypto("empreinte machine invalide (hm2 doit faire 32 octets)".into()))?;
    let plain = dec
        .decrypt_padded::<Pkcs7>(&mut data)
        .map_err(|_| Crypto("certificat Shield lié à une autre machine".into()))?;
    // Node `Buffer.toString("utf8")` replaces invalid sequences with U+FFFD, like from_utf8_lossy.
    let text = String::from_utf8_lossy(plain);
    Ok(sha256_hex(format!("{hm1}{text}").as_bytes()))
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// node-machine-id: `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` (64-bit view), whitespace
/// stripped, lowercased, then sha256 hex.
fn machine_id() -> Option<String> {
    use winreg::{RegKey, enums::*};
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Cryptography", KEY_READ | KEY_WOW64_64KEY)
        .ok()?;
    let guid: String = key.get_value("MachineGuid").ok()?;
    let norm: String = guid.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    if norm.is_empty() { None } else { Some(sha256_hex(norm.as_bytes())) }
}

/// Node `os.userInfo().username` on Windows (libuv uses `GetUserNameW`).
fn username() -> String {
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    use windows::core::PWSTR;
    let mut buf = vec![0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and length describe a valid writable UTF-16 buffer.
    let ok = unsafe { GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok();
    if ok && len > 0 {
        // len includes the terminating NUL on success.
        String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)])
    } else {
        std::env::var("USERNAME").unwrap_or_default()
    }
}

/// `parseFloat(major + "." + minor)` of `os.release()`, formatted like a JS number ("10", "6.1").
fn os_version() -> String {
    use winreg::{RegKey, enums::*};
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ | KEY_WOW64_64KEY)
        .ok();
    let major: Option<u32> = key.as_ref().and_then(|k| k.get_value("CurrentMajorVersionNumber").ok());
    let minor: u32 = key.as_ref().and_then(|k| k.get_value("CurrentMinorVersionNumber").ok()).unwrap_or(0);
    match major {
        Some(m) => js_version(m, minor),
        // Pre-Windows 10 keys lack the DWORDs: CurrentVersion is "6.1" / "6.3".
        None => key
            .and_then(|k| k.get_value::<String, _>("CurrentVersion").ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|f| f.to_string())
            .unwrap_or_else(|| "10".into()),
    }
}

fn js_version(major: u32, minor: u32) -> String {
    // parseFloat("10.0") === 10 -> "10"; parseFloat("6.1") -> "6.1". f64 Display matches JS here.
    format!("{major}.{minor}").parse::<f64>().map(|f| f.to_string()).unwrap_or_else(|_| major.to_string())
}

/// Node `os.totalmem()` (libuv: `GlobalMemoryStatusEx().ullTotalPhys`, same source as sysinfo).
fn total_memory_bytes() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory()
}

/// `2 ** Math.round(Math.log2(bytes / 1024 / 1024))`, as a JS integer string.
pub(crate) fn ram_power_of_two_mib(bytes: u64) -> String {
    if bytes == 0 {
        return "0".into();
    }
    let mib = bytes as f64 / 1024.0 / 1024.0;
    // Math.round rounds .5 toward +inf; for this domain (positive or small negative) use floor(x + 0.5).
    let exp = (mib.log2() + 0.5).floor();
    let v = 2f64.powf(exp);
    if v >= 1.0 { format!("{}", v as u64) } else { v.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockModeEncrypt;

    fn sample() -> Fingerprint {
        Fingerprint {
            arch: "x64".into(),
            platform: "win32".into(),
            machine_id: Some(sha256_hex(b"0e1f2a3b-4c5d-6e7f-8091-a2b3c4d5e6f7")),
            username: "Joueur".into(),
            os_version: "10".into(),
            ram_mib: "16384".into(),
        }
    }

    #[test]
    fn hm_pair_format_and_derivation() {
        let fp = sample();
        let (hm1, hm2) = fp.hm_pair();
        assert_eq!(hm1.len(), 32);
        assert!(hm1.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(hm2, hm1.chars().rev().collect::<String>());
        let joined = format!("x64win32{}Joueur1016384", fp.machine_id.as_deref().unwrap());
        assert_eq!(hm1, &sha256_hex(joined.as_bytes())[..32]);
        // machineId omitted when unavailable
        let mut no_id = fp.clone();
        no_id.machine_id = None;
        assert_eq!(no_id.hm_pair().0, &sha256_hex(b"x64win32Joueur1016384")[..32]);
    }

    #[test]
    fn current_machine_hm_pair_is_stable() {
        let fp = Fingerprint::current();
        assert_eq!(fp.arch, "x64");
        assert!(fp.machine_id.as_ref().is_some_and(|m| m.len() == 64));
        assert!(!fp.username.is_empty());
        let ram: u64 = fp.ram_mib.parse().unwrap();
        assert!(ram.is_power_of_two());
        assert_eq!(hm_pair(), hm_pair());
    }

    #[test]
    fn ram_rounding_matches_js() {
        assert_eq!(ram_power_of_two_mib(16 * 1024 * 1024 * 1024), "16384");
        // 15.8 GiB usable -> 16384
        assert_eq!(ram_power_of_two_mib(16_965_000_000), "16384");
        // 24 GiB: log2(24576)=14.58 -> 2^15
        assert_eq!(ram_power_of_two_mib(24 * 1024 * 1024 * 1024), "32768");
        assert_eq!(ram_power_of_two_mib(0), "0");
    }

    #[test]
    fn js_version_format() {
        assert_eq!(js_version(10, 0), "10");
        assert_eq!(js_version(6, 1), "6.1");
        assert_eq!(js_version(6, 3), "6.3");
    }

    #[test]
    fn certificate_hash_round_trip() {
        let (hm1, hm2) = sample().hm_pair();
        let plaintext = "certificat-de-test-é-0123456789-abcdefghijklmnopqrstuvwxyz";
        let enc = ecb::Encryptor::<aes::Aes256>::new_from_slice(hm2.as_bytes()).unwrap();
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext.as_bytes());
        let ct = enc.encrypt_padded::<Pkcs7>(&mut buf, plaintext.len()).unwrap().to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&ct);

        let hash = certificate_hash(&b64, &hm1, &hm2).unwrap();
        assert_eq!(hash, sha256_hex(format!("{hm1}{plaintext}").as_bytes()));
        assert_eq!(hash.len(), 64);

        // Wrong machine (different hm2) must fail cleanly, not hash garbage silently.
        let (_, other_hm2) = Fingerprint { username: "Autre".into(), ..sample() }.hm_pair();
        assert!(certificate_hash(&b64, &hm1, &other_hm2).is_err());
        assert!(certificate_hash("pas du base64 !", &hm1, &hm2).is_err());
        assert!(certificate_hash(&b64, &hm1, "court").is_err());
    }
}
