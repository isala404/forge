//! Testing utilities for FORGE applications.
//!
//! This module provides comprehensive testing infrastructure for all FORGE function types:
//! - Queries (read-only database access)
//! - Mutations (write operations + job/workflow dispatch)
//! - Actions (external HTTP calls)
//! - Jobs (background processing)
//! - Crons (scheduled tasks)
//! - Workflows (durable multi-step processes)
//!
//! # Philosophy
//!
//! FORGE tests against real PostgreSQL, not in-memory substitutes. There is no
//! `MockDatabase` because PostgreSQL-specific features (advisory locks, LISTEN/NOTIFY,
//! GIN indexes, `FOR UPDATE SKIP LOCKED`) are load-bearing and not faithfully
//! reproduced by SQLite or in-memory stubs. This is a deliberate tradeoff: tests
//! are slower (seconds, not milliseconds) but catch real integration bugs.
//!
//! For unit tests that don't need database access, the test contexts can be used
//! without a pool.
//!
//! # What's mocked and what's not
//!
//! | Layer | Mock | Real |
//! |---|---|---|
//! | HTTP calls | [`MockHttp`] | — |
//! | Job dispatch | [`MockJobDispatch`] | `JobQueue` (needs PG) |
//! | Workflow dispatch | [`MockWorkflowDispatch`] | `WorkflowExecutor` (needs PG) |
//! | Auth context | Builder (`.as_user()`, `.with_role()`) | JWT middleware (needs gateway) |
//! | KV store | Builder (`.with_kv()`) | `KvStore` (needs PG) |
//! | Database | [`IsolatedTestDb`] (real PG, isolated schema) | Production pool |
//!
//! Job and workflow *executors* are not mocked: use `IsolatedTestDb` with the
//! `testcontainers` feature for integration tests that need end-to-end execution.
//!
//! # Database Setup
//!
//! Set `TEST_DATABASE_URL` and use `TestDatabase::from_env()` to connect to a
//! PostgreSQL instance, or enable the `testcontainers` feature for automatic
//! container provisioning via [`IsolatedTestDb`].
//!
//! # Example
//!
//! ```ignore
//! use forge::prelude::*;
//!
//! #[tokio::test]
//! async fn test_authenticated_query() {
//!     let ctx = TestQueryContext::builder()
//!         .as_user(Uuid::new_v4())
//!         .with_role("admin")
//!         .build();
//!
//!     assert!(ctx.auth.is_authenticated());
//!     assert!(ctx.auth.has_role("admin"));
//! }
//! ```

pub mod assertions;
pub mod context;
pub mod db;
pub mod mock_dispatch;
pub mod mock_email;
pub mod mock_http;

pub use assertions::*;
pub use context::*;
pub use db::{IsolatedTestDb, TestDatabase};
pub use mock_dispatch::{DispatchedJob, MockJobDispatch, MockWorkflowDispatch, StartedWorkflow};
pub use mock_email::{MockEmailSender, SentEmail};
pub use mock_http::{MockHttp, MockHttpBuilder, MockRequest, MockResponse};

use std::time::Duration;

/// Default test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default job test timeout.
pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(10);

/// Default workflow test timeout.
pub const DEFAULT_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(60);

/// Default timeout for individual test actions (e.g., `tokio::time::timeout`).
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for eventually-consistent assertion helpers.
pub const ASSERTION_TIMEOUT: Duration = Duration::from_secs(5);
