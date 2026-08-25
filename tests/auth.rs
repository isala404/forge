#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{ApiKeyOpts, Bytes, PhcString, SessionOpts};
use std::collections::HashMap;
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
    assert_eq!(
        a.verify_password("x", &bad).await.unwrap_err().code(),
        "INVALID"
    );
    assert!(a.needs_rehash(&bad));

    assert_eq!(a.hash_password("").await.unwrap_err().code(), "INVALID");
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
async fn one_time_tokens_consume_once_purpose_scoped() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let token = a
        .create_token("user-7", "password-reset", Duration::from_secs(900))
        .await
        .unwrap();
    assert!(!format!("{token:?}").contains(token.as_str()));

    // Wrong purpose leaves the token intact.
    assert!(
        a.consume_token(token.as_str(), "email-verify")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        a.consume_token(token.as_str(), "password-reset")
            .await
            .unwrap()
            .as_deref(),
        Some("user-7")
    );
    assert!(
        a.consume_token(token.as_str(), "password-reset")
            .await
            .unwrap()
            .is_none(),
        "single use"
    );
    assert!(
        a.consume_token("unknown-token", "password-reset")
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(
        a.create_token("u", "p", Duration::ZERO)
            .await
            .unwrap_err()
            .code(),
        "INVALID"
    );
    assert_eq!(
        a.create_token("u", "", Duration::from_secs(60))
            .await
            .unwrap_err()
            .code(),
        "INVALID"
    );
}

#[tokio::test]
async fn one_time_token_payload_is_bounded_and_consumed_atomically() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let auth = forge.auth();
    let token = auth
        .create_token_with_payload(
            "user-7",
            "password-reset",
            Duration::from_secs(900),
            Bytes::from_static(b"return-to=/settings"),
        )
        .await
        .unwrap();
    let consumed = auth
        .consume_token_with_payload(token.as_str(), "password-reset")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(consumed.user_id, "user-7");
    assert_eq!(consumed.payload, Bytes::from_static(b"return-to=/settings"));
    assert!(
        auth.consume_token_with_payload(token.as_str(), "password-reset")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        auth.create_token_with_payload(
            "user-7",
            "password-reset",
            Duration::from_secs(900),
            Bytes::from(vec![0; 4097]),
        )
        .await
        .unwrap_err()
        .code(),
        "LIMIT"
    );
}

#[tokio::test]
async fn expired_tokens_are_absent_and_swept() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let a = forge.auth();

    let token = a
        .create_token("u", "magic-link", Duration::from_millis(100))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        a.consume_token(token.as_str(), "magic-link")
            .await
            .unwrap()
            .is_none(),
        "past the expiry"
    );
    // The expired row lingers until maintenance reclaims it.
    forge.maintain().await.unwrap();
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

#[tokio::test]
async fn api_key_verification_returns_expiry_scopes_and_metadata() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let auth = forge.auth();
    let opts = ApiKeyOpts::new()
        .with_expires_in(Duration::from_secs(60))
        .with_scopes(vec!["deploy".to_string(), "artifacts:read".to_string()])
        .with_metadata(HashMap::from([(
            "environment".to_string(),
            "test".to_string(),
        )]));
    let key = auth
        .create_api_key_with("owner-1", "ci-token", opts)
        .await
        .unwrap();
    assert!(key.expires_at.is_some());
    let info = auth
        .verify_api_key(key.secret.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.scopes, vec!["deploy", "artifacts:read"]);
    assert_eq!(
        info.metadata.get("environment").map(String::as_str),
        Some("test")
    );
    assert_eq!(info.expires_at, key.expires_at);
}
