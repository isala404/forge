#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{EvalCtx, FlagRule};
use std::time::Duration;

#[tokio::test]
async fn committed_config_writes_invalidate_other_process_caches() {
    let db = TestDatabase::new().await.unwrap();
    let writer = db.forge().await.unwrap();
    let reader = db.forge().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    writer.config().set_raw("mode", "old").await.unwrap();
    assert_eq!(
        reader.config().get_raw("mode").await.unwrap().as_deref(),
        Some("old")
    );
    writer.config().set_raw("mode", "new").await.unwrap();

    let refreshed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if reader.config().get_raw("mode").await.unwrap().as_deref() == Some("new") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        refreshed.is_ok(),
        "notification should evict the stale cache"
    );
}

#[tokio::test]
async fn committed_flag_writes_invalidate_typed_evaluations() {
    let db = TestDatabase::new().await.unwrap();
    let writer = db.forge().await.unwrap();
    let reader = db.forge().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    writer
        .config()
        .set_flag("theme", FlagRule::Off)
        .await
        .unwrap();
    assert!(!reader.config().flag("theme", false, &EvalCtx::new()).await);
    writer
        .config()
        .set_flag(
            "theme",
            FlagRule::Value {
                value: serde_json::json!({"palette": "dark"}),
                variant: "dark-v1".to_string(),
            },
        )
        .await
        .unwrap();

    let refreshed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = reader
                .config()
                .flag_details("theme", &serde_json::json!({}), &EvalCtx::new())
                .await;
            if result.variant.as_deref() == Some("dark-v1") {
                assert_eq!(result.reason, "static");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        refreshed.is_ok(),
        "notification should evict the stale flag cache"
    );
}
