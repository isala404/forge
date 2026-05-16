// TODO(pre-1.0): Collapse to 4 generic contexts per 07-DELETION-LIST.md

//! Test context builders for all FORGE function types.
//!
//! Each test context provides:
//! - Authentication configuration (user ID, roles, claims)
//! - Optional database pool for integration tests
//! - Mocking capabilities (HTTP, job dispatch, workflow dispatch)
//! - Context-specific fields (job_id, attempt, cron schedule, etc.)

use crate::function::AuthContext;
use std::collections::HashMap;
use uuid::Uuid;

mod cron;
mod daemon;
mod job;
mod mcp_tool;
mod mutation;
mod query;
mod webhook;
mod workflow;

pub use cron::{TestCronContext, TestCronContextBuilder};
pub use daemon::{TestDaemonContext, TestDaemonContextBuilder};
pub use job::{TestJobContext, TestJobContextBuilder, TestProgressUpdate};
pub use mcp_tool::{TestMcpToolContext, TestMcpToolContextBuilder};
pub use mutation::{TestMutationContext, TestMutationContextBuilder};
pub use query::{TestQueryContext, TestQueryContextBuilder};
pub use webhook::{TestWebhookContext, TestWebhookContextBuilder};
pub use workflow::{TestWorkflowContext, TestWorkflowContextBuilder};

/// Build an AuthContext from test builder fields.
/// Handles UUID-based users, subject-based auth (Firebase/Clerk), and unauthenticated.
pub(crate) fn build_test_auth(
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
) -> AuthContext {
    if let Some(user_id) = user_id {
        AuthContext::authenticated(user_id, roles, claims)
    } else if claims.contains_key("sub") {
        AuthContext::authenticated_without_uuid(roles, claims)
    } else {
        AuthContext::unauthenticated()
    }
}
