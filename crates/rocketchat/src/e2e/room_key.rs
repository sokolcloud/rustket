//! Room keys (AES-256-GCM, legacy AES-128-CBC) as JWK strings, their RSA
//! wrapping and message content encryption (§3.5–§3.7).

use std::fmt;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use aes_gcm::aead::{Aead, KeyInit};
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;
use zeroize::{Zeroize, Zeroizing};

use super::codec::{EncryptedContent, WrappedKey, b64url_decode, b64url_encode, latin1_bytes};
use super::user_key::{UserPrivateKey, UserPublicKey};
use super::{E2eError, Result, random_bytes};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// The AES mode of a room key. The mode always follows the key, never the
/// envelope version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomKeyAlg {
    /// AES-256-GCM, 12-byte IV, 16-byte tag appended (current).
    A256Gcm,
    /// AES-128-CBC with PKCS#7, 16-byte IV (legacy rooms).
    A128Cbc,
}

impl RoomKeyAlg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A256Gcm => "A256GCM",
            Self::A128Cbc => "A128CBC",
        }
    }

    fn key_len(self) -> usize {
        match self {
            Self::A256Gcm => 32,
            Self::A128Cbc => 16,
        }
    }

    fn iv_len(self) -> usize {
        match self {
            Self::A256Gcm => 12,
            Self::A128Cbc => 16,
        }
    }
}

