#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{
    ConfigExt, ConfigSnapshot, EvalCtx, FlagEvaluationRequest, FlagRule, ForgeError,
    SnapshotSecretHandling,
};
use std::time::Duration;

fn assert_code<T>(result: Result<T, ForgeError>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), expected),
        Ok(_) => panic!("expected {expected} error"),
    }
}

#[tokio::test]
async fn get_set_raw_roundtrip_is_last_write_wins() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();

    assert_eq!(c.get_raw("missing").await.unwrap(), None);
    c.set_raw("greeting", "hello").await.unwrap();
    assert_eq!(c.get_raw("greeting").await.unwrap(), Some("hello".into()));
    c.set_raw("greeting", "hi").await.unwrap();
    assert_eq!(c.get_raw("greeting").await.unwrap(), Some("hi".into()));
}

#[tokio::test]
async fn delete_raw_removes_value_and_reports_whether_present() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();

    c.set_raw("k", "v").await.unwrap();
    assert!(c.delete_raw("k").await.unwrap(), "removed an existing key");
    assert_eq!(c.get_raw("k").await.unwrap(), None);
    assert!(
        !c.delete_raw("k").await.unwrap(),
        "deleting an absent key is false"
    );
}

#[tokio::test]
async fn delete_flag_reverts_to_default() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();
    let ctx = EvalCtx::new();

    c.set_flag("f", FlagRule::On).await.unwrap();
    assert!(c.flag("f", false, &ctx).await, "flag is on");
    assert!(c.delete_flag("f").await.unwrap(), "removed the rule");
    assert!(
        !c.flag("f", false, &ctx).await,
        "flag reverts to the caller default once deleted"
    );
    assert!(!c.delete_flag("f").await.unwrap(), "absent flag is false");
}

#[tokio::test]
async fn typed_get_parses_json_and_flags_bad_values() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();

    c.set_raw("max_items", "42").await.unwrap();
    assert_eq!(c.get::<i32>("max_items").await.unwrap(), Some(42));
    assert_eq!(c.get::<i32>("absent").await.unwrap(), None);

    c.set_raw("not_a_number", "abc").await.unwrap();
    assert_code(c.get::<i32>("not_a_number").await, "INVALID");
}

#[tokio::test]
async fn flags_on_off_and_missing_resolves_to_default() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();
    let ctx = EvalCtx::user("u1");

    assert!(c.flag("ff", true, &ctx).await);
    assert!(!c.flag("ff", false, &ctx).await);

    c.set_flag("ff", FlagRule::On).await.unwrap();
    assert!(c.flag("ff", false, &ctx).await);
    c.set_flag("ff", FlagRule::Off).await.unwrap();
    assert!(!c.flag("ff", true, &ctx).await);
}

#[tokio::test]
async fn percent_rollout_endpoints_are_definite() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();
    let ctx = EvalCtx::user("alice");

    c.set_flag("rollout", FlagRule::Percent(0)).await.unwrap();
    assert!(
        !c.flag("rollout", true, &ctx).await,
        "Percent(0) is always out"
    );
    c.set_flag("rollout", FlagRule::Percent(100)).await.unwrap();
    assert!(
        c.flag("rollout", false, &ctx).await,
        "Percent(100) is always in"
    );
    // Percent with no targeting key falls back to the caller default.
    c.set_flag("rollout", FlagRule::Percent(50)).await.unwrap();
    assert!(c.flag("rollout", true, &EvalCtx::new()).await);
    assert!(!c.flag("rollout", false, &EvalCtx::new()).await);
}

#[tokio::test]
async fn allowlist_targets_by_key() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();

    c.set_flag(
        "beta",
        FlagRule::AllowList(vec!["alice".into(), "bob".into()]),
    )
    .await
    .unwrap();
    assert!(c.flag("beta", false, &EvalCtx::user("alice")).await);
    assert!(!c.flag("beta", false, &EvalCtx::user("carol")).await);
    // No targeting key cannot be in any list.
    assert!(!c.flag("beta", true, &EvalCtx::new()).await);
}

#[tokio::test]
async fn set_flag_and_set_raw_enforce_limits() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let c = forge.config();

    assert_code(c.set_flag("x", FlagRule::Percent(150)).await, "INVALID");
    let big = "x".repeat(64 * 1024 + 1);
    assert_code(c.set_raw("k", &big).await, "LIMIT");
    assert_code(c.set_raw("", "v").await, "INVALID");
}

#[tokio::test]
async fn bulk_reads_and_bounded_snapshot_preserve_order_and_details() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let config = forge.config();
    config.set_raw("region", "eu-west").await.unwrap();
    config.set_raw("retries", "3").await.unwrap();
    config
        .set_flag(
            "theme",
            FlagRule::Value {
                value: serde_json::json!("blue"),
                variant: "blue-v2".into(),
            },
        )
        .await
        .unwrap();

    let keys = vec!["retries".into(), "missing".into(), "region".into()];
    let values = config.get_many_raw(&keys).await.unwrap();
    assert_eq!(
        values
            .iter()
            .map(|entry| entry.value.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("3"), None, Some("eu-west")]
    );

    let requests = vec![FlagEvaluationRequest {
        id: "header-theme".into(),
        key: "theme".into(),
        default: serde_json::json!("gray"),
        context: EvalCtx::user("user-1").with_field("region", serde_json::json!("eu")),
    }];
    let evaluations = config.flag_details_many(&requests).await.unwrap();
    let evaluation = evaluations.first().unwrap();
    assert_eq!(evaluation.evaluation.value_json, r#""blue""#);
    assert_eq!(evaluation.evaluation.variant.as_deref(), Some("blue-v2"));

    let snapshot = config
        .snapshot(
            &keys,
            &requests,
            Duration::from_secs(60),
            SnapshotSecretHandling::NoSecrets,
        )
        .await
        .unwrap();
    let decoded = ConfigSnapshot::decode(&snapshot.encode().unwrap()).unwrap();
    assert_eq!(
        decoded
            .get_raw("region", decoded.created_at_ms)
            .unwrap()
            .as_deref(),
        Some("eu-west")
    );
    assert_eq!(
        decoded
            .flag_details("header-theme", decoded.created_at_ms)
            .unwrap()
            .variant
            .as_deref(),
        Some("blue-v2")
    );
    assert_code(
        decoded.ensure_fresh(decoded.expires_at_ms + 1),
        "PRECONDITION",
    );
}
