#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{Bytes, ConditionalGet, ForgeError, PutOpts};
use std::time::Duration;

fn assert_code<T>(result: Result<T, ForgeError>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), expected),
        Ok(_) => panic!("expected {expected} error"),
    }
}

#[tokio::test]
async fn put_get_head_delete_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let b = forge.blob();

    assert_eq!(b.get("missing").await.unwrap(), None);
    assert!(b.head("missing").await.unwrap().is_none());
    b.delete("missing").await.unwrap();

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

    b.delete("docs/a.txt").await.unwrap();
    b.delete("docs/a.txt").await.unwrap();
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
async fn conditional_copy_http_metadata_and_checksum_are_durable() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let blob = forge.blob();
    let checksum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    blob.put(
        "source",
        Bytes::from_static(b"hello"),
        PutOpts::new()
            .with_cache_control("public, max-age=60")
            .with_content_disposition("attachment; filename=hello.txt")
            .with_checksum_sha256(checksum),
    )
    .await
    .unwrap();
    let source = blob.head("source").await.unwrap().unwrap();
    assert_eq!(source.checksum_sha256.as_deref(), Some(checksum));
    assert!(
        blob.verify_checksum_sha256("source", checksum)
            .await
            .unwrap()
    );
    assert!(matches!(
        blob.get_if("source", None, Some(&source.etag))
            .await
            .unwrap(),
        ConditionalGet::NotModified { .. }
    ));
    assert_code(
        blob.get_if("source", Some("wrong"), None).await,
        "PRECONDITION",
    );
    let copied = blob
        .copy("source", "copy", PutOpts::new().create_only())
        .await
        .unwrap();
    assert_eq!(copied.cache_control, source.cache_control);
    assert_eq!(
        blob.get("copy").await.unwrap(),
        Some(Bytes::from_static(b"hello"))
    );
}

#[tokio::test]
async fn presign_requires_secret_and_signs() {
    let db = TestDatabase::new().await.unwrap();

    // No secret configured: presigning errors `Config` (CRUD still works). Missing
    // signing secret is a configuration problem, classified the same way that
    // verify_presigned and blob_router classify it.
    let plain = db.forge().await.unwrap();
    assert_code(
        plain
            .blob()
            .presign_download("k", Duration::from_secs(60))
            .await,
        "CONFIG",
    );

    // With a secret: presign produces signed URLs against the same database.
    let signed = db
        .forge_with("[blob]\nsigning_secret = \"test-secret\"\n")
        .await
        .unwrap();
    let dl = signed
        .blob()
        .presign_download("exports/a.csv", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(dl.url.starts_with("/api/files?v=1&"));
    assert!(dl.url.contains("sig="));

    let up = signed
        .blob()
        .presign_upload("k", Duration::from_secs(60), 1024)
        .await
        .unwrap();
    assert!(up.url.contains("max_bytes=1024"));

    assert_code(
        signed.blob().presign_download("k", Duration::ZERO).await,
        "INVALID",
    );

    // A 0-byte upload cap admits only empty bodies, so it is rejected.
    assert_code(
        signed
            .blob()
            .presign_upload("k", Duration::from_secs(60), 0)
            .await,
        "INVALID",
    );
}

#[tokio::test]
async fn verify_presigned_matches_what_presign_mints() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db
        .forge_with("[blob]\nsigning_secret = \"test-secret\"\n")
        .await
        .unwrap();
    let b = forge.blob();

    let ticket = b
        .presign_upload("media/x.bin", Duration::from_secs(600), 4096)
        .await
        .unwrap();
    let key = ticket.key;
    let expires = ticket.expires_epoch;
    let max_bytes = ticket.max_bytes;
    let sig = ticket.signature;

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
    assert_code(
        b.verify_presigned("PATCH", &key, expires, max_bytes, &sig)
            .await,
        "INVALID",
    );

    // No signing secret => Config error.
    let plain = db.forge().await.unwrap();
    assert_code(
        plain
            .blob()
            .verify_presigned("PUT", "k", expires, 0, "deadbeef")
            .await,
        "CONFIG",
    );
}

#[tokio::test]
async fn limits_are_enforced() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let b = forge.blob();

    let big_key = "x".repeat(1025);
    assert_code(
        b.put(&big_key, Bytes::from_static(b"x"), PutOpts::new())
            .await,
        "LIMIT",
    );
    assert_code(
        b.put("", Bytes::from_static(b"x"), PutOpts::new()).await,
        "INVALID",
    );
}
