// NOTE: the plan's brief was written against an older totp-rs API
// (`TOTP::new(...)`, `Secret::generate_secret()/.to_encoded()`,
// `TOTP::get_url()`, `check_current(...) -> bool`). The version actually
// resolved by `cargo add totp-rs` (6.0.0) replaced that with a `Builder` and
// renamed `TOTP` to `Totp`, and `check_current` now returns `Option<u64>`
// (the matched time-step) instead of `bool`. None of this changes the
// security-relevant parameters: algorithm (SHA1), digits (6), skew (1 step),
// and step duration (30s) are unchanged and are set explicitly below rather
// than relying on defaults, so validation behavior matches the brief intent.
use totp_rs::{Algorithm, Builder, Secret, Totp};

fn totp_for(secret: &str, email: &str) -> Totp {
    let secret = Secret::try_from_base32(secret).expect("stored totp secret must be valid base32");
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret)
        .with_account_name(email.to_string())
        .with_issuer(Some("Nami"))
        .build()
        .unwrap()
}

pub fn generate_secret() -> String {
    Secret::generate().to_base32()
}

pub fn provisioning_uri(secret: &str, email: &str) -> String {
    totp_for(secret, email).to_url().unwrap()
}

pub fn verify_code(secret: &str, email: &str, code: &str) -> bool {
    totp_for(secret, email).check_current(code).is_some()
}
