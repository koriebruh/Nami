//! Encryption of secrets at rest (monitor header values and basic-auth
//! passwords): AES-256-GCM with a random 96-bit nonce per value, stored as
//! `nonce || ciphertext || tag`. The key comes from `NAMI_ENCRYPTION_KEY`
//! (see `Config::from_env`).
//!
//! Every value is sealed for a `context` (e.g. `"monitors.auth_password"`),
//! passed as AES-GCM associated data: a value sealed for one column cannot be
//! opened as another.

use std::fmt;

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Nonce, Payload},
};
use base64::Engine;
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;

/// AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// The fixed key `Config::from_env_for_test` (and so `AppState::for_test`)
/// uses. Production always reads `NAMI_ENCRYPTION_KEY` instead.
pub const TEST_KEY: [u8; KEY_LEN] = *b"nami-test-encryption-key-32bytes";

/// Decryption failed: wrong key, wrong context, truncated or tampered data.
/// Deliberately carries no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("stored secret could not be decrypted")]
pub struct DecryptError;

#[derive(Clone)]
pub struct Cipher {
    aead: Aes256Gcm,
}

/// Never prints key material.
impl fmt::Debug for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cipher(<redacted>)")
    }
}

impl Cipher {
    pub fn new(key: &[u8; KEY_LEN]) -> Cipher {
        Cipher {
            aead: Aes256Gcm::new(&(*key).into()),
        }
    }

    /// Seals `plaintext` under a fresh random nonce from the OS RNG.
    pub fn encrypt(&self, context: &str, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
        let ciphertext = self
            .aead
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .expect("AES-GCM encryption of an in-memory buffer cannot fail");
        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        sealed
    }

    pub fn decrypt(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, DecryptError> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(DecryptError);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| DecryptError)?;
        self.aead
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| DecryptError)
    }
}

/// Parses `NAMI_ENCRYPTION_KEY`: standard base64 (surrounding whitespace
/// ignored) that decodes to exactly 32 bytes. The error is a complete
/// startup message.
pub fn parse_key(raw: &str) -> Result<[u8; KEY_LEN], String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(
            "NAMI_ENCRYPTION_KEY must be set to 32 random bytes, base64-encoded \
                    (generate one with `openssl rand -base64 32`)"
                .to_string(),
        );
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "NAMI_ENCRYPTION_KEY must be valid base64".to_string())?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| format!("NAMI_ENCRYPTION_KEY must decode to exactly 32 bytes, got {len}"))
}

/// A write-only secret from a request body (a header value, a password).
/// `Debug` never shows it and it cannot be serialized, so it cannot leak
/// through a log line or an API response by accident.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> SecretString {
        SecretString(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}
