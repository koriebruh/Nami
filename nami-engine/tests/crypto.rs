use std::collections::HashSet;

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Payload},
};
use base64::Engine;
use hkdf::Hkdf;
use nami_engine::crypto::{Cipher, DecryptError, KEY_LEN, SecretString, TEST_KEY, parse_key};
use sha2::Sha256;

const CONTEXT: &str = "monitors.auth_password";

fn cipher() -> Cipher {
    Cipher::new(&TEST_KEY)
}

// An independent re-implementation of the documented format, built straight
// from the primitives, so tests can make a value that is wrong in exactly one
// way. Documented format (version 1): `0x01 || nonce(12) || ciphertext || tag`,
// AES-256-GCM under the key HKDF-SHA256(salt: none, ikm: master, info:
// "nami/aead/v1"), associated data `0x01 || context`.

/// HKDF-SHA256 with no salt.
fn hkdf_sha256(master: &[u8; KEY_LEN], info: &str) -> [u8; KEY_LEN] {
    let mut okm = [0u8; KEY_LEN];
    Hkdf::<Sha256>::new(None, master)
        .expand(info.as_bytes(), &mut okm)
        .unwrap();
    okm
}

/// `version || nonce || AES-256-GCM(aes_key, nonce, aad, plaintext)`, every
/// parameter chosen by the caller.
fn seal_by_hand(version: u8, aes_key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let nonce = [7u8; 12];
    let ciphertext = Aes256Gcm::new(aes_key.into())
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .unwrap();
    [&[version][..], &nonce, &ciphertext].concat()
}

/// The associated data the format uses: the version byte, then the context.
fn documented_aad(context: &str) -> Vec<u8> {
    [&[1u8][..], context.as_bytes()].concat()
}

#[test]
fn encrypt_then_decrypt_round_trips() {
    let sealed = cipher().encrypt(CONTEXT, b"s3cret password");
    assert_eq!(
        cipher().decrypt(CONTEXT, &sealed).unwrap(),
        b"s3cret password"
    );
}

#[test]
fn the_sealed_value_is_version_nonce_ciphertext_and_tag_and_hides_the_plaintext() {
    let plaintext = b"Bearer abc.def.ghi";
    let sealed = cipher().encrypt(CONTEXT, plaintext);
    // 1-byte format version + 12-byte nonce + ciphertext (same length as the
    // plaintext) + 16-byte tag.
    assert_eq!(sealed.len(), 1 + 12 + plaintext.len() + 16);
    assert_eq!(sealed[0], 0x01, "the first byte is the format version");
    assert!(
        !sealed.windows(plaintext.len()).any(|w| w == plaintext),
        "the plaintext must not appear in the sealed bytes"
    );
}

#[test]
fn sealed_values_of_every_size_carry_the_version_byte_and_round_trip() {
    let c = cipher();
    for plaintext in [&b""[..], b"x", b"a longer secret value", &[0u8; 4096]] {
        let sealed = c.encrypt(CONTEXT, plaintext);
        assert_eq!(sealed[0], 0x01, "{} byte plaintext", plaintext.len());
        assert_eq!(sealed.len(), 1 + 12 + plaintext.len() + 16);
        assert_eq!(c.decrypt(CONTEXT, &sealed).unwrap(), plaintext);
    }
}

#[test]
fn only_format_version_1_is_accepted() {
    let c = cipher();
    let sealed = c.encrypt(CONTEXT, b"s3cret password");
    for version in 0..=u8::MAX {
        let mut candidate = sealed.clone();
        candidate[0] = version;
        let result = c.decrypt(CONTEXT, &candidate);
        if version == 1 {
            assert_eq!(result.unwrap(), b"s3cret password");
        } else {
            assert_eq!(result, Err(DecryptError), "version byte {version:#04x}");
        }
    }
}

