//! Integration tests for credential storage backends.
//!
//! These test the `AgeFileBackend` across module boundaries (auth + filesystem),
//! verifying encryption, round-tripping, and passphrase validation.

use age::secrecy::SecretString;

use kley::auth::{AgeFileBackend, Credentials, OpenAiCredentials, SecretBackend, ZaiCredentials};

fn sample_creds() -> Credentials {
    Credentials {
        active_provider: Some("openai".into()),
        openai: Some(OpenAiCredentials {
            access_token: "sk-super-secret-token".into(),
            refresh_token: "rt-refresh-me".into(),
            expires_at_ms: u64::MAX,
            account_id: "acct-integration-test".into(),
        }),
        zai: Some(ZaiCredentials {
            api_key: "zai-integration-key".into(),
        }),
    }
}

// ── Round-trip through age-encrypted file ───────────────────────────────────

#[test]
fn age_file_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("creds.age");
    let backend = AgeFileBackend::new(path, SecretString::from("integration-test".to_owned()));

    // Empty initially
    assert!(backend.load().unwrap().is_none());

    // Save and reload
    let creds = sample_creds();
    backend.save(&creds).unwrap();

    let loaded = backend.load().unwrap().expect("should have data");
    assert_eq!(loaded.active_provider.as_deref(), Some("openai"));

    let oa = loaded.openai.unwrap();
    assert_eq!(oa.access_token, "sk-super-secret-token");
    assert_eq!(oa.refresh_token, "rt-refresh-me");
    assert_eq!(oa.account_id, "acct-integration-test");

    let zai = loaded.zai.unwrap();
    assert_eq!(zai.api_key, "zai-integration-key");
}

// ── Wrong passphrase is rejected ────────────────────────────────────────────

#[test]
fn wrong_passphrase_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("creds.age");

    let writer = AgeFileBackend::new(path.clone(), SecretString::from("correct".to_owned()));
    let reader = AgeFileBackend::new(path, SecretString::from("wrong".to_owned()));

    writer.save(&sample_creds()).unwrap();
    assert!(reader.load().is_err(), "wrong passphrase should fail");
}

// ── Secrets are encrypted at rest ───────────────────────────────────────────

#[test]
fn secrets_encrypted_at_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("creds.age");
    let backend = AgeFileBackend::new(path.clone(), SecretString::from("my-passphrase".to_owned()));

    backend.save(&sample_creds()).unwrap();

    let raw_bytes = std::fs::read(&path).unwrap();
    let raw = String::from_utf8_lossy(&raw_bytes);

    // None of the secret values should appear in plaintext
    let secrets = [
        "sk-super-secret-token",
        "rt-refresh-me",
        "acct-integration-test",
        "zai-integration-key",
    ];

    for secret in &secrets {
        assert!(
            !raw.contains(secret),
            "secret '{secret}' found in plaintext on disk!"
        );
    }
}

// ── Overwrite replaces previous data ────────────────────────────────────────

#[test]
fn overwrite_replaces_data() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("creds.age");
    let backend = AgeFileBackend::new(path, SecretString::from("pass".to_owned()));

    // Save OpenAI creds
    backend.save(&sample_creds()).unwrap();

    // Overwrite with ZAI-only creds
    let zai_only = Credentials {
        active_provider: Some("zai".into()),
        openai: None,
        zai: Some(ZaiCredentials {
            api_key: "new-key".into(),
        }),
    };
    backend.save(&zai_only).unwrap();

    let loaded = backend.load().unwrap().unwrap();
    assert_eq!(loaded.active_provider.as_deref(), Some("zai"));
    assert!(
        loaded.openai.is_none(),
        "openai should be gone after overwrite"
    );
    assert_eq!(loaded.zai.unwrap().api_key, "new-key");
}

// ── Missing file returns None ───────────────────────────────────────────────

#[test]
fn missing_file_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nonexistent.age");
    let backend = AgeFileBackend::new(path, SecretString::from("anything".to_owned()));

    assert!(backend.load().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn file_permissions_are_restrictive() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("creds.age");
    let backend = AgeFileBackend::new(path.clone(), SecretString::from("perms-test".to_owned()));

    backend.save(&sample_creds()).unwrap();

    let metadata = std::fs::metadata(&path).unwrap();
    let mode = metadata.mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "credentials file should be owner-only (0600), got {mode:o}"
    );
}
