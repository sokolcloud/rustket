//! Private key backup envelopes (`users.e2e.private_key`, §3.3).

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeDecrypt, KeyIvInit};
use aes_gcm::aead::{Aead, KeyInit};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::codec::{b64_decode, b64_encode, latin1_bytes};
use super::{E2eError, PassphrasePolicyReport, Result, random_bytes};

/// PBKDF2 iterations of v1 envelopes.
const V1_ITERATIONS: u32 = 1000;
/// PBKDF2 iterations written by the web client (and Rustket).
const V2_ITERATIONS: u32 = 100_000;
/// Refuse absurd iteration counts from the server (a hostile value would hang
/// the unlock).
const MAX_ITERATIONS: u32 = 10_000_000;
/// Minimum length of a user-chosen passphrase (web `PASSPHRASE_POLICY`).
pub const PASSPHRASE_MIN_LENGTH: usize = 30;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
#[cfg(test)]
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

/// A parsed `users.e2e.private_key` string.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateKeyEnvelope {
    pub iv: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub salt: String,
    pub iterations: u32,
    /// `true` for the legacy `{"$binary": …}` form.
    pub legacy_v1: bool,
}

impl std::fmt::Debug for PrivateKeyEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateKeyEnvelope")
            .field("iterations", &self.iterations)
            .field("legacy_v1", &self.legacy_v1)
            .finish_non_exhaustive()
    }
}

fn derive_key(password: &[u8], salt: &[u8], iterations: u32) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, iterations, &mut *key);
    key
}

/// Salt/passphrase bytes the web would use (Latin-1), else UTF-8.
fn web_bytes(s: &str) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(latin1_bytes(s).unwrap_or_else(|| s.as_bytes().to_vec()))
}

impl PrivateKeyEnvelope {
    /// Parses v2 (`{iv, ciphertext, salt, iterations}`) or v1 (`{"$binary"}`).
    /// Like RN, a v2 envelope without `salt` falls back to the user id.
    pub fn parse(stored: &str, user_id: &str) -> Result<Self> {
        let v: Value = serde_json::from_str(stored)?;
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        if let (Some(iv), Some(ct), Some(iterations)) = (
            s("iv"),
            s("ciphertext"),
            v.get("iterations").and_then(Value::as_u64),
        ) {
            let iterations = u32::try_from(iterations)
                .ok()
                .filter(|i| (1..=MAX_ITERATIONS).contains(i))
                .ok_or(E2eError::Format("invalid iteration count"))?;
            return Ok(Self {
                iv: b64_decode(iv)?,
                ciphertext: b64_decode(ct)?,
                salt: s("salt").unwrap_or(user_id).to_owned(),
                iterations,
                legacy_v1: false,
            });
        }
        if let Some(bin) = s("$binary") {
            let mut bytes = b64_decode(bin)?;
            if bytes.len() < 32 {
                return Err(E2eError::Format("v1 private key too short"));
            }
            let ciphertext = bytes.split_off(16);
            return Ok(Self {
                iv: bytes,
                ciphertext,
                salt: user_id.to_owned(),
                iterations: V1_ITERATIONS,
                legacy_v1: true,
            });
        }
        Err(E2eError::Format("invalid private key format"))
    }

    /// Decrypts the private JWK string (**blocking**: PBKDF2, up to two
    /// derivations). The passphrase is tried as Latin-1 bytes (web) and as UTF-8
    /// (RN) when they differ. Any failure, including a CBC result that is not a
    /// JSON object, is [`E2eError::WrongPassword`].
    pub fn decrypt(&self, password: &str) -> Result<Zeroizing<String>> {
        let mut candidates: Vec<Zeroizing<Vec<u8>>> = Vec::with_capacity(2);
        if let Some(l1) = latin1_bytes(password) {
            candidates.push(Zeroizing::new(l1));
        }
        let utf8 = Zeroizing::new(password.as_bytes().to_vec());
        if candidates.first().is_none_or(|c| **c != *utf8) {
            candidates.push(utf8);
        }
        let salt = web_bytes(&self.salt);
        for pw in &candidates {
            let key = derive_key(pw, &salt, self.iterations);
            if let Some(plain) = self.decrypt_with(&key)? {
                return Ok(plain);
            }
        }
        Err(E2eError::WrongPassword)
    }

