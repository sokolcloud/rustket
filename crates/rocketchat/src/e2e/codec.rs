//! Encodings (base64, base64url, Latin-1), wrapped room keys and content envelopes.

use std::fmt;

use base64::Engine;
use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::{DecodePaddingMode, general_purpose};
use serde_json::{Value, json};

use super::{ALGORITHM_V1, ALGORITHM_V2, E2eError, RSA_BLOCK_LEN, Result};

const STANDARD_LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const URL_SAFE_LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Standard alphabet, padded.
pub fn b64_encode(data: &[u8]) -> String {
    general_purpose::STANDARD.encode(data)
}

/// Standard alphabet; padding optional.
pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
    STANDARD_LENIENT.decode(s).map_err(|_| E2eError::Base64)
}

/// URL-safe alphabet without padding (JWK members).
pub fn b64url_encode(data: &[u8]) -> String {
    URL_SAFE_LENIENT.encode(data)
}

/// URL-safe alphabet; padding optional.
pub fn b64url_decode(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_LENIENT.decode(s).map_err(|_| E2eError::Base64)
}

/// The web "Binary" codec: one char per byte. `None` if a char is above U+00FF
/// (the web throws a `RangeError` there).
pub fn latin1_bytes(s: &str) -> Option<Vec<u8>> {
    s.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
}

/// A room key wrapped for one member: `kid + base64(RSA-OAEP block)`
/// (`E2EKey`, `E2ESuggestedKey`, `oldRoomKeys[].E2EKey`). The kid is not
/// authenticated.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedKey {
    pub kid: String,
    pub blob: [u8; RSA_BLOCK_LEN],
}

/// Length of the base64 part of a wrapped key.
const WRAPPED_B64_LEN: usize = 344;

impl WrappedKey {
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() < WRAPPED_B64_LEN {
            return Err(E2eError::Format("wrapped key too short"));
        }
        let split = s.len() - WRAPPED_B64_LEN;
        if !s.is_char_boundary(split) {
            return Err(E2eError::Format("wrapped key is not ASCII"));
        }
        let (kid, b64) = s.split_at(split);
        let bytes = b64_decode(b64)?;
        let blob: [u8; RSA_BLOCK_LEN] = bytes
            .try_into()
            .map_err(|_| E2eError::Format("wrapped key block is not 256 bytes"))?;
        Ok(Self {
            kid: kid.to_owned(),
            blob,
        })
    }
}

impl fmt::Display for WrappedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kid, b64_encode(&self.blob))
    }
}

impl fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WrappedKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

/// A decoded message/file content envelope (`IMessage.content`).
#[derive(Clone, PartialEq, Eq)]
pub enum EncryptedContent {
    /// `rc.v1.aes-sha2`: `ciphertext = kid12 + base64(iv16 ‖ ct)`.
    V1 {
        kid: String,
        iv: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// `rc.v2.aes-sha2`: separate `kid`, `iv`, `ciphertext`.
    V2 {
        kid: String,
        iv: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Any other algorithm (e.g. `m.megolm.v1.aes-sha2`); not decryptable.
    Unsupported { algorithm: String },
}

impl fmt::Debug for EncryptedContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1 { kid, .. } => f
                .debug_struct("V1")
                .field("kid", kid)
                .finish_non_exhaustive(),
            Self::V2 { kid, .. } => f
                .debug_struct("V2")
                .field("kid", kid)
                .finish_non_exhaustive(),
            Self::Unsupported { algorithm } => f
                .debug_struct("Unsupported")
                .field("algorithm", algorithm)
                .finish(),
        }
    }
}

impl EncryptedContent {
    /// Decodes the encrypted part of a message: `content` if present, else a
    /// legacy v1 ciphertext in a non-empty `msg`. `None` when there is nothing
    /// to decrypt (no content and empty `msg`).
    pub fn from_message(content: Option<&Value>, msg: &str) -> Option<Result<Self>> {
        match content {
            Some(c) if !c.is_null() => Some(Self::from_value(c)),
            _ if !msg.is_empty() => Some(Self::parse_v1(msg)),
            _ => None,
        }
    }

