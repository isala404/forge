//! End-to-end check for the zero-config embedded Postgres backend.
//!
//! `#[ignore]`d because it downloads (~25MB, cached) and runs a real Postgres server, which
//! is too heavy for the default `cargo test` run. Exercise it explicitly:
//!
//! ```bash
//! cargo test --features embedded-postgres --test embedded -- --ignored --nocapture
//! ```
#![cfg(feature = "embedded-postgres")]

use forgelib::{Forge, SetOpts};

#[tokio::test]
#[ignore = "downloads and runs a native Postgres; run explicitly with --ignored"]
async fn embedded_boots_and_serves_a_primitive() {
    let dir = std::env::temp_dir().join("forge-embedded-e2e");
    let toml = format!(
        "[postgres]\nurl = \"embedded\"\nembedded_data_dir = \"{}\"\n",
        dir.display()
    );

    let forge = Forge::init_from_str(&toml)
        .await
        .expect("embedded postgres should boot and migrate");

    // A real round-trip through the KV primitive proves the wire path end-to-end.
    forge
        .kv()
        .set("greeting", "hello".into(), SetOpts::new())
        .await
        .expect("set");
    let got = forge.kv().get("greeting").await.expect("get");
    assert_eq!(got.as_deref(), Some(&b"hello"[..]));

    // A persisted counter: each run over the same data directory sees the previous run's
    // value, proving the server (and its data) survives a full restart — durability.
    let boots = forge.kv().incr("e2e:boots", 1).await.expect("incr");
    assert!(boots >= 1, "boot counter should persist and grow: {boots}");
    println!("embedded boot count (grows across restarts): {boots}");

    // The report should show the durable Postgres backend, not memory.
    let report = forge.backend_report();
    assert!(
        report.backends.iter().all(|b| b.provider == "postgres"),
        "every primitive should be postgres-backed: {report}"
    );
    println!("embedded backend report:\n{report}");
}
