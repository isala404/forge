#![cfg(feature = "pg-tests")]
// Dynamic table-name counts run against throwaway DBs, so the compile-time query macros
// don't apply; same exception the test harness takes.
#![allow(clippy::unwrap_used, clippy::panic, clippy::disallowed_methods)]

use bytes::Bytes;
use forgelib::Forge;
use forgelib::testing::TestDatabase;
use sqlx::{Connection, PgConnection};

async fn count(url: &str, table: &str) -> i64 {
    let mut conn = PgConnection::connect(url).await.unwrap();
    let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(&mut conn)
        .await
        .unwrap();
    conn.close().await.ok();
    n
}

/// kv on the default database, queue on a separate one; each write lands only in its own.
#[tokio::test]
async fn feature_override_routes_writes_to_its_own_database() {
    let default_db = TestDatabase::new().await.unwrap();
    let queue_db = TestDatabase::new().await.unwrap();

    let forge = Forge::init_from_str(&format!(
        "[postgres]\nurl = \"{}\"\nmax_connections = 4\n\
         [databases.queue]\nurl = \"{}\"\nmax_connections = 2\n",
        default_db.url(),
        queue_db.url(),
    ))
    .await
    .unwrap();

    forge
        .kv()
        .set("k", Bytes::from_static(b"v"), Default::default())
        .await
        .unwrap();
    forge
        .queue()
        .enqueue("emails", Bytes::from_static(b"job"), Default::default())
        .await
        .unwrap();

    assert_eq!(count(default_db.url(), "forge_kv").await, 1);
    assert_eq!(count(default_db.url(), "forge_jobs").await, 0);
    assert_eq!(count(queue_db.url(), "forge_jobs").await, 1);
    assert_eq!(count(queue_db.url(), "forge_kv").await, 0);

    // Both features still read back through their own pools.
    assert_eq!(
        forge.kv().get("k").await.unwrap().as_deref(),
        Some(&b"v"[..])
    );
    assert_eq!(forge.queue().depth("emails").await.unwrap().visible, 1);
}

/// An override pointed at the same server as the default still gets its own pool.
#[tokio::test]
async fn same_server_override_uses_a_separate_pool() {
    let db = TestDatabase::new().await.unwrap();

    let forge = db
        .forge_with(&format!(
            "[databases.kv]\nurl = \"{}\"\nmax_connections = 2\n",
            db.url()
        ))
        .await
        .unwrap();

    forge
        .kv()
        .set("k", Bytes::from_static(b"v"), Default::default())
        .await
        .unwrap();
    forge
        .queue()
        .enqueue("q", Bytes::from_static(b"j"), Default::default())
        .await
        .unwrap();

    // Migration ran once for the shared server; both pools see the same database.
    assert_eq!(count(db.url(), "forge_kv").await, 1);
    assert_eq!(count(db.url(), "forge_jobs").await, 1);
}
