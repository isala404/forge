//! Test context builders for all FORGE function types.
//!
//! Each test context provides:
//! - Authentication configuration (user ID, roles, claims)
//! - Optional database pool for integration tests
//! - Mocking capabilities (HTTP, job dispatch, workflow dispatch)
//! - Context-specific fields (job_id, attempt, cron schedule, etc.)

mod action;
mod cron;
mod job;
mod mutation;
mod query;
mod workflow;

pub use action::{TestActionContext, TestActionContextBuilder};
pub use cron::{TestCronContext, TestCronContextBuilder};
pub use job::{TestJobContext, TestJobContextBuilder, TestProgressUpdate};
pub use mutation::{TestMutationContext, TestMutationContextBuilder};
pub use query::{TestQueryContext, TestQueryContextBuilder};
pub use workflow::{TestWorkflowContext, TestWorkflowContextBuilder};
