#![cfg(feature = "testcontainers")]
//! Smoke test: boots the harness, hits a real RPC over HTTP, asserts the
//! envelope wire-shape. Mirrors what `forge-svelte/client.ts` puts on the
//! wire so this proves the harness sees the same bytes a browser would.

use forge::prelude::*;
use forge_harness::HarnessApp;

#[forge::query(auth = "none", tables("forge_jobs"))]
pub async fn harness_smoke_ping(_ctx: &QueryContext) -> Result<String> {
    Ok("pong".to_string())
}

#[forge::query(auth = "none", tables("forge_jobs"))]
pub async fn harness_smoke_echo(_ctx: &QueryContext, message: String) -> Result<String> {
    Ok(format!("echo:{message}"))
}

#[tokio::test]
async fn smoke_query_returns_value() {
    let app = HarnessApp::start("harness_smoke_query_returns_value")
        .await
        .expect("harness boot");

    let result: String = app
        .client()
        .call("harness_smoke_ping", ())
        .await
        .expect("rpc call");
    assert_eq!(result, "pong");

    app.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn smoke_query_args_round_trip() {
    let app = HarnessApp::start("harness_smoke_query_args_round_trip")
        .await
        .expect("harness boot");

    let result: String = app
        .client()
        .call(
            "harness_smoke_echo",
            serde_json::json!({ "message": "hello" }),
        )
        .await
        .expect("rpc call");
    assert_eq!(result, "echo:hello");

    app.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn smoke_unknown_function_returns_error_envelope() {
    let app = HarnessApp::start("harness_smoke_unknown_function")
        .await
        .expect("harness boot");

    let err = app
        .client()
        .expect_error("harness_smoke_no_such_function", ())
        .await
        .expect("expected error envelope");
    assert!(
        !err.code.is_empty(),
        "error envelope must carry a code, got: {:?}",
        err
    );

    app.shutdown().await.expect("shutdown");
}
