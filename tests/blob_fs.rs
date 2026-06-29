//! Filesystem blob backend held to the same `Blob` contract as the Postgres (`BYTEA`)
//! backend, plus the filesystem-specific orphan sweep and binary-safety guarantees.
//! Run with `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::disallowed_methods)]

use forgelib::testing::TestDatabase;
use forgelib::{Bytes, Forge, ForgeError, PutOpts};
use sqlx::{Connection, PgConnection};
use std::time::Duration;

/// A unique scratch directory for this test's blob bytes, derived from the unique test
/// database name so parallel tests never share a root.
fn temp_root(db: &TestDatabase) -> std::path::PathBuf {
    let name = db.url().rsplit('/').next().unwrap_or("forge_fs");
    std::env::temp_dir().join(format!("forge_fs_blob_{name}"))
}

async fn fs_forge(db: &TestDatabase, root: &std::path::Path) -> Forge {
    Forge::init_from_str(&db.config_toml(&format!(
        "[blob]\nbackend = \"fs\"\nfs_root = \"{}\"\nsigning_secret = \"test-secret\"\n",
        root.display()
    )))
    .await
    .unwrap()
}

async fn data_ref(db: &TestDatabase, key: &str) -> String {
    let mut conn = PgConnection::connect(db.url()).await.unwrap();
    let data_ref =
        sqlx::query_scalar::<_, String>("SELECT data_ref FROM forge_fs_blobs WHERE key = $1")
            .bind(key)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    conn.close().await.ok();
    data_ref
}

#[tokio::test]
async fn fs_put_get_head_delete_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
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
    assert_eq!(
        b.get("docs/a.txt").await.unwrap().unwrap(),
        Bytes::from_static(b"goodbye")
    );

    assert!(b.delete("docs/a.txt").await.unwrap());
    assert!(b.get("docs/a.txt").await.unwrap().is_none());
}

#[tokio::test]
async fn fs_list_is_prefixed_ordered_and_paginated() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
    let b = forge.blob();

    for k in ["img/b.png", "img/a.png", "doc/x.txt"] {
        b.put(k, Bytes::from_static(b"x"), PutOpts::new())
            .await
            .unwrap();
    }

    let page = b.list("img/", None, 100).await.unwrap();
    let keys: Vec<_> = page.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, vec!["img/a.png", "img/b.png"]);
    assert!(page.next.is_none());

    let p1 = b.list("img/", None, 1).await.unwrap();
    assert_eq!(p1.items.first().map(|i| i.key.as_str()), Some("img/a.png"));
    let p2 = b.list("img/", p1.next, 1).await.unwrap();
    assert_eq!(p2.items.first().map(|i| i.key.as_str()), Some("img/b.png"));
}

#[tokio::test]
async fn fs_missing_data_file_is_not_found_across_get_head_and_list() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
    let b = forge.blob();

    b.put("ghost/file.txt", Bytes::from_static(b"x"), PutOpts::new())
        .await
        .unwrap();
    db.execute_raw(
        "UPDATE forge_fs_blobs SET data_ref = 'missing/file' WHERE key = 'ghost/file.txt'",
    )
    .await
    .unwrap();

    assert!(b.get("ghost/file.txt").await.unwrap().is_none());
    assert!(b.head("ghost/file.txt").await.unwrap().is_none());
    let page = b.list("ghost/", None, 100).await.unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn fs_overwrite_keeps_old_file_until_orphan_grace() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
    let b = forge.blob();

    b.put("docs/race.txt", Bytes::from_static(b"old"), PutOpts::new())
        .await
        .unwrap();
    let old_ref = data_ref(&db, "docs/race.txt").await;
    let old_path = root.join(&old_ref);
    let old_file = std::fs::File::open(&old_path).unwrap();
    old_file
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .unwrap();

    b.put("docs/race.txt", Bytes::from_static(b"new"), PutOpts::new())
        .await
        .unwrap();
    forge.maintain().await.unwrap();

    assert!(
        old_path.exists(),
        "an overwritten file remains readable through the orphan grace window"
    );
    assert_eq!(
        b.get("docs/race.txt").await.unwrap().unwrap(),
        Bytes::from_static(b"new")
    );
}

#[tokio::test]
async fn fs_is_binary_safe() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
    let b = forge.blob();

    // Non-UTF-8 bytes must round-trip exactly (the bytes go straight to disk).
    let raw = Bytes::from_static(&[0u8, 159, 146, 150, 255, 0, 1, 2]);
    b.put("bin/blob", raw.clone(), PutOpts::new())
        .await
        .unwrap();
    assert_eq!(b.get("bin/blob").await.unwrap().unwrap(), raw);
}

#[tokio::test]
async fn fs_presign_and_verify() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;
    let b = forge.blob();

    let url = b
        .presign_upload("media/x.bin", Duration::from_secs(600), 4096)
        .await
        .unwrap();
    assert!(url.contains("max_bytes=4096"));
    let q = url.split_once('?').unwrap().1;
    let (mut expires, mut max_bytes) = (0i64, 0u64);
    let mut sig = String::new();
    for kv in q.split('&') {
        let (k, v) = kv.split_once('=').unwrap();
        match k {
            "expires" => expires = v.parse().unwrap(),
            "max_bytes" => max_bytes = v.parse().unwrap(),
            "sig" => sig = v.to_string(),
            _ => {}
        }
    }
    assert!(
        b.verify_presigned("PUT", "media/x.bin", expires, max_bytes, &sig)
            .await
            .unwrap()
    );

    // No secret on a filesystem backend => Config, same as the Postgres backend.
    let plain = Forge::init_from_str(&db.config_toml(&format!(
        "[blob]\nbackend = \"fs\"\nfs_root = \"{}\"\n",
        root.display()
    )))
    .await
    .unwrap();
    assert!(matches!(
        plain
            .blob()
            .presign_download("k", Duration::from_secs(60))
            .await,
        Err(ForgeError::Config(_))
    ));
}

#[tokio::test]
async fn fs_backend_report_and_maintain() {
    let db = TestDatabase::new().await.unwrap();
    let root = temp_root(&db);
    let forge = fs_forge(&db, &root).await;

    // The report shows the filesystem provider for blob, Postgres for the rest.
    let report = forge.backend_report();
    let blob = report
        .backends
        .iter()
        .find(|b| b.primitive == forgelib::Primitive::Blob)
        .unwrap();
    assert_eq!(blob.provider, "filesystem");
    let kv = report
        .backends
        .iter()
        .find(|b| b.primitive == forgelib::Primitive::Kv)
        .unwrap();
    assert_eq!(kv.provider, "postgres");
    assert_eq!(report.backends.len(), 8, "one backend entry per primitive");

    // Maintenance (including the filesystem orphan sweep) runs cleanly.
    forge
        .blob()
        .put("keep", Bytes::from_static(b"k"), PutOpts::new())
        .await
        .unwrap();
    forge.maintain().await.unwrap();
    // The kept object survives a sweep.
    assert_eq!(
        forge.blob().get("keep").await.unwrap().unwrap(),
        Bytes::from_static(b"k")
    );
}