#[test]
fn every_encryption_uses_a_fresh_random_nonce() {
    let c = cipher();
    let mut nonces = HashSet::new();
    for _ in 0..1000 {
        let sealed = c.encrypt(CONTEXT, b"same plaintext");
        assert!(nonces.insert(sealed[1..13].to_vec()), "a nonce was reused");
    }
    assert_ne!(
        c.encrypt(CONTEXT, b"x"),
        c.encrypt(CONTEXT, b"x"),
        "same input, different output"
    );
}

#[test]
fn any_tampering_fails_cleanly() {
    let c = cipher();
    let sealed = c.encrypt(CONTEXT, b"s3cret password");
    // The version byte, both ends of the nonce, the first ciphertext byte, the
    // middle, and the last byte of the tag.
    for index in [0, 1, 12, 13, sealed.len() / 2, sealed.len() - 1] {
        let mut tampered = sealed.clone();
        tampered[index] ^= 0x01;
        assert_eq!(
            c.decrypt(CONTEXT, &tampered),
            Err(DecryptError),
            "flipped byte {index}"
        );
    }
    // Shorter than version + nonce + tag (29 bytes), and one byte short of the
    // full value (long enough to reach the AEAD, which must reject it).
    for len in [0, 1, 12, 13, 28, sealed.len() - 1] {
        assert_eq!(
            c.decrypt(CONTEXT, &sealed[..len]),
            Err(DecryptError),
            "truncated to {len} bytes"
        );
    }
    let mut extended = sealed.clone();
    extended.push(0);
    assert_eq!(
        c.decrypt(CONTEXT, &extended),
        Err(DecryptError),
        "one byte appended"
    );
}

#[test]
fn a_different_key_or_context_cannot_decrypt() {
    let sealed = cipher().encrypt(CONTEXT, b"s3cret password");
    let other = Cipher::new(&[9u8; KEY_LEN]);
    assert_eq!(other.decrypt(CONTEXT, &sealed), Err(DecryptError));
    assert_eq!(
        cipher().decrypt("monitors.request_headers", &sealed),
        Err(DecryptError),
        "a value sealed for one column cannot be opened as another"
    );
}

#[test]
fn the_decrypt_error_says_nothing_about_the_secret() {
    assert_eq!(
        DecryptError.to_string(),
        "stored secret could not be decrypted"
    );
}

#[test]
fn a_ciphers_debug_output_never_shows_key_material() {
    let master = [0xA5u8; KEY_LEN];
    let c = Cipher::new(&master);
    // The master, the AEAD key derived from it and a subkey: none may show.
    let secrets = [
        ("master", master),
        ("AEAD key", hkdf_sha256(&master, "nami/aead/v1")),
        ("subkey", c.derive_subkey("nami/hmac/v1")),
    ];
    for rendered in [format!("{c:?}"), format!("{c:#?}")] {
        assert!(rendered.contains("<redacted>"), "{rendered}");
        for (name, key) in secrets {
            let leaks = [
                ("hex", hex::encode(key)),
                (
                    "base64",
                    base64::engine::general_purpose::STANDARD.encode(key),
                ),
                ("byte list", format!("{key:?}")),
                ("raw bytes", String::from_utf8_lossy(&key).into_owned()),
            ];
            for (encoding, needle) in leaks {
                assert!(
                    !rendered.contains(&needle),
                    "Debug output leaks the {name} as {encoding}: {rendered}"
                );
            }
        }
    }
}

#[test]
fn cloning_a_cipher_shares_the_key_schedule_instead_of_copying_it() {
    // `AppState` (and so the cipher) is cloned for every request: a `Cipher`
    // must be a handle to the key material, not hold the AES key schedule
    // inline.
    assert_eq!(std::mem::size_of::<Cipher>(), std::mem::size_of::<usize>());
    let original = cipher();
    let clone = original.clone();
    let sealed = clone.encrypt(CONTEXT, b"shared");
    assert_eq!(original.decrypt(CONTEXT, &sealed).unwrap(), b"shared");
}