    /// `Ok(None)` when the key is wrong.
    fn decrypt_with(&self, key: &[u8; 32]) -> Result<Option<Zeroizing<String>>> {
        let plain = match self.iv.len() {
            16 => Aes256CbcDec::new_from_slices(key, &self.iv)
                .map_err(|_| E2eError::Format("invalid key/iv length"))?
                .decrypt_padded_vec::<Pkcs7>(&self.ciphertext)
                .ok(),
            12 => aes_gcm::Aes256Gcm::new_from_slice(key)
                .map_err(|_| E2eError::Format("invalid key length"))?
                .decrypt(
                    self.iv
                        .as_slice()
                        .try_into()
                        .map_err(|_| E2eError::Format("iv"))?,
                    self.ciphertext.as_slice(),
                )
                .ok(),
            _ => return Err(E2eError::Format("unsupported iv length")),
        };
        let Some(plain) = plain.map(Zeroizing::new) else {
            return Ok(None);
        };
        let Ok(text) = std::str::from_utf8(&plain) else {
            return Ok(None);
        };
        // Only `kty` is materialized: the private members are skipped, not copied
        // into a non-zeroized `Value`.
        #[derive(serde::Deserialize)]
        struct KtyOnly {
            #[allow(dead_code)]
            kty: String,
        }
        let looks_like_jwk = serde_json::from_str::<KtyOnly>(text).is_ok();
        Ok(looks_like_jwk.then(|| Zeroizing::new(text.to_owned())))
    }
}

/// Printable ASCII only (keeps the passphrase decryptable by every client).
fn check_charset(password: &str) -> Result<()> {
    if password.is_empty() || !password.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(E2eError::PasswordCharset);
    }
    Ok(())
}

/// Encrypts the private JWK into a v2 envelope string, salt
/// `v2:<userId>:<random UUID>`, 100 000 iterations (**blocking**). The
/// passphrase must be printable ASCII.
pub fn encrypt_v2(private_jwk: &str, password: &str, user_id: &str) -> Result<String> {
    encrypt_v2_with(
        private_jwk,
        password,
        user_id,
        &uuid::Uuid::new_v4().to_string(),
        random_bytes::<12>(),
    )
}

/// [`encrypt_v2`] with a fixed salt UUID and IV (test vectors).
pub fn encrypt_v2_with(
    private_jwk: &str,
    password: &str,
    user_id: &str,
    salt_uuid: &str,
    iv: [u8; 12],
) -> Result<String> {
    check_charset(password)?;
    let salt = format!("v2:{user_id}:{salt_uuid}");
    let key = derive_key(password.as_bytes(), &web_bytes(&salt), V2_ITERATIONS);
    let ct = aes_gcm::Aes256Gcm::new_from_slice(&*key)
        .map_err(|_| E2eError::Format("invalid key length"))?
        .encrypt((&iv).into(), private_jwk.as_bytes())
        .map_err(|_| E2eError::Format("encryption failed"))?;
    Ok(json!({
        "iv": b64_encode(&iv),
        "ciphertext": b64_encode(&ct),
        "salt": salt,
        "iterations": V2_ITERATIONS,
    })
    .to_string())
}

/// Writes a v1 envelope (tests only; Rustket never writes v1).
#[cfg(test)]
pub(crate) fn encrypt_v1_with(
    private_jwk: &str,
    password: &str,
    user_id: &str,
    iv: [u8; 16],
) -> String {
    use aes::cipher::BlockModeEncrypt;
    let key = derive_key(password.as_bytes(), user_id.as_bytes(), V1_ITERATIONS);
    let ct = Aes256CbcEnc::new_from_slices(&*key, &iv)
        .map(|c| c.encrypt_padded_vec::<Pkcs7>(private_jwk.as_bytes()))
        .unwrap_or_default();
    let mut blob = iv.to_vec();
    blob.extend_from_slice(&ct);
    json!({ "$binary": b64_encode(&blob) }).to_string()
}

