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

/// A fresh, empty directory of this test's own under the system temp dir.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nami-startup-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the engine with an empty environment except `vars` (plus an
/// unreachable `DATABASE_URL`), in `dir`, where a `.env` file may wait.
fn start_engine_in(dir: &std::path::Path, vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nami-engine"));
    command
        .env_clear()
        .current_dir(dir)
        .env("DATABASE_URL", "postgres://nami:nami@127.0.0.1:1/nami");
    for (name, value) in vars {
        command.env(name, value);
    }
    command.output().expect("the nami-engine binary runs")
}

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

/// A `.env` line that does not parse stops startup instead of silently ending
/// the loading there (every key after it would go unread). The message names
/// the line and its key, never a value from the file.
#[test]
fn the_engine_refuses_to_start_on_a_dotenv_file_that_does_not_parse() {
    let dir = scratch_dir("bad-dotenv");
    std::fs::write(
        dir.join(".env"),
        "# settings\nPORT=8090\nSMTP_FROM=Nami <noreply@nami.local>\nSMTP_PASSWORD=hunter2-secret\n",
    )
    .unwrap();

    let output = start_engine_in(&dir, &[]);

    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(".env"), "{stderr}");
    assert!(stderr.contains("line 3"), "{stderr}");
    assert!(stderr.contains("SMTP_FROM"), "{stderr}");
    for value in ["noreply", "Nami <", "hunter2"] {
        assert!(!stderr.contains(value), "{value:?} leaked: {stderr}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Every line of a `.env` file is read, the ones after a quoted value with
/// spaces included: here the last line's key is the built-in test key, which
/// the engine refuses by name, so the refusal proves the line was read.
#[test]
fn the_engine_reads_a_dotenv_file_to_its_last_line() {
    let dir = scratch_dir("good-dotenv");
    let test_key = base64::engine::general_purpose::STANDARD.encode(TEST_KEY);
    std::fs::write(
        dir.join(".env"),
        format!("SMTP_FROM=\"Nami <noreply@nami.local>\"\nNAMI_ENCRYPTION_KEY={test_key}\n"),
    )
    .unwrap();

    let output = start_engine_in(&dir, &[]);

    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NAMI_ENCRYPTION_KEY must not be the built-in test key"),
        "{stderr}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// SMTP credentials sent in the clear to another machine are refused at
/// startup unless the operator accepts that explicitly. The message names the
/// variables and never the password. With `SMTP_ALLOW_PLAINTEXT_AUTH=true` the
/// engine gets past the check (and stops at the unreachable database).
#[test]
fn the_engine_refuses_plaintext_smtp_credentials_to_another_machine() {
    let dir = scratch_dir("smtp-auth");
    let key = base64::engine::general_purpose::STANDARD.encode([0x42u8; 32]);
    let vars = [
        ("NAMI_ENCRYPTION_KEY", key.as_str()),
        ("SMTP_HOST", "smtp.example.com"),
        ("SMTP_USERNAME", "alerts"),
        ("SMTP_PASSWORD", "hunter2-secret"),
    ];

    let output = start_engine_in(&dir, &vars);

    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for name in [
        "SMTP_USERNAME",
        "SMTP_PASSWORD",
        "SMTP_TLS",
        "SMTP_ALLOW_PLAINTEXT_AUTH",
    ] {
        assert!(stderr.contains(name), "{name} is named: {stderr}");
    }
    assert!(!stderr.contains("hunter2"), "the password leaked: {stderr}");

    let mut allowed = vars.to_vec();
    allowed.push(("SMTP_ALLOW_PLAINTEXT_AUTH", "true"));
    // Gives up on the unreachable database after 1 s instead of 5.
    allowed.push(("DATABASE_ACQUIRE_TIMEOUT_SECS", "1"));
    let output = start_engine_in(&dir, &allowed);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("SMTP_ALLOW_PLAINTEXT_AUTH"), "{stderr}");
    assert!(stderr.contains("failed to connect to database"), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}
