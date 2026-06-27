//! kv contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forge::testing::TestDatabase;
use forge::{ForgeConfig, ForgeError, SetMode, SetOpts};
use std::time::Duration;

fn b(s: &str) -> Bytes {
    Bytes::from(s.as_bytes().to_vec())
}

#[tokio::test]
async fn set_then_get_roundtrips() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    assert!(
        kv.set("greeting", b("hello"), SetOpts::new())
            .await
            .unwrap()
    );
    assert_eq!(kv.get("greeting").await.unwrap(), Some(b("hello")));
    assert_eq!(kv.get("missing").await.unwrap(), None);
}

#[tokio::test]
async fn set_nx_blocks_live_key_but_reclaims_expired() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();
    let nx = SetOpts::new().with_mode(SetMode::IfNotExists);

    assert!(
        kv.set("k", b("first"), nx.clone()).await.unwrap(),
        "first NX writes"
    );
    assert!(
        !kv.set("k", b("second"), nx.clone()).await.unwrap(),
        "second NX blocked"
    );
    assert_eq!(kv.get("k").await.unwrap(), Some(b("first")));

    // An expired key must not block NX.
    let ttl = SetOpts::new().with_ttl(Duration::from_secs(1));
    assert!(kv.set("e", b("x"), ttl).await.unwrap());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        kv.set("e", b("y"), nx).await.unwrap(),
        "NX reclaims expired key"
    );
    assert_eq!(kv.get("e").await.unwrap(), Some(b("y")));
}

#[tokio::test]
async fn set_xx_writes_only_when_present() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();
    let xx = SetOpts::new().with_mode(SetMode::IfExists);

    assert!(
        !kv.set("k", b("v"), xx.clone()).await.unwrap(),
        "XX on absent => false"
    );
    kv.set("k", b("v0"), SetOpts::new()).await.unwrap();
    assert!(
        kv.set("k", b("v1"), xx).await.unwrap(),
        "XX on present => true"
    );
    assert_eq!(kv.get("k").await.unwrap(), Some(b("v1")));
}

#[tokio::test]
async fn delete_reports_whether_removed() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    kv.set("k", b("v"), SetOpts::new()).await.unwrap();
    assert!(kv.delete("k").await.unwrap(), "removing a live key => true");
    assert!(
        !kv.delete("k").await.unwrap(),
        "removing an absent key => false"
    );
}

#[tokio::test]
async fn exists_respects_expiry() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    kv.set("k", b("v"), SetOpts::new().with_ttl(Duration::from_secs(1)))
        .await
        .unwrap();
    assert!(kv.exists("k").await.unwrap());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!kv.exists("k").await.unwrap(), "expired key does not exist");
    assert_eq!(
        kv.get("k").await.unwrap(),
        None,
        "get after expiry is None, guaranteed"
    );
}

#[tokio::test]
async fn incr_starts_from_zero_and_is_a_string_value() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    assert_eq!(kv.incr("c", 1).await.unwrap(), 1, "missing key starts at 0");
    assert_eq!(kv.incr("c", 10).await.unwrap(), 11);
    assert_eq!(kv.incr("c", -5).await.unwrap(), 6);
    // Counter is a string value (Redis): get returns decimal ASCII.
    assert_eq!(kv.get("c").await.unwrap(), Some(b("6")));
}

#[tokio::test]
async fn incr_on_non_numeric_value_is_invalid() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    kv.set("s", b("not-a-number"), SetOpts::new())
        .await
        .unwrap();
    assert!(matches!(kv.incr("s", 1).await, Err(ForgeError::Invalid(_))));
}

#[tokio::test]
async fn incr_on_non_utf8_value_is_invalid() {
    // Non-UTF-8 value: Postgres raises 22021 (not 22P02) before the int cast; both must surface as Invalid, not Backend.
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    kv.set("c", Bytes::from(vec![0xFF, 0xFE]), SetOpts::new())
        .await
        .unwrap();
    assert!(matches!(kv.incr("c", 1).await, Err(ForgeError::Invalid(_))));
}

#[tokio::test]
async fn expire_sets_ttl_and_does_not_create_keys() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    assert!(
        !kv.expire("ghost", Duration::from_secs(10)).await.unwrap(),
        "expire on absent => false"
    );
    kv.set("k", b("v"), SetOpts::new()).await.unwrap();
    assert!(kv.expire("k", Duration::from_secs(1)).await.unwrap());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(kv.get("k").await.unwrap(), None);
}

