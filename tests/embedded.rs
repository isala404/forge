//! Smoke test for `[postgres] embedded = true`. Needs no external database, but the
//! first run on a machine downloads the Postgres binaries (network) and initializes
//! a cluster, so it lives behind the `embedded` feature rather than in the default
//! test set: `cargo test --features embedded --test embedded`.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::{Forge, SetOpts};

#[tokio::test]
async fn embedded_server_boots_migrates_and_persists() {
    let dir = std::env::temp_dir().join(format!("forge-embedded-test-{}", std::process::id()));
    // A previous crashed run must not poison this one.
    let _ = std::fs::remove_dir_all(&dir);

    let toml = format!(
        "[postgres]\nembedded = true\nembedded_dir = \"{}\"",
        dir.display()
    );

    // First boot: download (if needed), initdb, migrate, serve.
    {
        let forge = Forge::init_from_str(&toml)
            .await
            .expect("first embedded boot");
        forge
            .kv()
            .set("embedded:k", "v1".into(), SetOpts::new())
            .await
            .expect("kv set on embedded server");
        let got = forge.kv().get("embedded:k").await.expect("kv get");
        assert_eq!(got.as_deref(), Some(b"v1".as_slice()));
        // Dropping the handle stops the postmaster; the data directory stays.
    }

    // Second boot: same data directory and password, fresh port; data survived.
    {
        let forge = Forge::init_from_str(&toml)
            .await
            .expect("second embedded boot (persistence)");
        let got = forge
            .kv()
            .get("embedded:k")
            .await
            .expect("kv get after restart");
        assert_eq!(got.as_deref(), Some(b"v1".as_slice()));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// An explicit url outranks `embedded = true` (the pattern `url = "${VAR:-}"` +
/// `embedded = true` must deploy against $VAR). The url points at a dead port, so
/// the attempt must fail with a connection error — not boot an embedded server.
#[tokio::test]
async fn explicit_url_wins_over_embedded() {
    let Err(err) = Forge::init_from_str(
        "[postgres]\nurl = \"postgres://127.0.0.1:1/nope\"\nembedded = true\nacquire_timeout_secs = 2",
    )
    .await
    else {
        panic!("connecting to a dead port must fail");
    };
    assert!(
        err.to_string().contains("could not connect"),
        "expected a connection failure to the explicit url, got: {err}"
    );
}
