#![cfg(feature = "s3-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{Bytes, ConditionalGet, Forge, ForgeError, PutOpts, S3Encryption};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn s3_fragment(bucket: &str, access_key: &str, secret_key: &str) -> String {
    let endpoint = std::env::var("S3_TEST_ENDPOINT").expect("S3_TEST_ENDPOINT is required");
    let region = std::env::var("S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    format!(
        "[blob]\nbackend = \"s3\"\nbucket = \"{bucket}\"\nregion = \"{region}\"\nendpoint = \"{endpoint}\"\naccess_key = \"{access_key}\"\nsecret_key = \"{secret_key}\"\npath_style = true\nconnect_timeout_secs = 1\nrequest_timeout_secs = 5\nmax_retries = 2\nsigning_secret = \"proxy-test-secret\"\n"
    )
}

async fn forge(db: &TestDatabase) -> Forge {
    let bucket = std::env::var("S3_TEST_BUCKET").expect("S3_TEST_BUCKET is required");
    let access = std::env::var("S3_TEST_ACCESS_KEY").expect("S3_TEST_ACCESS_KEY is required");
    let secret = std::env::var("S3_TEST_SECRET_KEY").expect("S3_TEST_SECRET_KEY is required");
    db.forge_with(&s3_fragment(&bucket, &access, &secret))
        .await
        .unwrap()
}

#[tokio::test]
async fn s3_crud_pagination_unicode_ranges_and_preconditions() {
    let db = TestDatabase::new().await.unwrap();
    let forge = forge(&db).await;
    let blob = forge.blob();
    let key = "unicode/හෙලෝ-東京.txt";
    blob.delete(key).await.unwrap();
    blob.put(
        key,
        Bytes::from_static(b"hello world"),
        PutOpts::new()
            .with_content_type("text/plain")
            .with_metadata("owner", "alice")
            .create_only(),
    )
    .await
    .unwrap();
    let duplicate = blob
        .put(
            key,
            Bytes::from_static(b"duplicate"),
            PutOpts::new().create_only(),
        )
        .await;
    assert_eq!(duplicate.unwrap_err().code(), "PRECONDITION");

    let head = blob.head(key).await.unwrap().unwrap();
    assert_eq!(head.content_type, "text/plain");
    assert_eq!(
        head.metadata.get("owner").map(String::as_str),
        Some("alice")
    );
    let etag = head.etag.clone();
    blob.put(
        key,
        Bytes::from_static(b"hello again"),
        PutOpts::new().match_version(etag),
    )
    .await
    .unwrap();
    assert_eq!(
        blob.put(
            key,
            Bytes::from_static(b"stale"),
            PutOpts::new().match_version("stale-version"),
        )
        .await
        .unwrap_err()
        .code(),
        "PRECONDITION"
    );
    assert_eq!(
        blob.get_range(key, 6, 10).await.unwrap(),
        Some(Bytes::from_static(b"again"))
    );
    let mut reader = blob.open(key).await.unwrap().unwrap();
    let mut streamed = Vec::new();
    reader.read_to_end(&mut streamed).await.unwrap();
    assert_eq!(streamed, b"hello again");

    for suffix in ["a", "b", "c"] {
        blob.put(
            &format!("page/{suffix}"),
            Bytes::from_static(b"x"),
            PutOpts::new(),
        )
        .await
        .unwrap();
    }
    let first = blob.list("page/", None, 2).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next.is_some());
    let second = blob.list("page/", first.next, 2).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next.is_none());
    assert!(first.items.iter().all(|item| !item.etag.is_empty()));

    blob.delete(key).await.unwrap();
    blob.delete(key).await.unwrap();
    assert!(blob.head(key).await.unwrap().is_none());
}

#[tokio::test]
async fn s3_multipart_stream_aborts_on_short_input_and_completes_large_input() {
    let db = TestDatabase::new().await.unwrap();
    let forge = forge(&db).await;
    let blob = forge.blob();
    let declared = 51 * 1024 * 1024;
    blob.delete("multipart/interrupted").await.unwrap();
    blob.delete("multipart/complete").await.unwrap();

    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let write = tokio::spawn(async move {
        writer.write_all(&vec![7u8; 1024 * 1024]).await.unwrap();
    });
    let interrupted = blob
        .put_stream(
            "multipart/interrupted",
            Box::pin(reader),
            declared,
            PutOpts::new(),
        )
        .await;
    assert_eq!(interrupted.unwrap_err().code(), "INVALID");
    write.await.unwrap();
    assert!(blob.head("multipart/interrupted").await.unwrap().is_none());

    let body = vec![9u8; declared as usize];
    blob.put_stream(
        "multipart/complete",
        Box::pin(std::io::Cursor::new(body)),
        declared,
        PutOpts::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        blob.head("multipart/complete").await.unwrap().unwrap().size,
        declared
    );
}

