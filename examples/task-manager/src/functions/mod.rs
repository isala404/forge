//! Function handlers for the Task Manager.
//!
//! This module organizes all FORGE functions by type:
//! - queries: Read-only database queries
//! - mutations: Write operations
//! - actions: External API calls
//! - jobs: Background processing
//! - crons: Scheduled tasks
//! - workflows: Multi-step sagas

pub mod queries;
pub mod mutations;
pub mod actions;
pub mod jobs;
pub mod crons;
pub mod workflows;

pub use queries::*;
pub use mutations::*;
pub use actions::*;
pub use jobs::*;
pub use crons::*;
pub use workflows::*;