/// Checks a user-chosen passphrase: printable ASCII ([`E2eError::PasswordCharset`])
/// and the web policy ([`E2eError::PasswordPolicy`]): at least 30 characters,
/// one lowercase, one uppercase, one digit, one special character.
pub fn validate_new_passphrase(password: &str) -> Result<()> {
    check_charset(password)?;
    let report = PassphrasePolicyReport {
        too_short: password.chars().count() < PASSPHRASE_MIN_LENGTH,
        missing_lowercase: !password.bytes().any(|b| b.is_ascii_lowercase()),
        missing_uppercase: !password.bytes().any(|b| b.is_ascii_uppercase()),
        missing_digit: !password.bytes().any(|b| b.is_ascii_digit()),
        missing_special: !password
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && b != b' '),
    };
    if report.is_ok() {
        Ok(())
    } else {
        Err(E2eError::PasswordPolicy(report))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::e2e::user_key::tests::fixture;
    use aes::cipher::BlockModeEncrypt;

    #[test]
    fn pbkdf2_vectors() {
        let k = derive_key(b"minus mobile dexter forest elvis", b"userE2EE", 1000);
        assert_eq!(
            hex::encode(*k),
            "7feb6c85cd105a8bb51c01a8604d357143b1b251ef6b854051c3c90cae233070"
        );
        let k = derive_key(
            b"prize fluid crystal small jaguar lunar bonus absent destroy settle carbon ignore",
            b"v2:BFqA4FAHKX4qEywfJ:5b36c4cc-4e7a-497d-b4b0-a1df863c65c4",
            100_000,
        );
        assert_eq!(
            hex::encode(*k),
            "8443db571f0dfbfd1acfa2770a84bd76402ce02daad360fad086cdc75ca578ec"
        );
    }

    #[test]
    fn upstream_v1_and_v2_envelopes_decrypt() {
        for name in ["keychain_pwdv1web.json", "keychain_pwdv2web.json"] {
            let f = fixture(name);
            let s = |k: &str| f[k].as_str().unwrap();
            let env = PrivateKeyEnvelope::parse(s("persisted"), s("userId")).unwrap();
            assert_eq!(env.legacy_v1, name.contains("v1"));
            assert_eq!(&*env.decrypt(s("passphrase")).unwrap(), s("private_key"));
            assert_eq!(
                env.decrypt("wrong passphrase"),
                Err(E2eError::WrongPassword)
            );
        }
    }

    const PLAIN: &str = r#"{"kty":"RSA","test":"rustket"}"#;

    #[test]
    fn v2_write_vector() {
        let iv: [u8; 12] = hex::decode("000102030405060708090a0b")
            .unwrap()
            .try_into()
            .unwrap();
        let s = encrypt_v2_with(
            PLAIN,
            "Rustket-E2EE-Passphrase-#2026-very-long",
            "UserIdRustket0001",
            "00000000-0000-4000-8000-000000000001",
            iv,
        )
        .unwrap();
        let expected: Value = serde_json::from_str(r#"{"iv":"AAECAwQFBgcICQoL","ciphertext":"vYyaUw2oCZd6wrL7t5nLq2SFVWUeo0H/mUfquMt7gkVLIyt0qisdf/pWrCbf8w==","salt":"v2:UserIdRustket0001:00000000-0000-4000-8000-000000000001","iterations":100000}"#).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), expected);
        let env = PrivateKeyEnvelope::parse(&s, "ignored").unwrap();
        assert_eq!(
            &*env
                .decrypt("Rustket-E2EE-Passphrase-#2026-very-long")
                .unwrap(),
            PLAIN
        );
    }

    #[test]
    fn v1_read_vector() {
        let stored =
            r#"{"$binary":"BwcHBwcHBwcHBwcHBwcHB+YPOUcoV6V49mKRuFiH/WtWzRsivURxbCgtArMK4g72"}"#;
        assert_eq!(
            encrypt_v1_with(PLAIN, "legacy-pass", "UserIdRustket0001", [7; 16]),
            stored
        );
        let env = PrivateKeyEnvelope::parse(stored, "UserIdRustket0001").unwrap();
        assert_eq!(&*env.decrypt("legacy-pass").unwrap(), PLAIN);
        let other_user = PrivateKeyEnvelope::parse(stored, "someone").unwrap();
        assert_eq!(
            other_user.decrypt("legacy-pass"),
            Err(E2eError::WrongPassword)
        );
    }

    #[test]
    fn random_v2_roundtrip_and_fresh_salt() {
        let a = encrypt_v2(PLAIN, "pass word", "uid").unwrap();
        let b = encrypt_v2(PLAIN, "pass word", "uid").unwrap();
        assert_ne!(a, b);
        let env = PrivateKeyEnvelope::parse(&a, "uid").unwrap();
        assert!(env.salt.starts_with("v2:uid:"));
        assert_eq!(env.salt.len(), "v2:uid:".len() + 36);
        assert_eq!(env.iv.len(), 12);
        assert_eq!(&*env.decrypt("pass word").unwrap(), PLAIN);
    }

    #[test]
    fn non_ascii_passwords() {
        assert_eq!(
            encrypt_v2(PLAIN, "пароль", "u"),
            Err(E2eError::PasswordCharset)
        );
        assert_eq!(encrypt_v2(PLAIN, "", "u"), Err(E2eError::PasswordCharset));
        // RN encodes U+0080..U+00FF as UTF-8, the web as Latin-1: both decrypt.
        let rn_key = derive_key("café".as_bytes(), b"uid", V1_ITERATIONS);
        let iv = [1u8; 16];
        let ct = Aes256CbcEnc::new_from_slices(&*rn_key, &iv)
            .unwrap()
            .encrypt_padded_vec::<Pkcs7>(PLAIN.as_bytes());
        let mut blob = iv.to_vec();
        blob.extend_from_slice(&ct);
        let stored = json!({ "$binary": b64_encode(&blob) }).to_string();
        let env = PrivateKeyEnvelope::parse(&stored, "uid").unwrap();
        assert_eq!(&*env.decrypt("café").unwrap(), PLAIN);
        let web_key = derive_key(&latin1_bytes("café").unwrap(), b"uid", V1_ITERATIONS);
        let ct = Aes256CbcEnc::new_from_slices(&*web_key, &iv)
            .unwrap()
            .encrypt_padded_vec::<Pkcs7>(PLAIN.as_bytes());
        let mut blob = iv.to_vec();
        blob.extend_from_slice(&ct);
        let stored = json!({ "$binary": b64_encode(&blob) }).to_string();
        let env = PrivateKeyEnvelope::parse(&stored, "uid").unwrap();
        assert_eq!(&*env.decrypt("café").unwrap(), PLAIN);
    }

    #[test]
    fn malformed_envelopes() {
        for bad in [
            "",
            "null",
            "{}",
            "[1]",
            r#"{"$binary": 5}"#,
            r#"{"$binary": "!!!"}"#,
            r#"{"$binary": "AAAA"}"#,
            r#"{"iv":"AAAA","ciphertext":"AAAA","iterations":0}"#,
            r#"{"iv":"AAAA","ciphertext":"AAAA","iterations":4294967296}"#,
            r#"{"iv":"AAAA","ciphertext":"AAAA","iterations":20000000}"#,
            r#"{"iv":"!!","ciphertext":"AAAA","iterations":1}"#,
        ] {
            assert!(PrivateKeyEnvelope::parse(bad, "u").is_err(), "{bad}");
        }
        // Salt missing: falls back to the user id (RN behaviour).
        let env =
            PrivateKeyEnvelope::parse(r#"{"iv":"AAAA","ciphertext":"AAAA","iterations":1}"#, "uid")
                .unwrap();
        assert_eq!(env.salt, "uid");
        // 3-byte IV: neither CBC nor GCM.
        assert!(matches!(env.decrypt("x"), Err(E2eError::Format(_))));
        // Truncated GCM ciphertext: wrong password, no panic.
        let env = PrivateKeyEnvelope::parse(
            r#"{"iv":"AAECAwQFBgcICQoL","ciphertext":"AA==","salt":"s","iterations":1}"#,
            "u",
        )
        .unwrap();
        assert_eq!(env.decrypt("x"), Err(E2eError::WrongPassword));
        // CBC ciphertext that is not a multiple of the block size.
        let env = PrivateKeyEnvelope::parse(
            r#"{"iv":"AAECAwQFBgcICQoLDA0ODw==","ciphertext":"AAAA","salt":"s","iterations":1}"#,
            "u",
        )
        .unwrap();
        assert_eq!(env.decrypt("x"), Err(E2eError::WrongPassword));
    }

    #[test]
    fn passphrase_policy() {
        assert!(validate_new_passphrase("Abcdefghijklmnopqrstuvwxyz012#").is_ok());
        assert!(validate_new_passphrase("Abcdefghijklmnopqrstuvwxyz01é#").is_err());
        let Err(E2eError::PasswordPolicy(r)) = validate_new_passphrase("short") else {
            panic!()
        };
        assert!(r.too_short && r.missing_uppercase && r.missing_digit && r.missing_special);
        assert!(!r.missing_lowercase);
        // Space is not a special character.
        let Err(E2eError::PasswordPolicy(r)) =
            validate_new_passphrase("Abcdefghijklmnopqrstuvwxyz 0123")
        else {
            panic!()
        };
        assert!(r.missing_special && !r.too_short);
    }
}
