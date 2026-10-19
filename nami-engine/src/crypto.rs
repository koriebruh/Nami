//! Encryption of secrets at rest (monitor header values and basic-auth
//! passwords): AES-256-GCM with a random 96-bit nonce per value. The master
//! key comes from `NAMI_ENCRYPTION_KEY` (see `Config::from_env`) and is never
//! used as an AES key itself: each purpose gets its own HKDF-SHA256 subkey of
//! it, so one master key can serve several primitives without key reuse. The
//! AEAD key is the subkey for info `nami/aead/v1`; other sub-projects take
//! theirs from `Cipher::derive_subkey` (e.g. an HMAC key for `nami/hmac/v1`).
//!
//! A sealed value is `version || nonce || ciphertext || tag`:
//!
//! - `version`: 1 byte, currently `0x01`;
//! - `nonce`: 12 random bytes from the OS RNG;
//! - `ciphertext`: as long as the plaintext;
//! - `tag`: the 16-byte GCM tag.
//!
//! The AES-GCM associated data is `version || context`, where `context` names
//! the column and the row the value belongs to (e.g.
//! `"monitors.auth_password:<monitor id>"`, see `models::monitor_request`): a
//! value sealed for one column cannot be opened as another, a value copied
//! onto another row does not open there, and the version byte is
//! authenticated along with the data. A value with an unknown version, or too
//! short to hold a version, a nonce and a tag, is rejected before any
//! decryption.
//!
//! Version 1 is what gets stored. A change to the layout, the key derivation
//! or the associated data needs a new version byte, with version 1 still
//! readable.

use std::{fmt, sync::Arc};

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Nonce, Payload},
};
use base64::Engine;
use hkdf::Hkdf;
use rand::{RngCore, rngs::OsRng};
use serde::{
    Deserialize, Deserializer,
    de::{self, Unexpected, Visitor},
};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Length in bytes of the master key (`NAMI_ENCRYPTION_KEY`) and of every key
/// derived from it.
pub const KEY_LEN: usize = 32;
/// Format version, the first byte of every sealed value.
const VERSION: u8 = 1;
const VERSION_LEN: usize = 1;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// HKDF `info` of the AEAD key. Reserved: `Cipher::derive_subkey` refuses it,
/// so the AEAD key never leaves the cipher.
const AEAD_INFO: &str = "nami/aead/v1";

/// The fixed key `Config::from_env_for_test` (and so `AppState::for_test`)
/// uses. Production always reads `NAMI_ENCRYPTION_KEY` instead, and `main`
/// refuses to start on this key: it is public.
pub const TEST_KEY: [u8; KEY_LEN] = *b"nami-test-encryption-key-32bytes";

/// Decryption failed: wrong key, wrong context, unknown version, truncated or
/// tampered data. Deliberately carries no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("stored secret could not be decrypted")]
pub struct DecryptError;

/// Seals and opens secrets (see the module docs for the format). Cloning is
/// cheap: every clone shares one set of key material.
#[derive(Clone)]
pub struct Cipher {
    keys: Arc<Keys>,
}

/// The key material behind a `Cipher`. Shared by every clone because
/// `AppState`, and so the cipher, is cloned for every request while the AES
/// key schedule and GHASH state are over a kilobyte. Both halves wipe
/// themselves on drop (the `zeroize` features of `aes-gcm` and `sha2`).
struct Keys {
    aead: Aes256Gcm,
    /// HKDF state keyed by the master key: the source of the AEAD key and of
    /// `Cipher::derive_subkey`. The master key itself is not kept.
    kdf: Hkdf<Sha256>,
}

/// Never prints key material.
impl fmt::Debug for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cipher(<redacted>)")
    }
}

impl Cipher {
    /// Builds a cipher from the master key. The AEAD key is the HKDF-SHA256
    /// subkey (no salt, info `nami/aead/v1`), not the master key itself.
    pub fn new(master_key: &[u8; KEY_LEN]) -> Cipher {
        let kdf = Hkdf::<Sha256>::new(None, master_key);
        // Wiped as soon as the AES key schedule has been built from it.
        let mut aead_key = Zeroizing::new([0u8; KEY_LEN]);
        kdf.expand(AEAD_INFO.as_bytes(), aead_key.as_mut_slice())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let aead = Aes256Gcm::new((&*aead_key).into());
        Cipher {
            keys: Arc::new(Keys { aead, kdf }),
        }
    }

