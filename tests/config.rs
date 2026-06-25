//! config contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
//!
//! Env-precedence (`FORGE_CFG_<KEY>` over the store) is not exercised here: setting a
//! process-global env var needs `unsafe` under edition 2024, which the package's
//! `unsafe_code = "forbid"` lint disallows. The resolution is a single env read ahead of
//! the store lookup in `get_raw`.
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forge::testing::TestDatabase;
use forge::{ConfigExt, EvalCtx, FlagRule, ForgeError};

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
    assert!(matches!(
        c.get::<i32>("not_a_number").await,
        Err(ForgeError::Invalid(_))
    ));
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

    assert!(matches!(
        c.set_flag("x", FlagRule::Percent(150)).await,
        Err(ForgeError::Invalid(_))
    ));
    let big = "x".repeat(64 * 1024 + 1);
    assert!(matches!(
        c.set_raw("k", &big).await,
        Err(ForgeError::Limit(_))
    ));
    assert!(matches!(
        c.set_raw("", "v").await,
        Err(ForgeError::Invalid(_))
    ));
}