#[tokio::test]
async fn s3_conditional_reads_copy_checksums_headers_encryption_and_multipart_handles() {
    let db = TestDatabase::new().await.unwrap();
    let forge = forge(&db).await;
    let blob = forge.blob();
    let checksum = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    for key in [
        "ergonomics/source.txt",
        "ergonomics/copy.txt",
        "ergonomics/multipart.bin",
        "ergonomics/abandoned.bin",
    ] {
        blob.delete(key).await.unwrap();
    }

    blob.put(
        "ergonomics/source.txt",
        Bytes::from_static(b"hello world"),
        PutOpts::new()
            .with_content_type("text/plain")
            .with_metadata("owner", "alice")
            .with_cache_control("public, max-age=60")
            .with_content_disposition("attachment; filename=source.txt")
            .with_checksum_sha256(checksum),
    )
    .await
    .unwrap();
    let info = blob.head("ergonomics/source.txt").await.unwrap().unwrap();
    assert_eq!(info.cache_control.as_deref(), Some("public, max-age=60"));
    assert_eq!(
        info.content_disposition.as_deref(),
        Some("attachment; filename=source.txt")
    );
    assert_eq!(info.checksum_sha256.as_deref(), Some(checksum));
    let encrypted_presign = blob
        .presign_native_put(
            "ergonomics/encrypted.txt",
            Duration::from_secs(60),
            PutOpts::new().with_s3_encryption(S3Encryption::S3Managed),
        )
        .await
        .unwrap();
    assert_eq!(
        encrypted_presign
            .required_headers
            .get("x-amz-server-side-encryption")
            .map(String::as_str),
        Some("AES256")
    );
    assert!(matches!(
        blob.get_if("ergonomics/source.txt", Some(&info.etag), None)
            .await
            .unwrap(),
        ConditionalGet::Found { body, .. } if body == Bytes::from_static(b"hello world")
    ));
    assert!(matches!(
        blob.get_if("ergonomics/source.txt", None, Some(&info.etag))
            .await
            .unwrap(),
        ConditionalGet::NotModified { .. }
    ));
    assert_eq!(
        blob.get_if("ergonomics/source.txt", Some("wrong"), None)
            .await
            .unwrap_err()
            .code(),
        "PRECONDITION"
    );
    let copied = blob
        .copy(
            "ergonomics/source.txt",
            "ergonomics/copy.txt",
            PutOpts::new(),
        )
        .await
        .unwrap();
    assert_eq!(copied.cache_control, info.cache_control);
    assert_eq!(copied.content_disposition, info.content_disposition);
    assert!(
        blob.verify_checksum_sha256("ergonomics/copy.txt", checksum)
            .await
            .unwrap()
    );

    let upload = blob
        .create_multipart(
            "ergonomics/multipart.bin",
            PutOpts::new().with_cache_control("private, max-age=0"),
        )
        .await
        .unwrap();
    let first = blob
        .upload_part(&upload, 1, Bytes::from(vec![7; 5 * 1024 * 1024]))
        .await
        .unwrap();
    let second = blob
        .upload_part(&upload, 2, Bytes::from_static(b"tail"))
        .await
        .unwrap();
    let completed = blob
        .complete_multipart(&upload, vec![first, second])
        .await
        .unwrap();
    assert_eq!(completed.size, 5 * 1024 * 1024 + 4);
    assert_eq!(
        completed.cache_control.as_deref(),
        Some("private, max-age=0")
    );

    let abandoned = blob
        .create_multipart("ergonomics/abandoned.bin", PutOpts::new())
        .await
        .unwrap();
    blob.abort_multipart(&abandoned).await.unwrap();
    blob.abort_multipart(&abandoned).await.unwrap();
}

#[tokio::test]
async fn native_presigned_put_and_get_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let forge = forge(&db).await;
    let blob = forge.blob();
    let client = reqwest::Client::new();
    blob.delete("native/ticket.txt").await.unwrap();

    let put = blob
        .presign_native_put(
            "native/ticket.txt",
            Duration::from_secs(60),
            PutOpts::new().with_content_type("text/plain"),
        )
        .await
        .unwrap();
    assert_eq!(
        put.constraints.get("maximum_body_size").map(String::as_str),
        Some("not_portably_enforced")
    );
    let mut request = client.put(&put.url).body("native-body");
    for (name, value) in &put.required_headers {
        request = request.header(name, value);
    }
    assert!(request.send().await.unwrap().status().is_success());

    let get = blob
        .presign_native_get("native/ticket.txt", Duration::from_secs(60))
        .await
        .unwrap();
    let mut request = client.get(&get.url);
    for (name, value) in &get.required_headers {
        request = request.header(name, value);
    }
    let response = request.send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"native-body");
}

#[tokio::test]
async fn startup_probe_rejects_bad_credentials_and_missing_bucket() {
    let db = TestDatabase::new().await.unwrap();
    let bucket = std::env::var("S3_TEST_BUCKET").unwrap();
    let config = db.config_toml(&s3_fragment(&bucket, "expired", "expired"));
    assert!(matches!(
        Forge::init_from_str(&config).await,
        Err(ForgeError::Config(_))
    ));

    let access = std::env::var("S3_TEST_ACCESS_KEY").unwrap();
    let secret = std::env::var("S3_TEST_SECRET_KEY").unwrap();
    let expired_session = s3_fragment(&bucket, &access, &secret).replace(
        "path_style = true",
        "session_token = \"expired-session-token\"\npath_style = true",
    );
    let config = db.config_toml(&expired_session);
    assert!(matches!(
        Forge::init_from_str(&config).await,
        Err(ForgeError::Config(_))
    ));

    let config = db.config_toml(&s3_fragment("forge-missing-bucket", &access, &secret));
    assert!(matches!(
        Forge::init_from_str(&config).await,
        Err(ForgeError::Config(_))
    ));

    let denied_access =
        std::env::var("S3_TEST_DENIED_ACCESS_KEY").expect("S3_TEST_DENIED_ACCESS_KEY is required");
    let denied_secret =
        std::env::var("S3_TEST_DENIED_SECRET_KEY").expect("S3_TEST_DENIED_SECRET_KEY is required");
    let config = db.config_toml(&s3_fragment(&bucket, &denied_access, &denied_secret));
    assert!(matches!(
        Forge::init_from_str(&config).await,
        Err(ForgeError::Config(_))
    ));
}
