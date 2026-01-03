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
//! Following sqlx's testing philosophy, we recommend testing against real databases
//! rather than mocks. However, for unit tests that don't need database access,
//! the test contexts can be used without a database connection.
//!
//! # Zero-Config Database
//!
//! When the `embedded-test-db` feature is enabled, `TestDatabase` will automatically
//! download and start an embedded PostgreSQL instance if `DATABASE_URL` is not set.
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
pub mod mock_http;

pub use assertions::*;
pub use context::*;
pub use db::{IsolatedTestDb, TestDatabase};
pub use mock_dispatch::{DispatchedJob, MockJobDispatch, MockWorkflowDispatch, StartedWorkflow};
pub use mock_http::{MockHttp, MockHttpBuilder, MockRequest, MockResponse};

use std::time::Duration;

/// Default test timeout.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default job test timeout.
pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(10);

/// Default workflow test timeout.
pub const DEFAULT_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(60);
