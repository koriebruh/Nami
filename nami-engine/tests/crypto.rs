use std::collections::HashSet;

use base64::Engine;
use nami_engine::crypto::{Cipher, DecryptError, KEY_LEN, SecretString, TEST_KEY, parse_key};

const CONTEXT: &str = "monitors.auth_password";

fn cipher() -> Cipher {
    Cipher::new(&TEST_KEY)
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
fn the_sealed_value_is_nonce_then_ciphertext_and_tag_and_hides_the_plaintext() {
    let plaintext = b"Bearer abc.def.ghi";
    let sealed = cipher().encrypt(CONTEXT, plaintext);
    // 12-byte nonce + ciphertext (same length as the plaintext) + 16-byte tag.
    assert_eq!(sealed.len(), 12 + plaintext.len() + 16);
    assert!(
        !sealed.windows(plaintext.len()).any(|w| w == plaintext),
        "the plaintext must not appear in the sealed bytes"
    );
}

#[test]
fn every_encryption_uses_a_fresh_random_nonce() {
    let c = cipher();
    let mut nonces = HashSet::new();
    for _ in 0..1000 {
        let sealed = c.encrypt(CONTEXT, b"same plaintext");
        assert!(nonces.insert(sealed[..12].to_vec()), "a nonce was reused");
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
    for index in [0, 11, 12, sealed.len() / 2, sealed.len() - 1] {
        let mut tampered = sealed.clone();
        tampered[index] ^= 0x01;
        assert_eq!(
            c.decrypt(CONTEXT, &tampered),
            Err(DecryptError),
            "flipped byte {index}"
        );
    }
    for len in [0, 1, 11, 12, 27] {
        assert_eq!(
            c.decrypt(CONTEXT, &sealed[..len]),
            Err(DecryptError),
            "truncated to {len} bytes"
        );
    }
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
fn secret_strings_never_print_their_value() {
    let secret: SecretString = serde_json::from_str("\"hunter2\"").unwrap();
    assert_eq!(secret.expose(), "hunter2");
    assert_eq!(format!("{secret:?}"), "SecretString(<redacted>)");
    assert_eq!(SecretString::new("hunter2"), secret);
}