    /// Seals `plaintext` under a fresh random nonce from the OS RNG.
    pub fn encrypt(&self, context: &str, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
        let ciphertext = self
            .keys
            .aead
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &associated_data(context),
                },
            )
            .expect("AES-GCM encryption of an in-memory buffer cannot fail");
        let mut sealed = Vec::with_capacity(VERSION_LEN + NONCE_LEN + ciphertext.len());
        sealed.push(VERSION);
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        sealed
    }

    /// Opens a value sealed by `encrypt` for the same `context`. Any other
    /// input (unknown version, too short, wrong key or context, tampered) is a
    /// `DecryptError`.
    pub fn decrypt(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, DecryptError> {
        let Some((&version, rest)) = sealed.split_first() else {
            return Err(DecryptError);
        };
        if version != VERSION || rest.len() < NONCE_LEN + TAG_LEN {
            return Err(DecryptError);
        }
        let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| DecryptError)?;
        self.keys
            .aead
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &associated_data(context),
                },
            )
            .map_err(|_| DecryptError)
    }

    /// Derives a key for another purpose from the master key: HKDF-SHA256 (no
    /// salt) with `info` as the domain label, e.g. `"nami/hmac/v1"` for an
    /// HMAC key. The result is independent of the AEAD key and of the subkey
    /// for any other `info`, and the master key cannot be recovered from it.
    /// Use one fixed, versioned label per purpose, and never use a subkey for
    /// two purposes. The caller owns the returned key: keep it out of logs and
    /// wipe it when done.
    ///
    /// # Panics
    ///
    /// If `info` is the label of the cipher's own AEAD key (`nami/aead/v1`).
    pub fn derive_subkey(&self, info: &str) -> [u8; KEY_LEN] {
        assert!(
            info != AEAD_INFO,
            "derive_subkey: info {AEAD_INFO:?} is reserved for the cipher's own AEAD key"
        );
        let mut subkey = [0u8; KEY_LEN];
        self.keys
            .kdf
            .expand(info.as_bytes(), &mut subkey)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        subkey
    }
}

/// The AES-GCM associated data of a value: the format version, then the
/// context.
fn associated_data(context: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(VERSION_LEN + context.len());
    aad.push(VERSION);
    aad.extend_from_slice(context.as_bytes());
    aad
}

/// How a valid `NAMI_ENCRYPTION_KEY` is written, appended to every error of
/// `parse_key`.
const KEY_FORMAT: &str = "standard base64 with = padding (e.g. `openssl rand -base64 32`)";

/// Parses `NAMI_ENCRYPTION_KEY`: standard base64 with `=` padding (surrounding
/// whitespace ignored) that decodes to exactly 32 bytes. The error is a
/// complete startup message and never contains any part of the key.
pub fn parse_key(raw: &str) -> Result<[u8; KEY_LEN], String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!(
            "NAMI_ENCRYPTION_KEY must be set to 32 random bytes; it must be {KEY_FORMAT}"
        ));
    }
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| {
                format!("NAMI_ENCRYPTION_KEY is not valid base64; it must be {KEY_FORMAT}")
            })?,
    );
    <[u8; KEY_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
        format!(
            "NAMI_ENCRYPTION_KEY must decode to exactly {KEY_LEN} bytes, got {}; it must be {KEY_FORMAT}",
            bytes.len()
        )
    })
}

/// A write-only secret from a request body (a header value, a password).
/// `Debug` never shows it and it cannot be serialized, so it cannot leak
/// through a log line or an API response by accident. Deserializing accepts a
/// string only, and a value of another type is refused without quoting it:
/// serde_json's own message would repeat it ("invalid type: integer 123456,
/// expected a string") into the error response and the log, which a password
/// written as a number would turn into a leak.
///
/// Serializing one is a compile error:
///
/// ```compile_fail
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<nami_engine::crypto::SecretString>();
/// ```
///
/// The same assertion holds for a type that does implement `Serialize`, so the
/// failure above is about `SecretString` and not a mistake in the check:
///
/// ```
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<String>();
/// ```
#[derive(Clone, PartialEq, Eq)]
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

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<SecretString, D::Error> {
        // `deserialize_any`, not `deserialize_string`: serde_json builds the
        // error for a non-string itself in the latter, quoted value included,
        // while here every type reaches the visitor below.
        deserializer.deserialize_any(SecretStringVisitor)
    }
}

/// Takes a string; names any other type without its value.
struct SecretStringVisitor;

impl SecretStringVisitor {
    fn wrong_type<E: de::Error>(&self, what: &'static str) -> E {
        E::invalid_type(Unexpected::Other(what), self)
    }
}

impl Visitor<'_> for SecretStringVisitor {
    type Value = SecretString;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<SecretString, E> {
        Ok(SecretString::new(value))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<SecretString, E> {
        Ok(SecretString(value))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<SecretString, E> {
        Err(self.wrong_type("boolean"))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<SecretString, E> {
        Err(self.wrong_type("number"))
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<SecretString, E> {
        Err(self.wrong_type("number"))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<SecretString, E> {
        Err(self.wrong_type("number"))
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<SecretString, E> {
        Err(self.wrong_type("number"))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<SecretString, E> {
        Err(self.wrong_type("number"))
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<SecretString, E> {
        Err(self.wrong_type("byte array"))
    }

    // `visit_char` forwards to `visit_str`, the rest (`null`, a list, an
    // object) never carry a value into the default message.
}
