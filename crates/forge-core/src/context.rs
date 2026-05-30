//! Sealed trait family for Forge handler contexts.
//!
//! These traits let shared helper functions accept any context type without
//! knowing the concrete type. Three levels, each building on the previous:
//!
//! - [`HandlerContext`]: database access available to every handler kind.
//! - [`AuthenticatedContext`]: adds auth accessors for contexts that carry an
//!   authenticated user (queries, mutations, MCP tools).
//!
//! All traits are sealed — they cannot be implemented outside forge-core.
//! The proc macros emit the required impls automatically.
//!
//! # Example
//!
//! ```ignore
//! use forge_core::context::HandlerContext;
//!
//! async fn count_rows<C: HandlerContext>(ctx: &C) -> forge_core::Result<i64> {
//!     sqlx::query_scalar!("SELECT COUNT(*) FROM items")
//!         .fetch_one(ctx.db())
//!         .await
//!         .map_err(forge_core::ForgeError::Database)
//! }
//! ```

use uuid::Uuid;

use crate::__sealed::Sealed;
use crate::function::{DbConn, ForgeDb};

/// Base trait for all Forge handler contexts.
///
/// Provides access to the database pool — the one capability shared by every
/// handler kind (queries, mutations, jobs, crons, daemons, webhooks, workflows,
/// MCP tools).
///
/// Sealed: only forge-core can implement this trait.
pub trait HandlerContext: Sealed {
    /// Database handle with automatic `db.query` tracing spans.
    ///
    /// Works directly with sqlx compile-time checked macros:
    /// ```ignore
    /// sqlx::query_as!(Item, "SELECT * FROM items")
    ///     .fetch_all(ctx.db())
    ///     .await?
    /// ```
    fn db(&self) -> ForgeDb;

    /// Unified connection handle for shared helper functions.
    ///
    /// Prefer this over `db()` when writing helpers that need to work with
    /// both pool-backed and transaction-backed contexts.
    fn db_conn(&self) -> DbConn<'_>;
}

/// Trait for contexts that carry an authenticated user.
///
/// Implemented by [`QueryContext`], [`MutationContext`], and [`McpToolContext`].
/// Extends [`HandlerContext`] with user identity and tenant accessors.
///
/// Sealed: only forge-core can implement this trait.
///
/// [`QueryContext`]: crate::function::QueryContext
/// [`MutationContext`]: crate::function::MutationContext
/// [`McpToolContext`]: crate::mcp::McpToolContext
pub trait AuthenticatedContext: HandlerContext {
    /// Returns the authenticated user's UUID, or `Unauthorized` if the request
    /// is not authenticated or the subject is not a UUID.
    fn user_id(&self) -> crate::error::Result<Uuid>;

    /// Returns the tenant ID from the `tenant_id` JWT claim, if present.
    fn tenant_id(&self) -> Option<Uuid>;
}

/// Forward [`HandlerContext`] to each context type's inherent `db()`/`db_conn()`.
macro_rules! impl_handler_context {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl HandlerContext for $ty {
                fn db(&self) -> ForgeDb { self.db() }
                fn db_conn(&self) -> DbConn<'_> { self.db_conn() }
            }
        )+
    };
}

impl_handler_context!(
    crate::function::QueryContext,
    crate::job::JobContext,
    crate::cron::CronContext,
    crate::daemon::DaemonContext,
    crate::webhook::WebhookContext,
    crate::workflow::WorkflowContext,
    crate::mcp::McpToolContext,
);

// MutationContext is the one exception: its inherent `db()` returns a
// transaction-bound handle, so the trait impl exposes the pool-backed view
// that intentionally bypasses the active transaction.
impl HandlerContext for crate::function::MutationContext {
    fn db(&self) -> ForgeDb {
        crate::function::ForgeDb::from_pool(self.pool_outside_transaction())
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

/// Forward [`AuthenticatedContext`] to each context type's inherent accessors.
macro_rules! impl_authenticated_context {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl AuthenticatedContext for $ty {
                fn user_id(&self) -> crate::error::Result<Uuid> { self.user_id() }
                fn tenant_id(&self) -> Option<Uuid> { self.tenant_id() }
            }
        )+
    };
}

impl_authenticated_context!(
    crate::function::QueryContext,
    crate::function::MutationContext,
    crate::mcp::McpToolContext,
);

macro_rules! impl_sealed {
    ($($ty:ty),+ $(,)?) => {
        $( impl Sealed for $ty {} )+
    };
}

impl_sealed!(
    crate::function::QueryContext,
    crate::function::MutationContext,
    crate::job::JobContext,
    crate::cron::CronContext,
    crate::daemon::DaemonContext,
    crate::webhook::WebhookContext,
    crate::workflow::WorkflowContext,
    crate::mcp::McpToolContext,
);
