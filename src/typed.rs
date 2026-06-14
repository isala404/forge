//! The typed layer: a thin, strongly-typed surface over the stringly-typed primitive
//! traits, so generated app code binds a *name + codec + defaults* to a Rust type
//! instead of inventing key strings and JSON conventions per app.
//!
//! Each typed handle wraps the low-level operation. `KvKey<T>` ties a key to a
//! JSON-serializable value type; `QueueName<T>` / [`QueuePayload`] tie a queue to its
//! payload and defaults; `ConfigKey<T>` carries the key, value type, and a default;
//! `RateBucket<S>` binds a bucket to its policy, fail mode, and subject type;
//! `BlobKey<K>` and `Topic<E>` do the same for blob paths and pubsub events. The
//! low-level traits stay the contract; this is the surface agents should prefer.
//!
//! The same definitions are the source of truth the Node and Python typed handles
//! mirror, so a `SendEmail` job is expressed once per language from one shape rather
//! than redeclared three times.

use crate::blob::{Blob, BlobInfo, ListPage, PutOpts};
use crate::config_store::ConfigStore;
use crate::error::{ForgeError, Result};
use crate::kv::{Kv, SetOpts};
use crate::pubsub::Pubsub;
use crate::queue::{DequeueOpts, EnqueueOpts, Job, JobId, Queue, QueueDepth};
use crate::ratelimit::{Decision, FailMode, Limit, RateLimit};
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::{BoxStream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;

fn ser<T: Serialize>(value: &T, what: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(value)
        .map_err(|e| ForgeError::invalid(format!("could not serialize {what}: {e}")))
}

fn de_slice<T: DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|e| ForgeError::invalid(format!("could not deserialize {what}: {e}")))
}

// ---------------------------------------------------------------------------------
// kv
// ---------------------------------------------------------------------------------

/// A kv key bound to a JSON value type `T`. Construct one constant per logical key so
/// the key string and value type are declared together, once.
#[derive(Debug, Clone)]
pub struct KvKey<T> {
    key: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T> KvKey<T> {
    /// Bind `key` to value type `T`.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            _marker: PhantomData,
        }
    }

    /// The underlying key string.
    pub fn as_str(&self) -> &str {
        &self.key
    }
}

/// Typed JSON access over [`Kv`], keyed by [`KvKey`]. Blanket-implemented, so it works
/// on `&dyn Kv` (e.g. `forge.kv()`).
#[async_trait]
pub trait KvTyped: Kv {
    /// `get` and deserialize the value bound to `key`. `None` if absent/expired.
    async fn get_typed<T: DeserializeOwned + Send>(&self, key: &KvKey<T>) -> Result<Option<T>> {
        match self.get(key.as_str()).await? {
            Some(b) => de_slice(&b, "kv value").map(Some),
            None => Ok(None),
        }
    }

    /// Serialize `value` and `set` it at `key`. Returns whether the write happened.
    async fn set_typed<T: Serialize + Send + Sync>(
        &self,
        key: &KvKey<T>,
        value: &T,
        opts: SetOpts,
    ) -> Result<bool> {
        self.set(key.as_str(), Bytes::from(ser(value, "kv value")?), opts)
            .await
    }

    /// `delete` the value at `key`.
    async fn delete_typed<T: Send + Sync>(&self, key: &KvKey<T>) -> Result<bool> {
        self.delete(key.as_str()).await
    }
}

impl<K: Kv + ?Sized> KvTyped for K {}

// ---------------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------------

/// A config key bound to a JSON value type `T`, carrying a default for when it is unset
/// at every layer.
#[derive(Debug, Clone)]
pub struct ConfigKey<T> {
    key: String,
    default: T,
}

impl<T> ConfigKey<T> {
    /// Bind `key` to value type `T` with `default`.
    pub fn new(key: impl Into<String>, default: T) -> Self {
        Self {
            key: key.into(),
            default,
        }
    }

    /// The underlying key string.
    pub fn as_str(&self) -> &str {
        &self.key
    }

    /// The configured default.
    pub fn default_value(&self) -> &T {
        &self.default
    }
}

