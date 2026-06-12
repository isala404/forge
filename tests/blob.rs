//! blob contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forge::testing::TestDatabase;
use forge::{Bytes, Forge, ForgeConfig, ForgeError, PutOpts};
use std::time::Duration;

#[tokio::test]
async fn put_get_head_delete_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let b = forge.blob();

    assert_eq!(b.get("missing").await.unwrap(), None);
    assert!(b.head("missing").await.unwrap().is_none());
    assert!(!b.delete("missing").await.unwrap());

    b.put(
        "docs/a.txt",
        Bytes::from_static(b"hello"),
        PutOpts::new()
            .with_content_type("text/plain")
            .with_metadata("author", "ada"),
    )
    .await
    .unwrap();

    assert_eq!(
        b.get("docs/a.txt").await.unwrap().unwrap(),
        Bytes::from_static(b"hello")
    );
    let info = b.head("docs/a.txt").await.unwrap().unwrap();
    assert_eq!(info.size, 5);
    assert_eq!(info.content_type, "text/plain");
    assert_eq!(info.metadata.get("author").map(String::as_str), Some("ada"));
    assert!(!info.etag.is_empty());

    // Overwrite: last-write-wins, fresh etag, default content type.
    let etag1 = info.etag;
    b.put("docs/a.txt", Bytes::from_static(b"goodbye"), PutOpts::new())
        .await
        .unwrap();
    let info2 = b.head("docs/a.txt").await.unwrap().unwrap();
    assert_ne!(info2.etag, etag1, "etag changes when bytes change");
    assert_eq!(info2.content_type, "application/octet-stream");

    assert!(b.delete("docs/a.txt").await.unwrap());
    assert!(b.get("docs/a.txt").await.unwrap().is_none());
}

#[tokio::test]
async fn list_is_prefixed_ordered_and_paginated() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let b = forge.blob();

    for k in ["img/b.png", "img/a.png", "doc/x.txt"] {
        b.put(k, Bytes::from_static(b"x"), PutOpts::new())
            .await
            .unwrap();
    }

    let page = b.list("img/", None, 100).await.unwrap();
    let keys: Vec<_> = page.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["img/a.png", "img/b.png"],
        "prefixed + lexicographic"
    );
    assert!(page.next.is_none());

    // Paginate one at a time.
    let p1 = b.list("img/", None, 1).await.unwrap();
    assert_eq!(p1.items.len(), 1);
    assert_eq!(p1.items.first().map(|i| i.key.as_str()), Some("img/a.png"));
    let p2 = b.list("img/", p1.next, 1).await.unwrap();
    assert_eq!(p2.items.first().map(|i| i.key.as_str()), Some("img/b.png"));
}

#[tokio::test]
async fn presign_requires_secret_and_signs() {
    let db = TestDatabase::new().await.unwrap();

    // No secret configured: presigning errors (CRUD still works).
    let plain = db.forge().await.unwrap();
    assert!(matches!(
        plain
            .blob()
            .presign_download("k", Duration::from_secs(60))
            .await,
        Err(ForgeError::Invalid(_))
    ));

    // With a secret: presign produces signed URLs against the same database.
    let signed = Forge::init(ForgeConfig::new(db.url()).with_blob_signing_secret("test-secret"))
        .await
        .unwrap();
    let dl = signed
        .blob()
        .presign_download("exports/a.csv", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(dl.starts_with("/_forge/blob?key="));
    assert!(dl.contains("sig="));

    let up = signed
        .blob()
        .presign_upload("k", Duration::from_secs(60), 1024)
        .await
        .unwrap();
    assert!(up.contains("max_bytes=1024"));

    assert!(matches!(
        signed.blob().presign_download("k", Duration::ZERO).await,
        Err(ForgeError::Invalid(_))
    ));
}

#[tokio::test]
async fn verify_presigned_matches_what_presign_mints() {
    let db = TestDatabase::new().await.unwrap();
    let forge = Forge::init(ForgeConfig::new(db.url()).with_blob_signing_secret("test-secret"))
        .await
        .unwrap();
    let b = forge.blob();

    // Mint an upload URL, then pull the signed params back off it.
    let url = b
        .presign_upload("media/x.bin", Duration::from_secs(600), 4096)
        .await
        .unwrap();
    let q = url.split_once('?').unwrap().1;
    let mut key = String::new();
    let (mut expires, mut max_bytes) = (0i64, 0u64);
    let mut sig = String::new();
    for kv in q.split('&') {
        let (k, v) = kv.split_once('=').unwrap();
        match k {
            // value is percent-encoded; the only escape here is for the path, which
            // verify reconstructs from the same key we pass, so decode minimally.
            "key" => key = v.replace("%2F", "/").replace("%2E", "."),
            "expires" => expires = v.parse().unwrap(),
            "max_bytes" => max_bytes = v.parse().unwrap(),
            "sig" => sig = v.to_string(),
            _ => {}
        }
    }
    assert_eq!(key, "media/x.bin");

    // A faithful PUT verification passes; the matching GET (different method) fails.
    assert!(
        b.verify_presigned("PUT", &key, expires, max_bytes, &sig)
            .await
            .unwrap()
    );
    assert!(
        !b.verify_presigned("GET", &key, expires, max_bytes, &sig)
            .await
            .unwrap()
    );
    // Tampered size, expired URL, and bad method are all rejected.
    assert!(
        !b.verify_presigned("PUT", &key, expires, max_bytes + 1, &sig)
            .await
            .unwrap()
    );
    assert!(
        !b.verify_presigned("PUT", &key, 1, max_bytes, &sig)
            .await
            .unwrap()
    );
    assert!(matches!(
        b.verify_presigned("PATCH", &key, expires, max_bytes, &sig)
            .await,
        Err(ForgeError::Invalid(_))
    ));

    // No signing secret => Config error.
    let plain = db.forge().await.unwrap();
    assert!(matches!(
        plain
            .blob()
            .verify_presigned("PUT", "k", expires, 0, "deadbeef")
            .await,
        Err(ForgeError::Config(_))
    ));
}

#[tokio::test]
async fn limits_are_enforced() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let b = forge.blob();

    let big_key = "x".repeat(1025);
    assert!(matches!(
        b.put(&big_key, Bytes::from_static(b"x"), PutOpts::new())
            .await,
        Err(ForgeError::Limit(_))
    ));
    assert!(matches!(
        b.put("", Bytes::from_static(b"x"), PutOpts::new()).await,
        Err(ForgeError::Invalid(_))
    ));
}
