#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use async_trait::async_trait;
use bytes::Bytes;
use forgelib::testing::TestDatabase;
use forgelib::{
    BackendLifecycle, Cursor, DequeueOpts, EnqueueOpts, Forge, Kv, Limit, Primitive, PutOpts,
    Result, ScheduleOpts, SessionOpts, SetMode, SetOpts,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

fn b(s: &str) -> Bytes {
    Bytes::from(s.as_bytes().to_vec())
}

/// A tiny in-process kv used only to exercise the injection path. It implements the `Kv`
/// operation trait and `BackendLifecycle`, so the blanket impl makes it a `KvBackend` that
/// `Forge::builder().kv(..)` accepts. `maintained` counts lifecycle sweeps so the test can
/// prove `maintain()` drives an injected backend.
#[derive(Default)]
struct InjectedKv {
    store: Mutex<HashMap<String, Bytes>>,
    maintained: AtomicUsize,
}

#[async_trait]
impl Kv for InjectedKv {
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        Ok(self.store.lock().unwrap().get(key).cloned())
    }
    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>> {
        let g = self.store.lock().unwrap();
        Ok(keys.iter().map(|k| g.get(*k).cloned()).collect())
    }
    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool> {
        let mut g = self.store.lock().unwrap();
        let present = g.contains_key(key);
        let write = match opts.mode {
            SetMode::Always => true,
            SetMode::IfNotExists => !present,
            SetMode::IfExists => present,
            _ => false,
        };
        if write {
            g.insert(key.to_string(), value);
        }
        Ok(write)
    }
    async fn delete(&self, key: &str) -> Result<bool> {
        Ok(self.store.lock().unwrap().remove(key).is_some())
    }
    async fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.store.lock().unwrap().contains_key(key))
    }
    async fn incr(&self, key: &str, by: i64) -> Result<i64> {
        let mut g = self.store.lock().unwrap();
        let cur = g
            .get(key)
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let next = cur.saturating_add(by);
        g.insert(key.to_string(), Bytes::from(next.to_string()));
        Ok(next)
    }
    async fn expire(&self, key: &str, _ttl: Duration) -> Result<bool> {
        Ok(self.store.lock().unwrap().contains_key(key))
    }
    async fn compare_and_swap(&self, key: &str, old: Option<Bytes>, new: Bytes) -> Result<bool> {
        let mut g = self.store.lock().unwrap();
        if g.get(key).cloned() == old {
            g.insert(key.to_string(), new);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    async fn scan(
        &self,
        prefix: &str,
        _cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)> {
        let g = self.store.lock().unwrap();
        let keys = g
            .keys()
            .filter(|k| k.starts_with(prefix))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok((keys, None))
    }
}

#[async_trait]
impl BackendLifecycle for InjectedKv {
    fn name(&self) -> &'static str {
        "injected-test"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Kv
    }
    fn durable(&self) -> bool {
        false
    }
    async fn maintain(&self) -> Result<()> {
        self.maintained.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Builder injects a kv backend while the other seven primitives stay on Postgres.
#[tokio::test]
async fn builder_injects_kv_while_the_rest_stay_postgres() {
    let db = TestDatabase::new().await.unwrap();

    let injected = Arc::new(InjectedKv::default());
    let probe = injected.clone();

    let forge = Forge::builder()
        .config_str(&db.config_toml(""))
        .unwrap()
        .kv(injected)
        .build()
        .await
        .expect("builder must construct a Forge with an injected kv and Postgres elsewhere");

    // A value set through the facade lands in the injected backend's own store, proving
    // forge.kv() routes to the injected instance and not a Postgres one.
    assert!(forge.kv().set("k", b("v"), SetOpts::new()).await.unwrap());
    assert_eq!(forge.kv().get("k").await.unwrap(), Some(b("v")));
    assert_eq!(probe.store.lock().unwrap().get("k").cloned(), Some(b("v")));

    // The report names the injected backend for kv; everything else is Postgres + durable.
    let report = forge.backend_capabilities();
    let kv = report
        .backends
        .iter()
        .find(|x| x.primitive == Primitive::Kv)
        .expect("kv line present in the report");
    assert_eq!(kv.provider, "injected-test");
    assert!(!kv.durable, "the injected in-process kv is not durable");
    for x in &report.backends {
        if x.primitive != Primitive::Kv {
            assert_eq!(
                x.provider, "postgres",
                "{:?} should remain postgres",
                x.primitive
            );
            assert_eq!(
                x.durable,
                x.primitive != Primitive::Pubsub,
                "{:?}: only pubsub (LISTEN/NOTIFY stores nothing) is non-durable on Postgres",
                x.primitive
            );
        }
    }

    // maintain() drives every backend's lifecycle, the injected one included.
    forge
        .maintain()
        .await
        .expect("maintain must drive every backend, injected included");
    assert!(
        probe.maintained.load(Ordering::SeqCst) >= 1,
        "the injected kv's lifecycle must have been maintained"
    );
}

/// The default path is unchanged by the builder refactor: `Forge::init` with a plain config
/// still produces an all-Postgres Forge.
#[tokio::test]
async fn init_default_config_is_all_postgres() {
    let db = TestDatabase::new().await.unwrap();

    let forge = db.forge().await.expect("default init must succeed");

    let report = forge.backend_capabilities();
    for x in &report.backends {
        assert_eq!(
            x.provider, "postgres",
            "{:?} must be postgres on the default path",
            x.primitive
        );
        assert_eq!(
            x.durable,
            x.primitive != Primitive::Pubsub,
            "{:?}: everything but pubsub (LISTEN/NOTIFY stores nothing) is durable on the default path",
            x.primitive
        );
    }

    // The default kv is the Postgres-backed one and works end to end.
    assert!(forge.kv().set("k", b("v"), SetOpts::new()).await.unwrap());
    assert_eq!(forge.kv().get("k").await.unwrap(), Some(b("v")));
}

/// Every primitive runs in the explicit, database-free memory profile.
#[tokio::test]
async fn all_memory_backends_init_and_operate_in_process() {
    let forge = Forge::init_from_str("[forge]\nmode = \"memory\"\nenvironment = \"test\"\n")
        .await
        .expect("memory init must not need a database");

    assert!(forge.kv().set("k", b("v"), SetOpts::new()).await.unwrap());
    assert_eq!(forge.kv().get("k").await.unwrap(), Some(b("v")));

    let id = forge
        .queue()
        .enqueue("jobs", b("payload"), EnqueueOpts::new())
        .await
        .unwrap();
    let deq = DequeueOpts::new()
        .with_wait(Duration::ZERO)
        .with_visibility_timeout(Duration::from_secs(60));
    let job = forge
        .queue()
        .dequeue("jobs", deq)
        .await
        .unwrap()
        .expect("the enqueued job is delivered");
    assert_eq!(job.id, id);
    forge.queue().ack(&job).await.unwrap();

    // ratelimit: a check inside a fresh budget is allowed.
    let decision = forge
        .ratelimit()
        .check(
            "api",
            "user-1",
            Limit::per_duration(5, Duration::from_secs(60)),
        )
        .await
        .unwrap();
    assert!(decision.allowed, "first call in a fresh bucket is allowed");

    forge.config().set_raw("retries", "3").await.unwrap();
    assert_eq!(
        forge.config().get_raw("retries").await.unwrap(),
        Some("3".to_string())
    );

    let token = forge
        .auth()
        .create_session("user-1", SessionOpts::new())
        .await
        .unwrap();
    let session = forge
        .auth()
        .validate_session(token.as_str())
        .await
        .unwrap()
        .expect("the session validates");
    assert_eq!(session.user_id, "user-1");

    // pubsub: subscribe, publish, receive (subscribe first: only later messages deliver).
    let mut sub = forge.pubsub().subscribe("events").await.unwrap();
    forge.pubsub().publish("events", b("ping")).await.unwrap();
    let msg = sub
        .next()
        .await
        .expect("a message is delivered")
        .expect("delivery is not an error");
    assert_eq!(msg, b("ping"));

    // schedule: register a one-shot already due, then fire it via process_due.
    forge
        .schedule()
        .at(
            SystemTime::now() - Duration::from_secs(5),
            "jobs",
            b("scheduled"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        forge.run_scheduler_once().await.unwrap(),
        1,
        "the due one-shot fires once"
    );

    forge
        .blob()
        .put("exports/data.csv", b("a,b,c"), PutOpts::new())
        .await
        .unwrap();
    assert_eq!(
        forge.blob().get("exports/data.csv").await.unwrap(),
        Some(b("a,b,c"))
    );

    // The report names every one of the eight providers `memory` and non-durable.
    let report = forge.backend_capabilities();
    assert_eq!(report.backends.len(), 8, "one report line per primitive");
    for x in &report.backends {
        assert_eq!(
            x.provider, "memory",
            "{:?} must report the memory provider",
            x.primitive
        );
        assert!(
            !x.durable,
            "{:?} memory backend is not durable",
            x.primitive
        );
    }
}
