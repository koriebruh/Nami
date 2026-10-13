//! Runs the real `nami-engine` binary to prove it refuses to start without a
//! usable `NAMI_ENCRYPTION_KEY`. The environment is cleared and the working
//! directory is the system temp dir, so neither the developer's shell nor a
//! local `.env` file can supply a key.
use std::process::{Command, Output};

use base64::Engine;

fn start_engine(encryption_key: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nami-engine"));
    command
        .env_clear()
        .current_dir(std::env::temp_dir())
        // Never reached: the key is validated before the database is dialed.
        .env("DATABASE_URL", "postgres://nami:nami@127.0.0.1:1/nami");
    if let Some(key) = encryption_key {
        command.env("NAMI_ENCRYPTION_KEY", key);
    }
    command.output().expect("the nami-engine binary runs")
}

#[test]
fn the_engine_refuses_to_start_without_an_encryption_key() {
    let output = start_engine(None);
    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NAMI_ENCRYPTION_KEY must be set"),
        "{stderr}"
    );
}

#[test]
fn the_engine_refuses_to_start_with_a_key_of_the_wrong_length() {
    let short_key = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
    let output = start_engine(Some(&short_key));
    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exactly 32 bytes, got 16"), "{stderr}");
}
