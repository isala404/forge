#![cfg(feature = "testcontainers")]
//! Authentication and authorization scenarios.
//!
//! Every frontend client is one of three callers: anonymous, an authenticated
//! user, or an authenticated user holding a role. The gateway must treat each
//! correctly — reject the anonymous caller from a private handler, isolate one
//! user's rows from another's, and let a role gate through only the holders of
//! that role. This is the browserless proxy for "log in, see only your data,
//! hit a 403 on the admin page": same gateway, same JWT verification, same
//! `require_auth` path, same RPC envelope a browser client would consume.

mod common;

use common::{Note, start_app};
use uuid::Uuid;

/// A private query must reject an anonymous caller at the gateway — before the
/// handler body runs — with the `UNAUTHORIZED` envelope a client turns into a
/// 401. If this regressed to a success the whole private surface would leak.
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

/// The other half of the role gate: a caller holding the role passes, and the
/// handler runs. The count reflects a note the same caller just created, which
/// proves the request reached the body rather than short-circuiting.
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