/// Typed config access over [`ConfigStore`], keyed by [`ConfigKey`]. Blanket-implemented.
#[async_trait]
pub trait ConfigTyped: ConfigStore {
    /// Resolve `key` and deserialize into `T`. `None` if unset at every layer.
    async fn get_typed<T: DeserializeOwned + Send + Sync>(
        &self,
        key: &ConfigKey<T>,
    ) -> Result<Option<T>> {
        match self.get_raw(key.as_str()).await? {
            Some(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| {
                ForgeError::invalid(format!("could not deserialize config value: {e}"))
            }),
            None => Ok(None),
        }
    }

    /// Like [`ConfigTyped::get_typed`] but falls back to the key's default when unset.
    async fn get_or_default<T: DeserializeOwned + Clone + Send + Sync>(
        &self,
        key: &ConfigKey<T>,
    ) -> Result<T> {
        Ok(self
            .get_typed(key)
            .await?
            .unwrap_or_else(|| key.default_value().clone()))
    }

    /// Serialize `value` to JSON and store it at `key` (an env var still shadows it).
    async fn set_typed<T: Serialize + Send + Sync>(
        &self,
        key: &ConfigKey<T>,
        value: &T,
    ) -> Result<()> {
        let raw = serde_json::to_string(value)
            .map_err(|e| ForgeError::invalid(format!("could not serialize config value: {e}")))?;
        self.set_raw(key.as_str(), &raw).await
    }
}

impl<C: ConfigStore + ?Sized> ConfigTyped for C {}

// ---------------------------------------------------------------------------------
// queue
// ---------------------------------------------------------------------------------

/// A queue name bound to its payload type `T`. `const`-constructible so it can live in
/// a [`QueuePayload`] associated constant.
#[derive(Debug, Clone, Copy)]
pub struct QueueName<T> {
    name: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T> QueueName<T> {
    /// Bind `name` to payload type `T`.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }

    /// The underlying queue name.
    pub const fn as_str(&self) -> &'static str {
        self.name
    }
}

/// A payload type that knows its own queue and defaults. Implement it once per job
/// type; the typed enqueue/dequeue then need no queue string and no manual JSON.
///
/// ```
/// use forge::typed::{QueueName, QueuePayload};
/// use serde::{Serialize, Deserialize};
/// #[derive(Serialize, Deserialize)]
/// struct SendEmail { to: String, template: String }
/// impl QueuePayload for SendEmail {
///     const QUEUE: QueueName<Self> = QueueName::new("emails");
///     const MAX_ATTEMPTS: u32 = 3;
/// }
/// ```
pub trait QueuePayload: Serialize + DeserializeOwned + Send + Sync + Sized {
    /// The queue this payload is enqueued into / dequeued from.
    const QUEUE: QueueName<Self>;
    /// Default delivery attempts before dead-lettering (used by `enqueue_typed`).
    const MAX_ATTEMPTS: u32 = 5;

    /// The default enqueue options for this payload type (applies `MAX_ATTEMPTS`).
    fn enqueue_opts() -> EnqueueOpts {
        EnqueueOpts::new().with_max_attempts(Self::MAX_ATTEMPTS)
    }
}

/// A dequeued job whose payload has been decoded into `P`. Carries the raw [`Job`] so
/// the lease can be `ack`/`nack`/`heartbeat`'d.
#[derive(Debug, Clone)]
pub struct TypedJob<P> {
    /// The decoded payload.
    pub payload: P,
    job: Job,
}

impl<P> TypedJob<P> {
    /// The underlying job (for `ack`/`nack`/`heartbeat` and its id/attempt).
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Split into the decoded payload and the raw job.
    pub fn into_parts(self) -> (P, Job) {
        (self.payload, self.job)
    }
}

/// Typed enqueue/dequeue over [`Queue`], driven by [`QueuePayload`]. Blanket-implemented.
#[async_trait]
pub trait QueueTyped: Queue {
    /// Enqueue `payload` onto its bound queue with the type's default options.
    async fn enqueue_typed<P: QueuePayload>(&self, payload: &P) -> Result<JobId> {
        self.enqueue_typed_with(payload, P::enqueue_opts()).await
    }

