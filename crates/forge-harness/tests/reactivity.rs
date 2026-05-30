//! Reactivity scenarios.
//!
//! A write to a reactive table must fan out to every SSE subscriber whose
//! query depends on that table — and must leave subscribers of unrelated
//! tables, or of unaffected argument sets, untouched. This is the cheap,
//! browserless proxy for "open the app in two tabs, change a row, watch both
//! update": same gateway, same reactor, same SSE wire format.

/// Sentinel test so `cargo test -p forge-harness` (without `--features
/// testcontainers`) doesn't silently report "0 tests passed" and lull a
/// contributor into thinking they ran the scenario suite. Always passes;
/// its job is to print the hint.
#[test]
fn ensure_testcontainers_feature_enabled() {
    eprintln!(
        "forge-harness reactivity scenarios are gated on `--features testcontainers`. \
         Re-run with `cargo test -p forge-harness --features testcontainers` \
         to exercise the reactor against a real Postgres."
    );
}

#[cfg(feature = "testcontainers")]
#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "testcontainers")]
use std::time::Duration;

#[cfg(feature = "testcontainers")]
use common::{BumpInput, Counter, collect_updates, start_app};

/// Generous budget for one reactor round-trip: NOTIFY -> invalidate (<=200ms
/// debounce) -> re-execute -> SSE push. Real latency is tens of ms; the slack
/// only absorbs CI scheduling noise.
#[cfg(feature = "testcontainers")]
const PUSH_BUDGET: Duration = Duration::from_secs(5);

/// Window to watch for a push that must NOT happen. Comfortably past the
/// 200ms max debounce, short enough to keep the suite quick.
#[cfg(feature = "testcontainers")]
const SILENCE_WINDOW: Duration = Duration::from_millis(1200);

/// A fresh subscription hands back the current rows synchronously, at
/// subscribe time — the equivalent of a SvelteKit store's first value.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn subscribe_returns_initial_snapshot() {
    let app = start_app("reactivity_initial_snapshot").await;

    let _: Counter = app
        .client()
        .call(
            "harness_bump_counter",
            BumpInput {
                name: "seeded".into(),
                by: 7,
            },
        )
        .await
        .expect("seed counter");

    let session = app.open_session(None).await.expect("open sse session");
    let snapshot = session
        .subscribe("counters", "harness_list_counters", serde_json::Value::Null)
        .await
        .expect("subscribe to counters");

    let counters: Vec<Counter> = serde_json::from_value(snapshot).expect("decode counter snapshot");
    assert_eq!(
        counters,
        vec![Counter {
            name: "seeded".into(),
            value: 7,
        }],
    );

    app.shutdown().await.expect("shutdown");
}

/// The core loop: subscribe, mutate over RPC, observe the reactor push the
/// fresh result down the SSE stream.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn single_client_sees_invalidation() {
    let app = start_app("reactivity_single_client").await;

    let session = app.open_session(None).await.expect("open sse session");
    let snapshot = session
        .subscribe("counters", "harness_list_counters", serde_json::Value::Null)
        .await
        .expect("subscribe to counters");
    let initial: Vec<Counter> = serde_json::from_value(snapshot).expect("decode initial snapshot");
    assert!(
        initial.is_empty(),
        "expected no counters before any mutation"
    );

    let _: Counter = app
        .client()
        .call(
            "harness_bump_counter",
            BumpInput {
                name: "alpha".into(),
                by: 3,
            },
        )
        .await
        .expect("bump alpha");

    let payload = session
        .next_query_update("counters", PUSH_BUDGET)
        .await
        .expect("reactor push after mutation");
    let pushed: Vec<Counter> = serde_json::from_value(payload).expect("decode pushed snapshot");
    assert_eq!(
        pushed,
        vec![Counter {
            name: "alpha".into(),
            value: 3,
        }],
    );

    app.shutdown().await.expect("shutdown");
}

