//! The user's RSA-OAEP-2048/SHA-256 key pair as JWK strings (§3.2).

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::codec::{b64url_decode, b64url_encode};
use super::rsa_backend::{Backend, PrivateParts, RsaBackend};
use super::{E2eError, RSA_BLOCK_LEN, Result};

const JWK_ALG: &str = "RSA-OAEP-256";
/// Longest plaintext RSA-OAEP-SHA256 can wrap with a 2048-bit key.
pub const MAX_OAEP_PLAINTEXT: usize = RSA_BLOCK_LEN - 2 * 32 - 2;

/// Public RSA JWK as stored in `users.e2e.public_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicJwk {
    pub kty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    pub e: String,
    pub n: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ops: Option<Vec<String>>,
}

/// Private RSA JWK (the plaintext of the key backup). Secret members are wiped
/// on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateJwk {
    pub kty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    pub e: String,
    pub n: String,
    pub d: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub p: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dp: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dq: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub qi: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ops: Option<Vec<String>>,
}

impl Drop for PrivateJwk {
    fn drop(&mut self) {
        for s in [
            &mut self.d,
            &mut self.p,
            &mut self.q,
            &mut self.dp,
            &mut self.dq,
            &mut self.qi,
        ] {
            s.zeroize();
        }
    }
}

impl fmt::Debug for PrivateJwk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateJwk").finish_non_exhaustive()
    }
}

/// WebCrypto import rules: `kty` RSA, `alg` RSA-OAEP-256 if present, `ext` not
/// false, `key_ops` containing `usage` if present.
fn check_import(
    kty: &str,
    alg: Option<&str>,
    ext: Option<bool>,
    ops: Option<&[String]>,
    usage: &str,
) -> Result<()> {
    if kty != "RSA" {
        return Err(E2eError::Format("JWK kty is not RSA"));
    }
    if alg.is_some_and(|a| a != JWK_ALG) {
        return Err(E2eError::UnsupportedAlgorithm(
            alg.unwrap_or_default().to_owned(),
        ));
    }
    if ext == Some(false) {
        return Err(E2eError::Format("JWK is not extractable"));
    }
    if ops.is_some_and(|o| !o.iter().any(|x| x == usage)) {
        return Err(E2eError::Format("JWK key_ops lacks the required usage"));
    }
    Ok(())
}

fn check_size(key: &<Backend as RsaBackend>::Public) -> Result<()> {
    if Backend::modulus_bytes(key) != RSA_BLOCK_LEN {
        return Err(E2eError::Format("RSA key is not 2048 bits"));
    }
    Ok(())
}

/// A member's public key (encrypts room keys for them).
#[derive(Clone)]
pub struct UserPublicKey {
    key: <Backend as RsaBackend>::Public,
}

impl fmt::Debug for UserPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserPublicKey").finish_non_exhaustive()
    }
}

impl PartialEq for UserPublicKey {
    fn eq(&self, other: &Self) -> bool {
        Backend::public_parts(&self.key) == Backend::public_parts(&other.key)
    }
}

impl UserPublicKey {
    /// Parses `users.e2e.public_key` (cheap).
    pub fn from_jwk_str(s: &str) -> Result<Self> {
        let jwk: PublicJwk = serde_json::from_str(s)?;
        check_import(
            &jwk.kty,
            jwk.alg.as_deref(),
            jwk.ext,
            jwk.key_ops.as_deref(),
            "encrypt",
        )?;
        let key = Backend::public_from_parts(&b64url_decode(&jwk.n)?, &b64url_decode(&jwk.e)?)?;
        check_size(&key)?;
        Ok(Self { key })
    }

    pub fn to_jwk(&self) -> PublicJwk {
        let (n, e) = Backend::public_parts(&self.key);
        PublicJwk {
            kty: "RSA".into(),
            alg: Some(JWK_ALG.into()),
            e: b64url_encode(&e),
            n: b64url_encode(&n),
            ext: Some(true),
            key_ops: Some(vec!["encrypt".into()]),
        }
    }

    /// `JSON.stringify(jwk)` form for `e2e.setUserPublicAndPrivateKeys`.
    pub fn to_jwk_string(&self) -> String {
        serde_json::to_string(&self.to_jwk()).unwrap_or_default()
    }

    /// RSA-OAEP-SHA256 (randomized). At most [`MAX_OAEP_PLAINTEXT`] bytes.
    pub fn encrypt_oaep(&self, msg: &[u8]) -> Result<[u8; RSA_BLOCK_LEN]> {
        if msg.len() > MAX_OAEP_PLAINTEXT {
            return Err(E2eError::Format("plaintext too long for RSA-OAEP"));
        }
        Backend::encrypt(&self.key, msg)
    }
}