    /// Enqueue `payload` with caller-chosen options (queue + codec still bound to `P`).
    async fn enqueue_typed_with<P: QueuePayload>(
        &self,
        payload: &P,
        opts: EnqueueOpts,
    ) -> Result<JobId> {
        self.enqueue(
            P::QUEUE.as_str(),
            Bytes::from(ser(payload, "queue payload")?),
            opts,
        )
        .await
    }

    /// Dequeue from `P`'s bound queue and decode the payload into `P`.
    async fn dequeue_typed<P: QueuePayload>(
        &self,
        opts: DequeueOpts,
    ) -> Result<Option<TypedJob<P>>> {
        match self.dequeue(P::QUEUE.as_str(), opts).await? {
            Some(job) => {
                let payload = de_slice::<P>(&job.payload, "queue payload")?;
                Ok(Some(TypedJob { payload, job }))
            }
            None => Ok(None),
        }
    }

    /// Approximate depth of `P`'s bound queue.
    async fn depth_typed<P: QueuePayload>(&self) -> Result<QueueDepth> {
        self.depth(P::QUEUE.as_str()).await
    }
}

impl<Q: Queue + ?Sized> QueueTyped for Q {}

// ---------------------------------------------------------------------------------
// ratelimit
// ---------------------------------------------------------------------------------

/// A value that names a rate-limit subject (the per-caller key). Blanket-implemented
/// for any `Display` type, so a `UserId` newtype works as a subject out of the box.
pub trait RateSubject {
    /// The subject string consumed by [`RateLimit::check`].
    fn rate_subject(&self) -> String;
}

impl<T: std::fmt::Display + ?Sized> RateSubject for T {
    fn rate_subject(&self) -> String {
        self.to_string()
    }
}

/// A rate-limit bucket bound to its policy, fail mode, and subject type `S`. Declaring
/// one constant per bucket keeps the bucket name, the limit, and the fail-open/closed
/// decision in one place instead of scattered across call sites. `S` may be unsized
/// (e.g. `RateBucket<str>`). `PhantomData<fn(&S)>` keeps it `Send`/`Sync` so a bucket
/// can be a `const`/`static`.
pub struct RateBucket<S: ?Sized> {
    bucket: &'static str,
    limit: Limit,
    fail: FailMode,
    _marker: PhantomData<fn(&S)>,
}

impl<S: RateSubject + ?Sized> RateBucket<S> {
    /// Bind `bucket` to its `limit` policy and `fail` mode for subject type `S`.
    pub const fn new(bucket: &'static str, limit: Limit, fail: FailMode) -> Self {
        Self {
            bucket,
            limit,
            fail,
            _marker: PhantomData,
        }
    }

    /// Check-and-consume one unit for `subject` against this bucket's bound policy.
    pub async fn check(&self, rl: &dyn RateLimit, subject: &S) -> Result<Decision> {
        rl.check_with(self.bucket, &subject.rate_subject(), self.limit, self.fail)
            .await
    }
}

// ---------------------------------------------------------------------------------
// blob
// ---------------------------------------------------------------------------------

/// A blob key bound to a "kind" marker `K`, so a path convention is named once and the
/// type system keeps avatars and exports from being mixed up at call sites.
#[derive(Debug, Clone)]
pub struct BlobKey<K> {
    key: String,
    _marker: PhantomData<fn() -> K>,
}

impl<K> BlobKey<K> {
    /// Build a key of kind `K`.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            _marker: PhantomData,
        }
    }

    /// The underlying key string.
    pub fn as_str(&self) -> &str {
        &self.key
    }

    /// `put` bytes at this key.
    pub async fn put(&self, blob: &dyn Blob, data: Bytes, opts: PutOpts) -> Result<()> {
        blob.put(&self.key, data, opts).await
    }

    /// `get` the bytes at this key.
    pub async fn get(&self, blob: &dyn Blob) -> Result<Option<Bytes>> {
        blob.get(&self.key).await
    }

    /// `head` the object at this key.
    pub async fn head(&self, blob: &dyn Blob) -> Result<Option<BlobInfo>> {
        blob.head(&self.key).await
    }

    /// `delete` the object at this key.
    pub async fn delete(&self, blob: &dyn Blob) -> Result<bool> {
        blob.delete(&self.key).await
    }
}

