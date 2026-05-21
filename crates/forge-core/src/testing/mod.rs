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

pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(30);

pub const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(10);

pub const DEFAULT_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(60);