#[tokio::test]
async fn compare_and_swap_guards_writes() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    // old = None means "expected absent".
    assert!(kv.compare_and_swap("k", None, b("v1")).await.unwrap());
    assert!(
        !kv.compare_and_swap("k", None, b("v2")).await.unwrap(),
        "key now present"
    );
    assert!(
        kv.compare_and_swap("k", Some(b("v1")), b("v2"))
            .await
            .unwrap()
    );
    assert!(
        !kv.compare_and_swap("k", Some(b("v1")), b("v3"))
            .await
            .unwrap(),
        "stale expected value"
    );
    assert_eq!(kv.get("k").await.unwrap(), Some(b("v2")));
}

#[tokio::test]
async fn scan_paginates_by_prefix() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    for i in 0..10 {
        kv.set(&format!("user{i:02}"), b("x"), SetOpts::new())
            .await
            .unwrap();
    }
    kv.set("other", b("y"), SetOpts::new()).await.unwrap();

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let (keys, next) = kv.scan("user", cursor, 3).await.unwrap();
        seen.extend(keys);
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    seen.sort();
    assert_eq!(seen.len(), 10, "exactly the 10 user* keys");
    assert_eq!(seen.first().map(String::as_str), Some("user00"));
    assert!(
        !seen.iter().any(|k| k == "other"),
        "prefix excludes non-matching keys"
    );
}

#[tokio::test]
async fn size_limits_are_rejected() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    let big_key = "x".repeat(513);
    assert!(matches!(kv.get(&big_key).await, Err(ForgeError::Limit(_))));

    let big_val = Bytes::from(vec![0u8; 1024 * 1024 + 1]);
    assert!(matches!(
        kv.set("k", big_val, SetOpts::new()).await,
        Err(ForgeError::Limit(_))
    ));
}

#[tokio::test]
async fn colon_keys_are_allowed_redis_style() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    // Redis idiom `entity:id:field` must work; agents generate it constantly.
    kv.set("user:42:session", b("tok"), SetOpts::new())
        .await
        .unwrap();
    assert_eq!(kv.get("user:42:session").await.unwrap(), Some(b("tok")));
    let (keys, _) = kv.scan("user:42:", None, 10).await.unwrap();
    assert_eq!(keys, vec!["user:42:session".to_string()]);
}

#[tokio::test]
async fn namespaces_isolate_keys() {
    let db = TestDatabase::new().await.unwrap();
    let a = forge::Forge::init(ForgeConfig::new(db.url()).with_kv_namespace("app_a"))
        .await
        .unwrap();
    let bb = forge::Forge::init(ForgeConfig::new(db.url()).with_kv_namespace("app_b"))
        .await
        .unwrap();

    a.kv()
        .set("shared", b("from-a"), SetOpts::new())
        .await
        .unwrap();
    bb.kv()
        .set("shared", b("from-b"), SetOpts::new())
        .await
        .unwrap();

    assert_eq!(a.kv().get("shared").await.unwrap(), Some(b("from-a")));
    assert_eq!(bb.kv().get("shared").await.unwrap(), Some(b("from-b")));

    let (keys, _) = a.kv().scan("", None, 100).await.unwrap();
    assert_eq!(keys, vec!["shared".to_string()]);
}

#[tokio::test]
async fn mget_returns_values_in_input_order_with_holes() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let kv = forge.kv();

    kv.set("a", b("1"), SetOpts::new()).await.unwrap();
    kv.set("c", b("3"), SetOpts::new()).await.unwrap();
    // "b" is never set; an expired key must read as a hole too.
    kv.set("d", b("4"), SetOpts::new().with_ttl(Duration::from_secs(1)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let got = kv.mget(&["a", "b", "c", "d", "a"]).await.unwrap();
    assert_eq!(
        got,
        vec![Some(b("1")), None, Some(b("3")), None, Some(b("1"))]
    );

    assert_eq!(kv.mget(&[]).await.unwrap(), Vec::<Option<Bytes>>::new());
}

#[tokio::test]
async fn mget_respects_namespaces() {
    let db = TestDatabase::new().await.unwrap();
    let a = forge::Forge::init(ForgeConfig::new(db.url()).with_kv_namespace("ns_a"))
        .await
        .unwrap();
    let bb = forge::Forge::init(ForgeConfig::new(db.url()).with_kv_namespace("ns_b"))
        .await
        .unwrap();
    a.kv().set("k", b("from-a"), SetOpts::new()).await.unwrap();
    bb.kv().set("k", b("from-b"), SetOpts::new()).await.unwrap();

    assert_eq!(a.kv().mget(&["k"]).await.unwrap(), vec![Some(b("from-a"))]);
    assert_eq!(bb.kv().mget(&["k"]).await.unwrap(), vec![Some(b("from-b"))]);
}