/// The user's private key (decrypts wrapped room keys).
pub struct UserPrivateKey {
    key: <Backend as RsaBackend>::Private,
}

impl fmt::Debug for UserPrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserPrivateKey").finish_non_exhaustive()
    }
}

impl UserPrivateKey {
    /// Parses a private JWK string (validates the key; somewhat slow when `p`/`q`
    /// are missing).
    pub fn from_jwk_str(s: &str) -> Result<Self> {
        let jwk: PrivateJwk = serde_json::from_str(s)?;
        Self::from_jwk(&jwk)
    }

    pub fn from_jwk(jwk: &PrivateJwk) -> Result<Self> {
        check_import(
            &jwk.kty,
            jwk.alg.as_deref(),
            jwk.ext,
            jwk.key_ops.as_deref(),
            "decrypt",
        )?;
        let opt = |s: &str| -> Result<Zeroizing<Vec<u8>>> {
            if s.is_empty() {
                Ok(Zeroizing::new(Vec::new()))
            } else {
                b64url_decode(s).map(Zeroizing::new)
            }
        };
        let parts = PrivateParts {
            n: b64url_decode(&jwk.n)?,
            e: b64url_decode(&jwk.e)?,
            d: opt(&jwk.d)?,
            p: opt(&jwk.p)?,
            q: opt(&jwk.q)?,
            dp: Zeroizing::new(Vec::new()),
            dq: Zeroizing::new(Vec::new()),
            qi: Zeroizing::new(Vec::new()),
        };
        if parts.d.is_empty() {
            return Err(E2eError::Format("JWK without d"));
        }
        let key = Backend::private_from_parts(&parts)?;
        check_size(&Backend::public_of(&key))?;
        Ok(Self { key })
    }

    pub fn public_key(&self) -> UserPublicKey {
        UserPublicKey {
            key: Backend::public_of(&self.key),
        }
    }

    pub fn to_jwk(&self) -> Result<PrivateJwk> {
        let p = Backend::private_parts(&self.key)?;
        Ok(PrivateJwk {
            kty: "RSA".into(),
            alg: Some(JWK_ALG.into()),
            e: b64url_encode(&p.e),
            n: b64url_encode(&p.n),
            d: b64url_encode(&p.d),
            p: b64url_encode(&p.p),
            q: b64url_encode(&p.q),
            dp: b64url_encode(&p.dp),
            dq: b64url_encode(&p.dq),
            qi: b64url_encode(&p.qi),
            ext: Some(true),
            key_ops: Some(vec!["decrypt".into()]),
        })
    }

    /// The private JWK string (the plaintext of the key backup).
    pub fn to_jwk_string(&self) -> Result<Zeroizing<String>> {
        Ok(Zeroizing::new(serde_json::to_string(&self.to_jwk()?)?))
    }

    /// RSA-OAEP-SHA256 decryption (blocking: a few ms).
    pub fn decrypt_oaep(&self, block: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        Backend::decrypt(&self.key, block)
    }
}

/// A freshly generated or imported key pair.
#[derive(Debug)]
pub struct UserKeyPair {
    pub public: UserPublicKey,
    pub private: UserPrivateKey,
}

impl UserKeyPair {
    /// Generates a new RSA-2048 key pair (**blocking**, tens to hundreds of ms in
    /// release builds, seconds in debug builds).
    pub fn generate() -> Result<Self> {
        let key = Backend::generate()?;
        let private = UserPrivateKey { key };
        Ok(Self {
            public: private.public_key(),
            private,
        })
    }

