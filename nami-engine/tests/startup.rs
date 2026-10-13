//! Runs the real `nami-engine` binary to prove it refuses to start without a
//! usable `NAMI_ENCRYPTION_KEY`. The environment is cleared, so the developer's
//! shell cannot supply a key, and the working directory is the system temp
//! dir. `dotenvy` looks for a `.env` file in the working directory and then in
//! every parent directory, so a stray `.env` that sets `NAMI_ENCRYPTION_KEY`
//! in the temp dir or in any directory above it would break the "key is not
//! set" test below. The tests that do set the variable (even to an empty
//! string) are immune: `dotenvy` never overrides a variable that is already
//! set.
use std::process::{Command, Output};

use base64::Engine;
use nami_engine::crypto::TEST_KEY;

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
fn the_engine_refuses_to_start_with_an_empty_encryption_key() {
    let output = start_engine(Some(""));
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
    // Not interpolated into the failure message on purpose: if it ever is
    // echoed, the test output should not repeat it.
    assert!(
        !stderr.contains(&short_key),
        "the supplied key must not be echoed to stderr"
    );
}

#[test]
fn the_engine_refuses_to_start_on_the_built_in_test_key() {
    // The test key is public (it lives in this repository), so secrets sealed
    // under it would not be secret.
    let test_key = base64::engine::general_purpose::STANDARD.encode(TEST_KEY);
    let output = start_engine(Some(&test_key));
    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NAMI_ENCRYPTION_KEY must not be the built-in test key"),
        "{stderr}"
    );
}