#[test]
fn key_material_wipes_itself_on_drop() {
    // These bounds only hold while `aes-gcm` and `sha2` are built with their
    // `zeroize` features: the AES key schedule and GHASH state, and the SHA-256
    // state under the HKDF that derives the keys. Dropping either feature makes
    // this test fail to compile.
    fn wipes_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    wipes_on_drop::<Aes256Gcm>();
    wipes_on_drop::<Sha256>();
}

#[test]
fn the_documented_format_built_from_primitives_decrypts() {
    // Positive control for the negative tests below: exactly what the format
    // document says, built without `Cipher`.
    let sealed = seal_by_hand(
        1,
        &hkdf_sha256(&TEST_KEY, "nami/aead/v1"),
        &documented_aad(CONTEXT),
        b"s3cret password",
    );
    assert_eq!(
        cipher().decrypt(CONTEXT, &sealed).unwrap(),
        b"s3cret password"
    );
}

#[test]
fn a_value_sealed_under_the_raw_master_key_does_not_decrypt() {
    // The AEAD key is an HKDF subkey of the master, never the master itself.
    let aad = documented_aad(CONTEXT);
    let sealed = seal_by_hand(1, &TEST_KEY, &aad, b"s3cret password");
    // Control: the value is well formed, the master really opens it ...
    let nonce = [7u8; 12];
    let opened = Aes256Gcm::new((&TEST_KEY).into())
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: &sealed[13..],
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(opened, b"s3cret password");
    // ... yet the cipher, which uses the derived key, refuses it.
    assert_eq!(cipher().decrypt(CONTEXT, &sealed), Err(DecryptError));
}

#[test]
fn values_that_deviate_from_the_documented_format_do_not_decrypt() {
    let aead_key = hkdf_sha256(&TEST_KEY, "nami/aead/v1");
    let plaintext = b"s3cret password";
    let deviations = [
        (
            "a different HKDF info",
            seal_by_hand(
                1,
                &hkdf_sha256(&TEST_KEY, "nami/aead/v2"),
                &documented_aad(CONTEXT),
                plaintext,
            ),
        ),
        (
            "the HMAC subkey instead of the AEAD key",
            seal_by_hand(
                1,
                &hkdf_sha256(&TEST_KEY, "nami/hmac/v1"),
                &documented_aad(CONTEXT),
                plaintext,
            ),
        ),
        (
            "the version byte missing from the associated data",
            seal_by_hand(1, &aead_key, CONTEXT.as_bytes(), plaintext),
        ),
        (
            "another version in the associated data",
            seal_by_hand(
                1,
                &aead_key,
                &[&[2u8][..], CONTEXT.as_bytes()].concat(),
                plaintext,
            ),
        ),
        (
            "another version byte on the wire",
            seal_by_hand(2, &aead_key, &documented_aad(CONTEXT), plaintext),
        ),
    ];
    for (what, sealed) in deviations {
        assert_eq!(
            cipher().decrypt(CONTEXT, &sealed),
            Err(DecryptError),
            "{what}"
        );
    }
}

#[test]
fn a_sealed_value_from_an_independent_implementation_still_opens() {
    // Sealed by Node's OpenSSL-backed `crypto`, not by this crate: master =
    // `TEST_KEY`, context "monitors.auth_password", plaintext "s3cret password",
    // nonce 000102..0b. Pins the stored format. If this fails, a change just
    // made every stored secret unreadable: add a new format version instead of
    // editing version 1.
    let golden = hex::decode(
        "01000102030405060708090a0b6d04bb935d1f8f53d70df5000aff7ad0c42ed166b70e5b6828517c39c6c9b2",
    )
    .unwrap();
    assert_eq!(
        cipher().decrypt(CONTEXT, &golden).unwrap(),
        b"s3cret password"
    );
}