/// List a `BlobKey` *kind* by prefix — a free function because `K` is a path family,
/// not one object. The `prefix` is a `BlobKey<K>` so the kind stays bound.
pub async fn list_kind<K>(
    blob: &dyn Blob,
    prefix: &BlobKey<K>,
    cursor: Option<Cursor>,
    limit: u32,
) -> Result<ListPage> {
    blob.list(prefix.as_str(), cursor, limit).await
}

// ---------------------------------------------------------------------------------
// pubsub
// ---------------------------------------------------------------------------------

/// A pubsub topic bound to an event type `E`.
#[derive(Debug, Clone)]
pub struct Topic<E> {
    topic: String,
    _marker: PhantomData<fn() -> E>,
}

impl<E> Topic<E> {
    /// Bind `topic` to event type `E`.
    pub fn new(topic: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            _marker: PhantomData,
        }
    }

    /// The underlying topic string.
    pub fn as_str(&self) -> &str {
        &self.topic
    }
}

/// A typed pubsub stream: each item is one event decoded into `E` (a decode failure is
/// surfaced as `Err` on the stream rather than dropping the message silently).
pub type TypedSubscription<E> = BoxStream<'static, Result<E>>;

/// Typed publish/subscribe over [`Pubsub`], keyed by [`Topic`]. Blanket-implemented.
#[async_trait]
pub trait PubsubTyped: Pubsub {
    /// Serialize `event` and publish it to `topic`.
    async fn publish_typed<E: Serialize + Send + Sync>(
        &self,
        topic: &Topic<E>,
        event: &E,
    ) -> Result<()> {
        self.publish(topic.as_str(), Bytes::from(ser(event, "event")?))
            .await
    }

    /// Subscribe to `topic`, decoding each payload into `E`.
    async fn subscribe_typed<E: DeserializeOwned + 'static>(
        &self,
        topic: &Topic<E>,
    ) -> Result<TypedSubscription<E>> {
        let sub = self.subscribe(topic.as_str()).await?;
        Ok(sub
            .map(|item| item.and_then(|bytes| de_slice::<E>(&bytes, "event")))
            .boxed())
    }
}

impl<P: Pubsub + ?Sized> PubsubTyped for P {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::time::Duration;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct SendEmail {
        to: String,
    }
    impl QueuePayload for SendEmail {
        const QUEUE: QueueName<Self> = QueueName::new("emails");
        const MAX_ATTEMPTS: u32 = 3;
    }

    #[test]
    fn queue_payload_binds_name_and_defaults() {
        assert_eq!(SendEmail::QUEUE.as_str(), "emails");
        assert_eq!(SendEmail::enqueue_opts().max_attempts, 3);
    }

    #[test]
    fn keys_carry_their_strings() {
        let k: KvKey<u64> = KvKey::new("counter:1");
        assert_eq!(k.as_str(), "counter:1");
        let c = ConfigKey::new("max_upload", 1024u64);
        assert_eq!(c.as_str(), "max_upload");
        assert_eq!(*c.default_value(), 1024);
        let t: Topic<SendEmail> = Topic::new("emails.events");
        assert_eq!(t.as_str(), "emails.events");
        let b: BlobKey<()> = BlobKey::new("avatars/u1.png");
        assert_eq!(b.as_str(), "avatars/u1.png");
    }

    #[test]
    fn rate_bucket_is_const_constructible() {
        const LOGIN: RateBucket<str> = RateBucket::new(
            "login",
            Limit {
                max: 5,
                per: Duration::from_secs(60),
                algo: crate::ratelimit::Algo::TokenBucket,
            },
            FailMode::Closed,
        );
        assert_eq!(LOGIN.bucket, "login");
    }
}
