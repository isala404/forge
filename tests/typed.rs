//! Typed-layer contract tests: the strongly-typed surface round-trips through the same
//! backends as the stringly-typed primitives. Run with `cargo test --features pg-tests`.
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forge::testing::TestDatabase;
use forge::{
    BlobKey, ConfigKey, ConfigTyped, DequeueOpts, Forge, ForgeConfig, KvKey, KvTyped, PubsubTyped,
    QueueName, QueuePayload, QueueTyped, RateBucket, SetOpts, Topic,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct SendEmail {
    to: String,
    template: String,
}
impl QueuePayload for SendEmail {
    const QUEUE: QueueName<Self> = QueueName::new("emails");
    const MAX_ATTEMPTS: u32 = 3;
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Profile {
    name: String,
    age: u32,
}

#[tokio::test]
async fn kv_typed_roundtrips() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let key: KvKey<Profile> = KvKey::new("user:1:profile");

    assert!(forge.kv().get_typed(&key).await.unwrap().is_none());
    let p = Profile {
        name: "Ada".into(),
        age: 36,
    };
    assert!(
        forge
            .kv()
            .set_typed(&key, &p, SetOpts::new())
            .await
            .unwrap()
    );
    assert_eq!(forge.kv().get_typed(&key).await.unwrap().unwrap(), p);
    assert!(forge.kv().delete_typed(&key).await.unwrap());
    assert!(forge.kv().get_typed(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn config_typed_reads_default_and_set() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let key: ConfigKey<u64> = ConfigKey::new("max_upload_bytes", 1024);

    assert_eq!(forge.config().get_or_default(&key).await.unwrap(), 1024);
    assert!(forge.config().get_typed(&key).await.unwrap().is_none());
    forge.config().set_typed(&key, &5_000_000).await.unwrap();
    assert_eq!(
        forge.config().get_typed(&key).await.unwrap(),
        Some(5_000_000)
    );
    assert_eq!(
        forge.config().get_or_default(&key).await.unwrap(),
        5_000_000
    );
}

#[tokio::test]
async fn queue_typed_enqueue_dequeue() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let id = forge
        .queue()
        .enqueue_typed(&SendEmail {
            to: "a@b.c".into(),
            template: "welcome".into(),
        })
        .await
        .unwrap();

    let job = forge
        .queue()
        .dequeue_typed::<SendEmail>(DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.job().id, id);
    assert_eq!(job.payload.template, "welcome");
    // The lease is acked through the raw job.
    forge.queue().ack(job.job()).await.unwrap();
}

#[tokio::test]
async fn ratelimit_typed_bucket_enforces_policy() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    const LOGIN: RateBucket<str> = RateBucket::new(
        "login",
        forge::Limit::per_duration(2, Duration::from_secs(60)),
        forge::FailMode::Closed,
    );

    assert!(
        LOGIN
            .check(forge.ratelimit(), "user-1")
            .await
            .unwrap()
            .allowed
    );
    assert!(
        LOGIN
            .check(forge.ratelimit(), "user-1")
            .await
            .unwrap()
            .allowed
    );
    assert!(
        !LOGIN
            .check(forge.ratelimit(), "user-1")
            .await
            .unwrap()
            .allowed
    );
    // A different subject has its own budget.
    assert!(
        LOGIN
            .check(forge.ratelimit(), "user-2")
            .await
            .unwrap()
            .allowed
    );
}

#[tokio::test]
async fn blob_typed_key_roundtrips() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    struct Avatar;
    let key: BlobKey<Avatar> = BlobKey::new("avatars/u1.png");

    key.put(
        forge.blob(),
        forge::Bytes::from_static(b"png-bytes"),
        forge::PutOpts::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        key.get(forge.blob()).await.unwrap().unwrap(),
        forge::Bytes::from_static(b"png-bytes")
    );
    assert!(key.head(forge.blob()).await.unwrap().is_some());
    assert!(key.delete(forge.blob()).await.unwrap());
}

#[tokio::test]
async fn pubsub_typed_publishes_and_receives() {
    let db = TestDatabase::new().await.unwrap();
    let forge = Forge::init(ForgeConfig::new(db.url())).await.unwrap();
    let topic: Topic<Profile> = Topic::new("profiles.updated");

    let mut sub = forge.pubsub().subscribe_typed(&topic).await.unwrap();
    forge
        .pubsub()
        .publish_typed(
            &topic,
            &Profile {
                name: "Lin".into(),
                age: 28,
            },
        )
        .await
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("event within timeout")
        .expect("stream item")
        .expect("decoded event");
    assert_eq!(got.name, "Lin");
}