    /// Decodes a `content` object.
    pub fn from_value(v: &Value) -> Result<Self> {
        let obj = v
            .as_object()
            .ok_or(E2eError::Format("content is not an object"))?;
        let s = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default();
        let algorithm = obj
            .get("algorithm")
            .and_then(Value::as_str)
            .ok_or(E2eError::Format("content without algorithm"))?;
        match algorithm {
            ALGORITHM_V1 => Self::parse_v1(s("ciphertext")),
            ALGORITHM_V2 => {
                let (kid, iv, ct) = (s("kid"), s("iv"), s("ciphertext"));
                if kid.is_empty() || iv.is_empty() || ct.is_empty() {
                    return Err(E2eError::Format("v2 content needs kid, iv and ciphertext"));
                }
                Ok(Self::V2 {
                    kid: kid.to_owned(),
                    iv: b64_decode(iv)?,
                    ciphertext: b64_decode(ct)?,
                })
            }
            other => Ok(Self::Unsupported {
                algorithm: other.to_owned(),
            }),
        }
    }

    /// Decodes a v1 ciphertext string (the kid is always the first 12 chars).
    pub fn parse_v1(s: &str) -> Result<Self> {
        let split = s
            .char_indices()
            .nth(12)
            .map(|(i, _)| i)
            .ok_or(E2eError::Format("v1 ciphertext too short for the kid"))?;
        let (kid, b64) = s.split_at(split);
        let mut bytes = b64_decode(b64)?;
        if bytes.len() < 16 {
            return Err(E2eError::Format("v1 ciphertext too short for the iv"));
        }
        let ciphertext = bytes.split_off(16);
        Ok(Self::V1 {
            kid: kid.to_owned(),
            iv: bytes,
            ciphertext,
        })
    }

    pub fn kid(&self) -> Option<&str> {
        match self {
            Self::V1 { kid, .. } | Self::V2 { kid, .. } => Some(kid),
            Self::Unsupported { .. } => None,
        }
    }

    /// `(iv, ciphertext)` of a decryptable envelope.
    pub(crate) fn parts(&self) -> Option<(&[u8], &[u8])> {
        match self {
            Self::V1 { iv, ciphertext, .. } | Self::V2 { iv, ciphertext, .. } => {
                Some((iv, ciphertext))
            }
            Self::Unsupported { .. } => None,
        }
    }

