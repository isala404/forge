//! Authentication and authorization scenarios.
//!
//! Every frontend client is one of three callers: anonymous, an authenticated
//! user, or an authenticated user holding a role. The gateway must treat each
//! correctly — reject the anonymous caller from a private handler, isolate one
//! user's rows from another's, and let a role gate through only the holders of
//! that role. This is the browserless proxy for "log in, see only your data,
//! hit a 403 on the admin page": same gateway, same JWT verification, same
//! `require_auth` path, same RPC envelope a browser client would consume.

/// Sentinel test so `cargo test -p forge-harness` (without `--features
/// testcontainers`) doesn't silently report "0 tests passed" and lull a
/// contributor into thinking they ran the scenario suite. Always passes;
/// its job is to print the hint.
#[test]
fn ensure_testcontainers_feature_enabled() {
    eprintln!(
        "forge-harness auth scenarios are gated on `--features testcontainers`. \
         Re-run with `cargo test -p forge-harness --features testcontainers` \
         to exercise authentication paths against a real Postgres."
    );
}

#[cfg(feature = "testcontainers")]
#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "testcontainers")]
use common::{Note, start_app};
#[cfg(feature = "testcontainers")]
use uuid::Uuid;

/// A private query must reject an anonymous caller at the gateway — before the
/// handler body runs — with the `UNAUTHORIZED` envelope a client turns into a
/// 401. If this regressed to a success the whole private surface would leak.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn private_query_rejects_anonymous_caller() {
    let app = start_app("auth_anon_query").await;

    let error = app
        .client()
        .expect_error("harness_my_notes", ())
        .await
        .expect("anonymous call to a private query must fail, not succeed");
    assert_eq!(
        error.code, "UNAUTHORIZED",
        "a private query refused to an anonymous caller must carry the UNAUTHORIZED \
         code, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// The same gate applies to a private mutation: an anonymous caller is turned
/// away before any write is attempted.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn private_mutation_rejects_anonymous_caller() {
    let app = start_app("auth_anon_mutation").await;

    let error = app
        .client()
        .expect_error(
            "harness_create_note",
            serde_json::json!({ "body": "ghost" }),
        )
        .await
        .expect("anonymous call to a private mutation must fail, not succeed");
    assert_eq!(
        error.code, "UNAUTHORIZED",
        "a private mutation refused to an anonymous caller must carry the UNAUTHORIZED \
         code, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// The core tenancy guarantee: a note created by one user is visible only to
/// that user. This exercises `ctx.user_id()` on both sides — the mutation
/// stamps `owner_id` from the JWT subject, and the query filters by it — so a
/// regression on either side (a dropped `WHERE`, a mis-read subject claim)
/// surfaces as one user seeing the other's rows.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn notes_stay_isolated_between_users() {
    let app = start_app("auth_user_isolation").await;

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let client_a = app
        .client_as(user_a)
        .expect("authenticated client for user A");
    let client_b = app
        .client_as(user_b)
        .expect("authenticated client for user B");

    // Each create echoes the persisted row back: proof the mutation stamped
    // `owner_id` from the caller's JWT subject, not from anything client-sent.
    for body in ["a-one", "a-two"] {
        let created: Note = client_a
            .call("harness_create_note", serde_json::json!({ "body": body }))
            .await
            .expect("user A creates a note");
        assert_eq!(
            created.owner_id, user_a,
            "a created note must be owned by its authenticated creator",
        );
    }
    let created_b: Note = client_b
        .call(
            "harness_create_note",
            serde_json::json!({ "body": "b-one" }),
        )
        .await
        .expect("user B creates a note");
    assert_eq!(created_b.owner_id, user_b);

    // User A sees exactly their two notes — never user B's.
    let a_notes: Vec<Note> = client_a
        .call("harness_my_notes", ())
        .await
        .expect("user A lists their notes");
    let a_bodies: Vec<&str> = a_notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(
        a_bodies,
        vec!["a-one", "a-two"],
        "user A must see exactly their own notes, in order, saw: {a_notes:?}",
    );
    assert!(
        a_notes.iter().all(|n| n.owner_id == user_a),
        "every note in user A's list must be owned by user A, saw: {a_notes:?}",
    );

    // User B sees exactly their one note — never user A's.
    let b_notes: Vec<Note> = client_b
        .call("harness_my_notes", ())
        .await
        .expect("user B lists their notes");
    let b_bodies: Vec<&str> = b_notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(
        b_bodies,
        vec!["b-one"],
        "user B must see exactly their own note, saw: {b_notes:?}",
    );
    assert!(
        b_notes.iter().all(|n| n.owner_id == user_b),
        "every note in user B's list must be owned by user B, saw: {b_notes:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// A role-gated handler must reject an authenticated caller who lacks the
/// role with `FORBIDDEN` — distinct from the `UNAUTHORIZED` an anonymous
/// caller gets. Authentication alone is not authorization.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn role_gated_query_rejects_missing_role() {
    let app = start_app("auth_role_missing").await;

    let plain_user = app
        .client_as(Uuid::new_v4())
        .expect("authenticated client without roles");
    let error = plain_user
        .expect_error("harness_admin_note_count", ())
        .await
        .expect("a non-admin call to an admin-gated query must fail, not succeed");
    assert_eq!(
        error.code, "FORBIDDEN",
        "an authenticated caller missing the required role must be FORBIDDEN, not \
         UNAUTHORIZED, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// An expired JWT must be rejected with `UNAUTHORIZED` before the handler
/// runs. If this regressed to a success the gateway would honor any token
/// whose signature happens to verify, regardless of `exp`.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn expired_token_is_rejected_as_unauthorized() {
    let app = start_app("auth_expired_token").await;

    // duration_secs(-3600) issues a token whose `exp` is one hour in the past.
    let user_id = Uuid::new_v4();
    let expired = app
        .issue_token_with_claims(|b| b.user_id(user_id).duration_secs(-3600))
        .expect("issue expired token");

    let client = app.client().with_token(expired);
    let error = client
        .expect_error("harness_my_notes", ())
        .await
        .expect("a call with an expired token must fail, not succeed");
    assert_eq!(
        error.code, "UNAUTHORIZED",
        "expired tokens must carry UNAUTHORIZED, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// A malformed bearer token (not even three dotted segments) must be rejected
/// at the gateway, not silently treated as anonymous.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn malformed_token_is_rejected_as_unauthorized() {
    let app = start_app("auth_malformed_token").await;

    let client = app.client().with_token("this-is-not-a-jwt");
    let error = client
        .expect_error("harness_my_notes", ())
        .await
        .expect("a call with a malformed token must fail, not succeed");
    assert_eq!(
        error.code, "UNAUTHORIZED",
        "malformed tokens must carry UNAUTHORIZED, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// A token signed with a different secret must be rejected. This is the
/// signature-verification path: a regression that disabled verify would let
/// any attacker mint tokens against any deployment.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn wrong_secret_token_is_rejected_as_unauthorized() {
    use forge_core::TokenIssuer;
    use forge_runtime::gateway::HmacTokenIssuer;
    let app = start_app("auth_wrong_secret").await;

    // Mint a token using an unrelated secret. Same shape as the harness's
    // tokens; only the HMAC over header+payload differs.
    let attacker_secret = "totally-different-secret-not-used-by-the-harness-instance";
    let attacker_cfg = forge_runtime::gateway::AuthConfig::with_secret(attacker_secret.to_string());
    let attacker_issuer = HmacTokenIssuer::from_config(&attacker_cfg).expect("issuer");
    let claims = forge_core::Claims::builder()
        .user_id(Uuid::new_v4())
        .duration_secs(3600)
        .build()
        .expect("claims");
    let forged = attacker_issuer.sign(&claims).expect("sign");

    let client = app.client().with_token(forged);
    let error = client
        .expect_error("harness_my_notes", ())
        .await
        .expect("a call with a wrong-secret token must fail, not succeed");
    assert_eq!(
        error.code, "UNAUTHORIZED",
        "wrong-secret tokens must carry UNAUTHORIZED, saw: {error:?}",
    );

    app.shutdown().await.expect("shutdown");
}

/// The other half of the role gate: a caller holding the role passes, and the
/// handler runs. The count reflects a note the same caller just created, which
/// proves the request reached the body rather than short-circuiting.
#[cfg(feature = "testcontainers")]
#[tokio::test]
async fn role_gated_query_admits_present_role() {
    let app = start_app("auth_role_present").await;

    let admin = app
        .client_as_with_roles(Uuid::new_v4(), &["admin"])
        .expect("authenticated client holding the admin role");

    admin
        .call::<_, Note>("harness_create_note", serde_json::json!({ "body": "seed" }))
        .await
        .expect("admin creates a note");

    let count: i64 = admin
        .call("harness_admin_note_count", ())
        .await
        .expect("a caller holding the admin role must pass the role gate");
    assert_eq!(
        count, 1,
        "the admin-gated count must run and see the one note created in this test",
    );

    app.shutdown().await.expect("shutdown");
}