/// Two independent SSE sessions subscribed to the same query collapse into a
/// single reactor group (dedup by query + args + auth scope), yet one mutation
/// still fans out to both. This is the "two browser tabs" scenario.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn two_clients_both_receive_invalidation() {
    let app = start_app("reactivity_two_clients").await;

    let alice = app.open_session(None).await.expect("open alice session");
    let bob = app.open_session(None).await.expect("open bob session");

    alice
        .subscribe("counters", "harness_list_counters", serde_json::Value::Null)
        .await
        .expect("alice subscribe");
    bob.subscribe("counters", "harness_list_counters", serde_json::Value::Null)
        .await
        .expect("bob subscribe");

    let _: Counter = app
        .client()
        .call(
            "harness_bump_counter",
            BumpInput {
                name: "shared".into(),
                by: 9,
            },
        )
        .await
        .expect("bump shared");

    let expected = vec![Counter {
        name: "shared".into(),
        value: 9,
    }];

    let alice_payload = alice
        .next_query_update("counters", PUSH_BUDGET)
        .await
        .expect("alice receives push");
    let bob_payload = bob
        .next_query_update("counters", PUSH_BUDGET)
        .await
        .expect("bob receives push");

    let alice_counters: Vec<Counter> =
        serde_json::from_value(alice_payload).expect("decode alice payload");
    let bob_counters: Vec<Counter> =
        serde_json::from_value(bob_payload).expect("decode bob payload");
    assert_eq!(alice_counters, expected);
    assert_eq!(bob_counters, expected);

    app.shutdown().await.expect("shutdown");
}

/// A mutation to a different reactive table must not wake a counter
/// subscription. To rule out a false pass from a dead stream, we then issue a
/// real counter write and confirm that push *does* arrive.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn unrelated_table_mutation_does_not_push() {
    let app = start_app("reactivity_unrelated_table").await;

    let session = app.open_session(None).await.expect("open sse session");
    session
        .subscribe("counters", "harness_list_counters", serde_json::Value::Null)
        .await
        .expect("subscribe to counters");

    // Touches `harness_widgets` only — a different reactive table.
    let _: String = app
        .client()
        .call(
            "harness_add_widget",
            serde_json::json!({ "name": "gadget" }),
        )
        .await
        .expect("add widget");

    let quiet = collect_updates(&session, SILENCE_WINDOW).await;
    assert!(
        quiet.is_empty(),
        "widget mutation must not invalidate a counter subscription, saw: {quiet:?}",
    );

    // The stream is still healthy: a real counter write gets through.
    let _: Counter = app
        .client()
        .call(
            "harness_bump_counter",
            BumpInput {
                name: "live".into(),
                by: 1,
            },
        )
        .await
        .expect("bump live");
    let payload = session
        .next_query_update("counters", PUSH_BUDGET)
        .await
        .expect("counter push proves the stream is alive");
    let counters: Vec<Counter> = serde_json::from_value(payload).expect("decode payload");
    assert_eq!(
        counters,
        vec![Counter {
            name: "live".into(),
            value: 1,
        }],
    );

    app.shutdown().await.expect("shutdown");
}

/// Two subscriptions to the same query with different args. A write that
/// changes only one arg-set's result must push to that subscription alone:
/// the table invalidation re-executes both, but hash comparison suppresses the
/// push for the one whose result did not change.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn subscriptions_isolated_by_args() {
    let app = start_app("reactivity_args_isolation").await;

    let session = app.open_session(None).await.expect("open sse session");
    session
        .subscribe(
            "counter-alpha",
            "harness_get_counter",
            serde_json::json!({ "name": "alpha" }),
        )
        .await
        .expect("subscribe alpha");
    session
        .subscribe(
            "counter-beta",
            "harness_get_counter",
            serde_json::json!({ "name": "beta" }),
        )
        .await
        .expect("subscribe beta");

    let _: Counter = app
        .client()
        .call(
            "harness_bump_counter",
            BumpInput {
                name: "alpha".into(),
                by: 5,
            },
        )
        .await
        .expect("bump alpha");

    // Drain a full window so a stray beta push can't slip past unseen.
    let updates = collect_updates(&session, Duration::from_secs(2)).await;

    // `sub:` is the wire prefix the gateway adds to query-subscription targets.
    let alpha = updates
        .get("sub:counter-alpha")
        .expect("alpha subscription must receive a push");
    let alpha_counter: Option<Counter> =
        serde_json::from_value(alpha.clone()).expect("decode alpha payload");
    assert_eq!(
        alpha_counter,
        Some(Counter {
            name: "alpha".into(),
            value: 5,
        }),
    );
    assert!(
        !updates.contains_key("sub:counter-beta"),
        "beta's result did not change; it must stay silent, saw: {updates:?}",
    );

    app.shutdown().await.expect("shutdown");
}