#[test]
fn derive_subkey_is_deterministic_and_differs_per_info_and_master() {
    let c = cipher();
    let hmac_key = c.derive_subkey("nami/hmac/v1");
    assert_eq!(hmac_key, c.derive_subkey("nami/hmac/v1"), "deterministic");
    assert_eq!(
        hmac_key,
        Cipher::new(&TEST_KEY).derive_subkey("nami/hmac/v1"),
        "same master, same subkey"
    );
    assert_ne!(hmac_key, c.derive_subkey("nami/hmac/v2"), "other info");
    assert_ne!(
        hmac_key,
        c.derive_subkey(""),
        "empty info is just another label"
    );
    assert_ne!(
        hmac_key,
        Cipher::new(&[9u8; KEY_LEN]).derive_subkey("nami/hmac/v1"),
        "other master"
    );
    assert_ne!(hmac_key, TEST_KEY, "never the master itself");
}

#[test]
fn derive_subkey_is_hkdf_sha256_of_the_master() {
    // Known answer for HKDF-SHA256 (no salt), ikm = `TEST_KEY`, info =
    // "nami/hmac/v1", computed with three unrelated implementations: Node's
    // OpenSSL-backed `crypto.hkdfSync`, RFC 5869 in Python's stdlib, and
    // `openssl kdf`.
    let expected =
        hex::decode("503482eda13cb0f8aa819f750c1057c3b6d627c0010a0d04fbd357bd85f30453").unwrap();
    assert_eq!(cipher().derive_subkey("nami/hmac/v1").to_vec(), expected);
    assert_eq!(
        cipher().derive_subkey("nami/hmac/v1"),
        hkdf_sha256(&TEST_KEY, "nami/hmac/v1")
    );
}

#[test]
#[should_panic(expected = "reserved")]
fn derive_subkey_never_hands_out_the_aead_key() {
    cipher().derive_subkey("nami/aead/v1");
}

#[test]
fn parse_key_accepts_exactly_32_base64_bytes() {
    let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    assert_eq!(parse_key(&encoded).unwrap(), [7u8; 32]);
    assert_eq!(
        parse_key(&format!("  {encoded}\n")).unwrap(),
        [7u8; 32],
        "surrounding whitespace is ignored"
    );
}

#[test]
fn parse_key_rejects_missing_malformed_and_wrong_length_keys() {
    let b64 = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![1u8; n]);
    let cases = [
        (String::new(), "must be set"),
        ("   ".to_string(), "must be set"),
        ("not base64!".to_string(), "valid base64"),
        (b64(16), "exactly 32 bytes, got 16"),
        (b64(33), "exactly 32 bytes, got 33"),
    ];
    for (raw, fragment) in cases {
        let message = parse_key(&raw).unwrap_err();
        assert!(message.starts_with("NAMI_ENCRYPTION_KEY"), "{message}");
        assert!(
            message.contains(fragment),
            "{raw:?}: {message:?} should mention {fragment:?}"
        );
    }
}

#[test]
fn parse_key_errors_tell_the_operator_how_to_make_a_key() {
    let b64 = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![1u8; n]);
    let unpadded = b64(32).trim_end_matches('=').to_string();
    // `openssl rand -hex 32` gives 64 characters that are all valid base64, so
    // that common mistake is a length error, not an encoding error.
    let hex_key = "ab".repeat(32);
    for raw in [
        String::new(),
        "   ".to_string(),
        "not base64!".to_string(),
        unpadded,
        b64(16),
        hex_key,
    ] {
        let message = parse_key(&raw).unwrap_err();
        assert!(
            message.contains(
                "must be standard base64 with = padding (e.g. `openssl rand -base64 32`)"
            ),
            "{raw:?}: {message}"
        );
    }
}

#[test]
fn secret_strings_never_print_their_value() {
    let secret: SecretString = serde_json::from_str("\"hunter2\"").unwrap();
    assert_eq!(secret.expose(), "hunter2");
    assert_eq!(format!("{secret:?}"), "SecretString(<redacted>)");
    assert_eq!(SecretString::new("hunter2"), secret);
}