    /// The wire form: an object for v2, `{algorithm, ciphertext}` for v1.
    pub fn to_json(&self) -> Value {
        match self {
            Self::V2 {
                kid,
                iv,
                ciphertext,
            } => json!({
                "algorithm": ALGORITHM_V2,
                "kid": kid,
                "iv": b64_encode(iv),
                "ciphertext": b64_encode(ciphertext),
            }),
            Self::V1 {
                kid,
                iv,
                ciphertext,
            } => {
                let mut blob = iv.clone();
                blob.extend_from_slice(ciphertext);
                json!({
                    "algorithm": ALGORITHM_V1,
                    "ciphertext": format!("{kid}{}", b64_encode(&blob)),
                })
            }
            Self::Unsupported { algorithm } => json!({ "algorithm": algorithm }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const PREFIX: &str = "32c9e7917b78";
    const B64: &str = "ibtLAKG9zcQ/NTp+86nVelUjewPbPNW+EC+eagVPVVlbxvWNXkgltrBQB4gDao1Fp6fHUibQB3dirJ4rzy7CViww0o4QjAwPPQMIxZ9DLJhjKnu6bkkOp6Z0/a9g/8Wf/cvP9/bp7tUt7Et4XMmJwIe5iyJZ35lsyduLc8V+YyK8sJiGf4BRagJoBr8xEBgqBWqg6Vwn3qtbbiTs65PqErbaUmSM3Hn6tfkcS6ukLG/DbptW1B9U66IX3fQesj50zWZiJyvxOoxDeHRH9UEStyv9SP8nrFjEKM3TDiakBeDxja6LoN8l3CjP9K/5eg25YqANZAQjlwaCaeTTHndTgQ==";

    #[test]
    fn prefixed_base64_roundtrip() {
        let s = format!("{PREFIX}{B64}");
        let w = WrappedKey::parse(&s).unwrap();
        assert_eq!(w.kid, PREFIX);
        assert_eq!(w.to_string(), s);
        assert!(!format!("{w:?}").contains("ibtL"));
    }

    #[test]
    fn prefixed_base64_errors() {
        assert!(WrappedKey::parse("too-short").is_err());
        assert!(WrappedKey::parse(&format!("{PREFIX}{}", "A".repeat(343))).is_err());
        assert!(WrappedKey::parse(&format!("{PREFIX}{}", "!".repeat(344))).is_err());
        // 344 base64 chars that decode to 258 bytes (no padding).
        assert!(WrappedKey::parse(&"A".repeat(344)).is_err());
        // Non-ASCII where the split would fall.
        assert!(WrappedKey::parse(&format!("é{}", &B64[1..])).is_err());
        // Empty kid is allowed.
        assert_eq!(WrappedKey::parse(B64).unwrap().kid, "");
    }

    #[test]
    fn b64url_accepts_padding() {
        assert_eq!(b64url_decode("_-8").unwrap(), vec![0xff, 0xef]);
        assert_eq!(b64url_decode("_-8=").unwrap(), vec![0xff, 0xef]);
        assert_eq!(b64url_encode(&[0xff, 0xef]), "_-8");
        assert_eq!(b64_decode("AAE").unwrap(), vec![0, 1]);
        assert!(b64_decode("A").is_err());
    }

    #[test]
    fn latin1() {
        assert_eq!(latin1_bytes("aé").unwrap(), vec![b'a', 0xe9]);
        assert!(latin1_bytes("ж").is_none());
    }

    #[test]
    fn v1_parsing() {
        let c =
            EncryptedContent::parse_v1("32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4=")
                .unwrap();
        let EncryptedContent::V1 {
            kid,
            iv,
            ciphertext,
        } = &c
        else {
            panic!()
        };
        assert_eq!(kid, "32c9e7917b78");
        assert_eq!(iv.len(), 16);
        assert_eq!(ciphertext.len(), 16);
        assert_eq!(
            c.to_json()["ciphertext"],
            "32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4="
        );
        assert!(EncryptedContent::parse_v1("short").is_err());
        assert!(EncryptedContent::parse_v1("32c9e7917b78AAAA").is_err());
        assert!(EncryptedContent::parse_v1("32c9e7917b78!!!!").is_err());
        assert!(EncryptedContent::parse_v1("ééééééééééééé").is_err());
    }

    #[test]
    fn content_decoding() {
        let v2 = json!({"algorithm":"rc.v2.aes-sha2","kid":"k","iv":"AAAA","ciphertext":"AAAA"});
        let c = EncryptedContent::from_value(&v2).unwrap();
        assert_eq!(c.kid(), Some("k"));
        assert_eq!(c.to_json(), v2);
        for bad in [
            json!({"algorithm":"rc.v2.aes-sha2","kid":"","iv":"AAAA","ciphertext":"AAAA"}),
            json!({"algorithm":"rc.v2.aes-sha2","kid":"k","iv":"AA!A","ciphertext":"AAAA"}),
            json!({"algorithm":"rc.v2.aes-sha2"}),
            json!({"kid":"k"}),
            json!("string"),
            json!({"algorithm": 5}),
        ] {
            assert!(EncryptedContent::from_value(&bad).is_err(), "{bad}");
        }
        let megolm = json!({"algorithm":"m.megolm.v1.aes-sha2","ciphertext":"x"});
        assert_eq!(
            EncryptedContent::from_value(&megolm).unwrap(),
            EncryptedContent::Unsupported {
                algorithm: "m.megolm.v1.aes-sha2".into()
            }
        );
        assert!(EncryptedContent::from_message(None, "").is_none());
        assert!(EncryptedContent::from_message(Some(&Value::Null), "").is_none());
        assert!(matches!(
            EncryptedContent::from_message(
                None,
                "32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4="
            ),
            Some(Ok(EncryptedContent::V1 { .. }))
        ));
    }
}
