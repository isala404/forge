//! auth contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forge::testing::TestDatabase;
use forge::{ForgeError, PhcString, SessionOpts};
use std::time::Duration;

#[tokio::test]
async fn password_hash_verify_and_rehash() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let hash = a
        .hash_password("correct horse battery staple")
        .await
        .unwrap();
    assert!(hash.as_str().starts_with("$argon2id$"));
    assert!(
        a.verify_password("correct horse battery staple", &hash)
            .await
            .unwrap()
    );
    assert!(!a.verify_password("wrong", &hash).await.unwrap());
    assert!(!a.needs_rehash(&hash), "fresh hash uses current params");

    // A malformed PHC string: Invalid on verify, true on needs_rehash (rehash it).
    let bad = PhcString::new("not-a-phc-string");
    assert!(matches!(
        a.verify_password("x", &bad).await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(a.needs_rehash(&bad));

    assert!(matches!(
        a.hash_password("").await,
        Err(ForgeError::Invalid(_))
    ));
}

#[tokio::test]
async fn sessions_create_validate_revoke() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let token = a
        .create_session("user-1", SessionOpts::new())
        .await
        .unwrap();
    assert!(!format!("{token:?}").contains(token.as_str()));

    let s = a.validate_session(token.as_str()).await.unwrap().unwrap();
    assert_eq!(s.user_id, "user-1");
    assert!(s.expires_at > s.created_at);

    assert!(a.validate_session("unknown-token").await.unwrap().is_none());

    a.revoke_session(token.as_str()).await.unwrap();
    assert!(a.validate_session(token.as_str()).await.unwrap().is_none());
    a.revoke_session(token.as_str()).await.unwrap();
}

#[tokio::test]
async fn revoke_all_sessions_counts() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let t1 = a
        .create_session("user-x", SessionOpts::new())
        .await
        .unwrap();
    let _t2 = a
        .create_session("user-x", SessionOpts::new())
        .await
        .unwrap();
    assert_eq!(a.revoke_all_sessions("user-x").await.unwrap(), 2);
    assert!(a.validate_session(t1.as_str()).await.unwrap().is_none());
    assert_eq!(a.revoke_all_sessions("user-x").await.unwrap(), 0);
}

#[tokio::test]
async fn absolute_timeout_expires_the_session() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let opts = SessionOpts::new()
        .with_idle_timeout(Duration::from_secs(1))
        .with_absolute_timeout(Duration::from_secs(1));
    let token = a.create_session("u", opts).await.unwrap();
    assert!(a.validate_session(token.as_str()).await.unwrap().is_some());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        a.validate_session(token.as_str()).await.unwrap().is_none(),
        "past the absolute deadline"
    );
}

#[tokio::test]
async fn api_keys_create_verify_revoke() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let key = a.create_api_key("owner-1", "ci-token").await.unwrap();
    assert!(key.secret.as_str().starts_with("fk_"));
    assert!(!format!("{:?}", key.secret).contains(key.secret.as_str()));

    let info = a
        .verify_api_key(key.secret.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.owner_id, "owner-1");
    assert_eq!(info.label, "ci-token");
    assert_eq!(info.id, key.id);

    assert!(a.verify_api_key("fk_unknown").await.unwrap().is_none());

    assert!(a.revoke_api_key(&key.id).await.unwrap());
    assert!(
        a.verify_api_key(key.secret.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(!a.revoke_api_key("no-such-id").await.unwrap());
}