    pub fn from_private(private: UserPrivateKey) -> Self {
        Self {
            public: private.public_key(),
            private,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use serde_json::Value;

    pub(crate) fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/e2e/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    pub(crate) fn pwdv2() -> (UserPublicKey, UserPrivateKey) {
        let f = fixture("keychain_pwdv2web.json");
        (
            UserPublicKey::from_jwk_str(f["public_key"].as_str().unwrap()).unwrap(),
            UserPrivateKey::from_jwk_str(f["private_key"].as_str().unwrap()).unwrap(),
        )
    }

    #[test]
    fn imports_upstream_keys_and_roundtrips() {
        let (public, private) = pwdv2();
        assert_eq!(private.public_key(), public);
        let ct = public.encrypt_oaep(b"hello").unwrap();
        assert_eq!(&**private.decrypt_oaep(&ct).unwrap(), b"hello");
        // Re-exported JWK keeps the same components as the upstream one.
        let f = fixture("keychain_pwdv2web.json");
        let orig: Value = serde_json::from_str(f["private_key"].as_str().unwrap()).unwrap();
        let ours: Value = serde_json::from_str(&private.to_jwk_string().unwrap()).unwrap();
        for k in [
            "n", "e", "d", "p", "q", "dp", "dq", "qi", "alg", "kty", "key_ops", "ext",
        ] {
            assert_eq!(orig[k], ours[k], "{k}");
        }
        let pubv: Value = serde_json::from_str(&public.to_jwk_string()).unwrap();
        let origpub: Value = serde_json::from_str(f["public_key"].as_str().unwrap()).unwrap();
        assert_eq!(pubv, origpub);
    }

    #[test]
    fn import_rules() {
        let f = fixture("keychain_pwdv2web.json");
        let mut v: Value = serde_json::from_str(f["public_key"].as_str().unwrap()).unwrap();
        v["alg"] = "RSA-OAEP".into();
        assert!(UserPublicKey::from_jwk_str(&v.to_string()).is_err());
        v["alg"] = "RSA-OAEP-256".into();
        v["ext"] = false.into();
        assert!(UserPublicKey::from_jwk_str(&v.to_string()).is_err());
        v["ext"] = true.into();
        v["key_ops"] = serde_json::json!(["decrypt"]);
        assert!(UserPublicKey::from_jwk_str(&v.to_string()).is_err());
        v["key_ops"] = serde_json::json!(["encrypt"]);
        v["n"] = "AQAB".into();
        assert!(UserPublicKey::from_jwk_str(&v.to_string()).is_err());
        v["n"] = "!!".into();
        assert!(UserPublicKey::from_jwk_str(&v.to_string()).is_err());
        assert!(UserPublicKey::from_jwk_str("{").is_err());
        assert!(UserPublicKey::from_jwk_str("null").is_err());
        assert!(
            UserPrivateKey::from_jwk_str(
                "{\"kty\":\"RSA\",\"n\":\"AQAB\",\"e\":\"AQAB\",\"d\":\"\"}"
            )
            .is_err()
        );
        let mut p: Value = serde_json::from_str(f["private_key"].as_str().unwrap()).unwrap();
        p["d"] = "AQAB".into();
        assert!(UserPrivateKey::from_jwk_str(&p.to_string()).is_err());
    }

    #[test]
    fn private_key_without_primes_is_recovered() {
        let f = fixture("keychain_pwdv2web.json");
        let mut p: Value = serde_json::from_str(f["private_key"].as_str().unwrap()).unwrap();
        for k in ["p", "q", "dp", "dq", "qi"] {
            p.as_object_mut().unwrap().remove(k);
        }
        let key = UserPrivateKey::from_jwk_str(&p.to_string()).unwrap();
        let (public, _) = pwdv2();
        let ct = public.encrypt_oaep(b"x").unwrap();
        assert_eq!(&**key.decrypt_oaep(&ct).unwrap(), b"x");
    }

    #[test]
    fn oaep_limits() {
        let (public, private) = pwdv2();
        assert!(public.encrypt_oaep(&[0u8; MAX_OAEP_PLAINTEXT]).is_ok());
        assert!(public.encrypt_oaep(&[0u8; MAX_OAEP_PLAINTEXT + 1]).is_err());
        assert!(private.decrypt_oaep(&[0u8; 256]).is_err());
        assert!(private.decrypt_oaep(&[0u8; 10]).is_err());
    }

    #[test]
    fn generated_keys_have_webcrypto_shape() {
        let pair = UserKeyPair::generate().unwrap();
        let public: Value = serde_json::from_str(&pair.public.to_jwk_string()).unwrap();
        assert_eq!(public["alg"], "RSA-OAEP-256");
        assert_eq!(public["kty"], "RSA");
        assert_eq!(public["e"], "AQAB");
        assert_eq!(public["ext"], true);
        assert_eq!(public["key_ops"], serde_json::json!(["encrypt"]));
        assert!(!public["n"].as_str().unwrap().contains('='));
        assert_eq!(
            b64url_decode(public["n"].as_str().unwrap()).unwrap().len(),
            256
        );
        let private: Value = serde_json::from_str(&pair.private.to_jwk_string().unwrap()).unwrap();
        assert_eq!(private["key_ops"], serde_json::json!(["decrypt"]));
        for k in ["d", "p", "q", "dp", "dq", "qi"] {
            let s = private[k].as_str().unwrap();
            assert!(!s.is_empty() && !s.contains('='), "{k}");
        }
        let again = UserPrivateKey::from_jwk_str(&pair.private.to_jwk_string().unwrap()).unwrap();
        let ct = pair.public.encrypt_oaep(b"k").unwrap();
        assert_eq!(&**again.decrypt_oaep(&ct).unwrap(), b"k");
    }
}