/// A symmetric room key with its key id (`kid`, opaque string).
#[derive(Clone)]
pub struct RoomKey {
    pub kid: String,
    pub alg: RoomKeyAlg,
    key: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoomKey")
            .field("kid", &self.kid)
            .field("alg", &self.alg)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct OctJwkIn {
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    k: String,
}

impl Drop for OctJwkIn {
    fn drop(&mut self) {
        self.k.zeroize();
    }
}

/// A random UUID v4 key id (the web's `crypto.randomUUID()`).
pub fn new_key_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl RoomKey {
    /// A new AES-256-GCM key with a random UUID v4 kid.
    pub fn generate() -> Self {
        Self {
            kid: new_key_id(),
            alg: RoomKeyAlg::A256Gcm,
            key: Zeroizing::new(random_bytes::<32>().to_vec()),
        }
    }

    /// From raw key bytes (16 bytes for A128CBC, 32 for A256GCM).
    pub fn from_bytes(kid: impl Into<String>, alg: RoomKeyAlg, key: &[u8]) -> Result<Self> {
        if key.len() != alg.key_len() {
            return Err(E2eError::Format("room key has the wrong length"));
        }
        Ok(Self {
            kid: kid.into(),
            alg,
            key: Zeroizing::new(key.to_vec()),
        })
    }

    /// Parses a room key JWK string; `alg` must be `A256GCM` or `A128CBC`.
    pub fn from_jwk_str(kid: impl Into<String>, jwk: &str) -> Result<Self> {
        let parsed: OctJwkIn = serde_json::from_str(jwk)?;
        if parsed.kty.as_deref().is_some_and(|k| k != "oct") {
            return Err(E2eError::Format("room key kty is not oct"));
        }
        let alg = match parsed.alg.as_deref() {
            Some("A256GCM") => RoomKeyAlg::A256Gcm,
            Some("A128CBC") => RoomKeyAlg::A128Cbc,
            other => {
                return Err(E2eError::UnsupportedAlgorithm(
                    other.unwrap_or_default().to_owned(),
                ));
            }
        };
        let key = Zeroizing::new(b64url_decode(&parsed.k)?);
        Self::from_bytes(kid, alg, &key)
    }

    /// The **room key string** (`JSON.stringify(jwk)`, alphabetical keys like a
    /// browser export) that is RSA-wrapped.
    pub fn to_jwk_string(&self) -> Zeroizing<String> {
        let mut k = b64url_encode(&self.key);
        let s = format!(
            r#"{{"alg":"{}","ext":true,"k":"{k}","key_ops":["encrypt","decrypt"],"kty":"oct"}}"#,
            self.alg.as_str()
        );
        k.zeroize();
        Zeroizing::new(s)
    }

    /// Kid of RN 7.0–7.12: first 12 hex chars of SHA-256 of the room key string.
    pub fn legacy_kid(&self) -> String {
        let hash = sha2::Sha256::digest(self.to_jwk_string().as_bytes());
        hex::encode(hash)[..12].to_owned()
    }

    /// Same key material and algorithm (kid ignored).
    pub fn same_key(&self, other: &RoomKey) -> bool {
        self.alg == other.alg && *self.key == *other.key
    }

    pub fn with_kid(mut self, kid: impl Into<String>) -> Self {
        self.kid = kid.into();
        self
    }

    /// `kid + base64(RSA-OAEP(member key, room key string))`.
    pub fn wrap_for(&self, member: &UserPublicKey) -> Result<String> {
        self.wrap_for_with_kid(member, &self.kid)
    }

    /// Wraps under an explicit kid (old keys are prefixed with their `e2eKeyId`).
    pub fn wrap_for_with_kid(&self, member: &UserPublicKey, kid: &str) -> Result<String> {
        let jwk = self.to_jwk_string();
        let bytes = Zeroizing::new(
            latin1_bytes(&jwk).ok_or(E2eError::Format("room key string is not Latin-1"))?,
        );
        let blob = member.encrypt_oaep(&bytes)?;
        Ok(WrappedKey {
            kid: kid.to_owned(),
            blob,
        }
        .to_string())
    }

    /// Unwraps a wrapped key with the user's private key (**blocking**: RSA).
    /// The returned key carries the wrapped string's kid.
    pub fn unwrap(private: &UserPrivateKey, wrapped: &str) -> Result<RoomKey> {
        let w = WrappedKey::parse(wrapped)?;
        let plain = private.decrypt_oaep(&w.blob)?;
        let text =
            std::str::from_utf8(&plain).map_err(|_| E2eError::Format("room key is not text"))?;
        Self::from_jwk_str(w.kid, text)
    }

    /// Encrypts bytes into a v2 envelope with a random IV of the key's size.
    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<EncryptedContent> {
        let iv: Vec<u8> = match self.alg {
            RoomKeyAlg::A256Gcm => random_bytes::<12>().to_vec(),
            RoomKeyAlg::A128Cbc => random_bytes::<16>().to_vec(),
        };
        self.encrypt_bytes_with_iv(plaintext, &iv)
    }

    /// [`Self::encrypt_bytes`] with a fixed IV (test vectors).
    pub fn encrypt_bytes_with_iv(&self, plaintext: &[u8], iv: &[u8]) -> Result<EncryptedContent> {
        if iv.len() != self.alg.iv_len() {
            return Err(E2eError::Format("iv has the wrong length"));
        }
        let ciphertext = match self.alg {
            RoomKeyAlg::A256Gcm => aes_gcm::Aes256Gcm::new_from_slice(&self.key)
                .map_err(|_| E2eError::Format("invalid key length"))?
                .encrypt(
                    iv.try_into().map_err(|_| E2eError::Format("iv"))?,
                    plaintext,
                )
                .map_err(|_| E2eError::Format("encryption failed"))?,
            RoomKeyAlg::A128Cbc => Aes128CbcEnc::new_from_slices(&self.key, iv)
                .map_err(|_| E2eError::Format("invalid key length"))?
                .encrypt_padded_vec::<Pkcs7>(plaintext),
        };
        Ok(EncryptedContent::V2 {
            kid: self.kid.clone(),
            iv: iv.to_vec(),
            ciphertext,
        })
    }

    /// Serializes `value` (top-level keys starting with `$` are dropped, §11.15)
    /// and encrypts it into a v2 envelope.
    pub fn encrypt_json(&self, value: &Value) -> Result<EncryptedContent> {
        let mut value = value.clone();
        if let Value::Object(o) = &mut value {
            o.retain(|k, _| !k.starts_with('$'));
        }
        let text = Zeroizing::new(serde_json::to_string(&value)?);
        self.encrypt_bytes(text.as_bytes())
    }

    /// Decrypts an envelope with this key; the mode follows the key's `alg`.
    pub fn decrypt_bytes(&self, content: &EncryptedContent) -> Result<Zeroizing<Vec<u8>>> {
        let (iv, ct) = content.parts().ok_or_else(|| {
            E2eError::UnsupportedAlgorithm(match content {
                EncryptedContent::Unsupported { algorithm } => algorithm.clone(),
                _ => String::new(),
            })
        })?;
        if iv.len() != self.alg.iv_len() {
            return Err(E2eError::Decrypt);
        }
        let plain = match self.alg {
            RoomKeyAlg::A256Gcm => aes_gcm::Aes256Gcm::new_from_slice(&self.key)
                .map_err(|_| E2eError::Decrypt)?
                .decrypt(iv.try_into().map_err(|_| E2eError::Decrypt)?, ct)
                .map_err(|_| E2eError::Decrypt)?,
            RoomKeyAlg::A128Cbc => Aes128CbcDec::new_from_slices(&self.key, iv)
                .map_err(|_| E2eError::Decrypt)?
                .decrypt_padded_vec::<Pkcs7>(ct)
                .map_err(|_| E2eError::Decrypt)?,
        };
        Ok(Zeroizing::new(plain))
    }

    /// Decrypts and parses UTF-8 JSON.
    pub fn decrypt_json(&self, content: &EncryptedContent) -> Result<Value> {
        let plain = self.decrypt_bytes(content)?;
        Ok(serde_json::from_slice(&plain)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::e2e::user_key::tests::pwdv2;
    use serde_json::json;

    const CBC_JWK: &str = r#"{"alg":"A128CBC","ext":true,"k":"qb8In0Rpa9nwSusvxxDcbQ","key_ops":["encrypt","decrypt"],"kty":"oct"}"#;
    const GCM_K: &str = "9o1xoHt4OamRJvnaLna-5akUb5L98S_iWYGGaXPZ1Yg";
    const GCM_JWK: &str = r#"{"alg":"A256GCM","ext":true,"k":"9o1xoHt4OamRJvnaLna-5akUb5L98S_iWYGGaXPZ1Yg","key_ops":["encrypt","decrypt"],"kty":"oct"}"#;

    fn cbc() -> RoomKey {
        RoomKey::from_jwk_str("32c9e7917b78", CBC_JWK).unwrap()
    }

    fn gcm(kid: &str) -> RoomKey {
        RoomKey::from_jwk_str(
            kid,
            &format!(r#"{{"kty":"oct","alg":"A256GCM","k":"{GCM_K}"}}"#),
        )
        .unwrap()
    }

    fn content(v: Value) -> EncryptedContent {
        EncryptedContent::from_value(&v).unwrap()
    }

    #[test]
    fn jwk_strings_and_legacy_kid() {
        assert_eq!(&*cbc().to_jwk_string(), CBC_JWK);
        assert_eq!(&*gcm("x").to_jwk_string(), GCM_JWK);
        assert_eq!(GCM_JWK.len(), 122);
        assert_eq!(cbc().legacy_kid(), "32c9e7917b78");
        let g = RoomKey::generate();
        assert_eq!(g.kid.len(), 36);
        assert_eq!(g.alg, RoomKeyAlg::A256Gcm);
        assert!(!format!("{g:?}").contains(&*b64url_encode(&g.key)));
    }

    #[test]
    fn jwk_rejections() {
        for bad in [
            r#"{"alg":"A256CBC","k":"qb8In0Rpa9nwSusvxxDcbQ"}"#,
            r#"{"k":"qb8In0Rpa9nwSusvxxDcbQ"}"#,
            r#"{"alg":"A128CBC","k":"AAAA"}"#,
            r#"{"alg":"A256GCM","k":"qb8In0Rpa9nwSusvxxDcbQ"}"#,
            r#"{"alg":"A128CBC","kty":"RSA","k":"qb8In0Rpa9nwSusvxxDcbQ"}"#,
            r#"{"alg":"A128CBC","k":"!!"}"#,
            "[]",
            "",
        ] {
            assert!(RoomKey::from_jwk_str("k", bad).is_err(), "{bad}");
        }
        assert!(matches!(
            RoomKey::from_jwk_str("k", r#"{"alg":"A256CTR","k":"AA"}"#),
            Err(E2eError::UnsupportedAlgorithm(a)) if a == "A256CTR"
        ));
    }

    #[test]
    fn upstream_content_vectors() {
        let k = cbc();
        let v1 = content(
            json!({"algorithm":"rc.v1.aes-sha2","ciphertext":"32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4="}),
        );
        assert_eq!(k.decrypt_json(&v1).unwrap(), json!({"msg":"hello"}));
        let mob = "32c9e7917b783JpM8aOVludqIRzx+DOqjEU9Mj3NUWb+/GLRl7sdkvTtCMChH1LBjMjJJvVJ6Rlw4dI8BYFftZWiCOiR7TPwriCoSPiZ7dY5C4H2q8MVSdR95ZiyG7eWQ5j5/rxzAYsSWDA9LkumW8JBb+WQ1hD9JMfQd4IXtlFMnaDgEhZhe/s=";
        let expected = json!({"_id":"AZF8Myj605B3f7ZPL","text":"world","userId":"RQTYT5RJoDKZFwDhk","ts":{"$date":1759174115076u64}});
        let in_content = content(json!({"algorithm":"rc.v1.aes-sha2","ciphertext": mob}));
        assert_eq!(k.decrypt_json(&in_content).unwrap(), expected);
        let in_msg = EncryptedContent::from_message(None, mob).unwrap().unwrap();
        assert_eq!(k.decrypt_json(&in_msg).unwrap(), expected);

        let g = gcm("f46d2864-0384-4a87-8815-51fba2cad216");
        let v2 = json!({"algorithm":"rc.v2.aes-sha2","kid":"f46d2864-0384-4a87-8815-51fba2cad216","iv":"wXbYQ8q9sYRCHtNp","ciphertext":"cIDO9mXzCCrrl/wORP0Jf6oWeusqzSCXVGGvY7CHrA=="});
        assert_eq!(
            g.decrypt_json(&content(v2.clone())).unwrap(),
            json!({"msg":"hello"})
        );
        let iv = crate::e2e::b64_decode("wXbYQ8q9sYRCHtNp").unwrap();
        let enc = g.encrypt_bytes_with_iv(br#"{"msg":"hello"}"#, &iv).unwrap();
        assert_eq!(enc.to_json(), v2);
    }

    #[test]
    fn rustket_message_vectors() {
        let g = gcm("kid");
        let iv = hex::decode("a0a1a2a3a4a5a6a7a8a9aaab").unwrap();
        let plain = r#"{"msg":"Привет, Rustket! 🔐 @alice"}"#;
        let enc = g.encrypt_bytes_with_iv(plain.as_bytes(), &iv).unwrap();
        let json = enc.to_json();
        assert_eq!(json["iv"], "oKGio6Slpqeoqaqr");
        assert_eq!(
            json["ciphertext"],
            "qFgwu7iEzf4femB1smKZ16vs4ZGy52gaBRZ3KsolzKz+ChmhHkIm1RtTLrqJwTY8ySJoDZQIRzA+KWQP"
        );
        assert_eq!(&**g.decrypt_bytes(&enc).unwrap(), plain.as_bytes());

        // v1 over CBC (written by RN for A128CBC rooms).
        let k = cbc();
        let iv = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        let enc = k
            .encrypt_bytes_with_iv(br#"{"msg":"legacy v1"}"#, &iv)
            .unwrap();
        let EncryptedContent::V2 {
            kid,
            iv,
            ciphertext,
        } = enc
        else {
            panic!()
        };
        let v1 = EncryptedContent::V1 {
            kid,
            iv,
            ciphertext,
        };
        let v1s = "32c9e7917b78ABEiM0RVZneImaq7zN3u/5uyUeK+TssJ4yOk/QrZdAkh1KxP799yTru4sAL5hSLB";
        assert_eq!(v1.to_json()["ciphertext"], v1s);
        let parsed = EncryptedContent::parse_v1(v1s).unwrap();
        assert_eq!(k.decrypt_json(&parsed).unwrap(), json!({"msg":"legacy v1"}));

        // v2 envelope over a CBC key, as the 8.9 web writes it.
        let enc = k
            .encrypt_bytes_with_iv(br#"{"msg":"v2 over cbc"}"#, &[0x42; 16])
            .unwrap();
        let expected = json!({"algorithm":"rc.v2.aes-sha2","kid":"32c9e7917b78","iv":"QkJCQkJCQkJCQkJCQkJCQg==","ciphertext":"ovPPRuVf0amt9ULYGp+hxT1FRGzLv3u3mQH/q0lky7g="});
        assert_eq!(enc.to_json(), expected);
        assert_eq!(
            k.decrypt_json(&content(expected)).unwrap(),
            json!({"msg":"v2 over cbc"})
        );
    }

    #[test]
    fn wrong_key_and_malformed_content() {
        let g = gcm("kid");
        let other = RoomKey::generate();
        let enc = g.encrypt_json(&json!({"msg":"x"})).unwrap();
        assert_eq!(other.decrypt_bytes(&enc).unwrap_err(), E2eError::Decrypt);
        // CBC key on a GCM envelope: IV length mismatch.
        assert_eq!(cbc().decrypt_bytes(&enc).unwrap_err(), E2eError::Decrypt);
        let truncated = EncryptedContent::V2 {
            kid: "kid".into(),
            iv: vec![0; 12],
            ciphertext: vec![1, 2],
        };
        assert_eq!(g.decrypt_bytes(&truncated).unwrap_err(), E2eError::Decrypt);
        let unsupported = EncryptedContent::Unsupported {
            algorithm: "m.megolm.v1.aes-sha2".into(),
        };
        assert!(matches!(
            g.decrypt_bytes(&unsupported),
            Err(E2eError::UnsupportedAlgorithm(_))
        ));
        // Valid ciphertext of non-JSON.
        let enc = g.encrypt_bytes(b"not json").unwrap();
        assert!(matches!(g.decrypt_json(&enc), Err(E2eError::Json(_))));
        // `$` keys are not emitted.
        let enc = g.encrypt_json(&json!({"msg":"x","$date":1})).unwrap();
        assert_eq!(g.decrypt_json(&enc).unwrap(), json!({"msg":"x"}));
        let EncryptedContent::V2 { iv, .. } = &enc else {
            panic!()
        };
        assert_eq!(iv.len(), 12);
        let EncryptedContent::V2 { iv, .. } = cbc().encrypt_json(&json!({})).unwrap() else {
            panic!()
        };
        assert_eq!(iv.len(), 16);
        assert!(g.encrypt_bytes_with_iv(b"x", &[0; 16]).is_err());
    }

    #[test]
    fn upstream_wrapped_key_unwraps() {
        let (public, private) = pwdv2();
        let wrapped = "f46d2864-0384-4a87-8815-51fba2cad216WDf45Ovvni38XEDTqhsnVknrvpMEb9KtezR5DuQ5+mBRLKZHWfE1BjeMUsNh/EbcayHsGVzwEjbiIA+ziIVKgKBjABRVn41g6PbgGNem+XpjJ9Jpl48xcqWtFffCrTBXdWZKao7+8iLAUHKo/6dSQ9cXESHDOeGkPmLdyAtwDqUQ8JnxQRVwKSpLnKJg0SrFLAVA7lKqfv7/5N1hySKoP7MOC6d2WQl0kyiBgySzfHeNHezAmnQFLgG5ZE9+cGwX5unTl/KBcFp3unKYh6CP0rnGav6Hdf0kvuTY8q3j2P+WOr045RfZeGlHGzMouUCprsIcvdu69lU4q9R0QB9YKA==";
        let k = RoomKey::unwrap(&private, wrapped).unwrap();
        assert_eq!(k.kid, "f46d2864-0384-4a87-8815-51fba2cad216");
        assert_eq!(&*k.to_jwk_string(), GCM_JWK);

        let w = k.wrap_for(&public).unwrap();
        assert_eq!(w.len(), 36 + 344);
        assert!(w.starts_with("f46d2864-0384-4a87-8815-51fba2cad216"));
        assert!(RoomKey::unwrap(&private, &w).unwrap().same_key(&k));
        let old = k.wrap_for_with_kid(&public, "oldkid").unwrap();
        assert_eq!(RoomKey::unwrap(&private, &old).unwrap().kid, "oldkid");
        // Garbage of the right shape fails cleanly.
        assert!(RoomKey::unwrap(&private, &format!("kid{}", "A".repeat(342) + "==")).is_err());
        assert!(RoomKey::unwrap(&private, "short").is_err());
    }
}
