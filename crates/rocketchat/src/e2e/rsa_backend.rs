//! RSA-OAEP-2048/SHA-256 behind a small trait, so the implementation can move to
//! `rsa 0.10` or `aws-lc-rs` without touching the rest of the module.
//!
//! The current backend is RustCrypto `rsa 0.9` (RUSTSEC-2023-0071: timing side
//! channel in decryption; decryption is blinded and only runs locally on data
//! fetched from the server, so there is no remote timing oracle).

use zeroize::Zeroizing;

use super::{E2eError, RSA_BLOCK_LEN, Result};

/// Big-endian integer components of a private key. Wiped on drop.
pub(crate) struct PrivateParts {
    pub n: Vec<u8>,
    pub e: Vec<u8>,
    pub d: Zeroizing<Vec<u8>>,
    pub p: Zeroizing<Vec<u8>>,
    pub q: Zeroizing<Vec<u8>>,
    pub dp: Zeroizing<Vec<u8>>,
    pub dq: Zeroizing<Vec<u8>>,
    pub qi: Zeroizing<Vec<u8>>,
}

pub(crate) trait RsaBackend {
    type Public: Clone + Send + Sync + 'static;
    type Private: Send + Sync + 'static;

    /// Generates a 2048-bit key with e = 65537 (blocking, slow).
    fn generate() -> Result<Self::Private>;
    fn public_of(key: &Self::Private) -> Self::Public;
    fn public_from_parts(n: &[u8], e: &[u8]) -> Result<Self::Public>;
    /// `p`/`q` may be empty: the primes are then recovered from `d`.
    fn private_from_parts(parts: &PrivateParts) -> Result<Self::Private>;
    fn public_parts(key: &Self::Public) -> (Vec<u8>, Vec<u8>);
    fn private_parts(key: &Self::Private) -> Result<PrivateParts>;
    fn modulus_bytes(key: &Self::Public) -> usize;
    /// OAEP with SHA-256 for the label hash and MGF1, empty label.
    fn encrypt(key: &Self::Public, msg: &[u8]) -> Result<[u8; RSA_BLOCK_LEN]>;
    fn decrypt(key: &Self::Private, ct: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}

pub(crate) type Backend = RustCryptoRsa;

pub(crate) struct RustCryptoRsa;

fn rsa_err(e: rsa::Error) -> E2eError {
    E2eError::Rsa(e.to_string())
}

fn big(b: &[u8]) -> rsa::BigUint {
    rsa::BigUint::from_bytes_be(b)
}

impl RsaBackend for RustCryptoRsa {
    type Public = rsa::RsaPublicKey;
    type Private = rsa::RsaPrivateKey;

    fn generate() -> Result<Self::Private> {
        let mut rng = rsa::rand_core::OsRng;
        let mut key = rsa::RsaPrivateKey::new(&mut rng, 2048).map_err(rsa_err)?;
        key.precompute().map_err(rsa_err)?;
        Ok(key)
    }

    fn public_of(key: &Self::Private) -> Self::Public {
        key.to_public_key()
    }

    fn public_from_parts(n: &[u8], e: &[u8]) -> Result<Self::Public> {
        rsa::RsaPublicKey::new(big(n), big(e)).map_err(rsa_err)
    }

    fn private_from_parts(parts: &PrivateParts) -> Result<Self::Private> {
        let primes = if parts.p.is_empty() || parts.q.is_empty() {
            Vec::new()
        } else {
            vec![big(&parts.p), big(&parts.q)]
        };
        rsa::RsaPrivateKey::from_components(big(&parts.n), big(&parts.e), big(&parts.d), primes)
            .map_err(rsa_err)
    }

    fn public_parts(key: &Self::Public) -> (Vec<u8>, Vec<u8>) {
        use rsa::traits::PublicKeyParts;
        (key.n().to_bytes_be(), key.e().to_bytes_be())
    }

    fn private_parts(key: &Self::Private) -> Result<PrivateParts> {
        use rsa::traits::{PrivateKeyParts, PublicKeyParts};
        let primes = key.primes();
        if primes.len() != 2 {
            return Err(E2eError::Rsa("expected two primes".into()));
        }
        let missing = || E2eError::Rsa("missing CRT values".into());
        let z = |v: Vec<u8>| Zeroizing::new(v);
        Ok(PrivateParts {
            n: key.n().to_bytes_be(),
            e: key.e().to_bytes_be(),
            d: z(key.d().to_bytes_be()),
            p: z(primes[0].to_bytes_be()),
            q: z(primes[1].to_bytes_be()),
            dp: z(key.dp().ok_or_else(missing)?.to_bytes_be()),
            dq: z(key.dq().ok_or_else(missing)?.to_bytes_be()),
            qi: z(key.crt_coefficient().ok_or_else(missing)?.to_bytes_be()),
        })
    }

    fn modulus_bytes(key: &Self::Public) -> usize {
        use rsa::traits::PublicKeyParts;
        key.size()
    }

    fn encrypt(key: &Self::Public, msg: &[u8]) -> Result<[u8; RSA_BLOCK_LEN]> {
        let mut rng = rsa::rand_core::OsRng;
        let ct = key
            .encrypt(&mut rng, rsa::Oaep::new::<rsa::sha2::Sha256>(), msg)
            .map_err(rsa_err)?;
        ct.try_into()
            .map_err(|_| E2eError::Rsa("ciphertext is not 256 bytes".into()))
    }

    fn decrypt(key: &Self::Private, ct: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let mut rng = rsa::rand_core::OsRng;
        key.decrypt_blinded(&mut rng, rsa::Oaep::new::<rsa::sha2::Sha256>(), ct)
            .map(Zeroizing::new)
            .map_err(|_| E2eError::Decrypt)
    }
}
