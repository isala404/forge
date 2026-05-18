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

// ── HandlerContext impls ──────────────────────────────────────────────────────

impl HandlerContext for crate::function::QueryContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::function::MutationContext {
    fn db(&self) -> ForgeDb {
        // MutationContext::tx() returns DbConn, not ForgeDb.
        // For HandlerContext we expose the pool-backed ForgeDb view, which
        // intentionally bypasses the active transaction.
        crate::function::ForgeDb::from_pool(self.bypass_pool())
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::job::JobContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::cron::CronContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::daemon::DaemonContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::webhook::WebhookContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::workflow::WorkflowContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

impl HandlerContext for crate::mcp::McpToolContext {
    fn db(&self) -> ForgeDb {
        self.db()
    }

    fn db_conn(&self) -> DbConn<'_> {
        self.db_conn()
    }
}

// ── AuthenticatedContext impls ────────────────────────────────────────────────

impl AuthenticatedContext for crate::function::QueryContext {
    fn user_id(&self) -> crate::error::Result<Uuid> {
        self.user_id()
    }

    fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id()
    }
}

impl AuthenticatedContext for crate::function::MutationContext {
    fn user_id(&self) -> crate::error::Result<Uuid> {
        self.user_id()
    }

    fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id()
    }
}

impl AuthenticatedContext for crate::mcp::McpToolContext {
    fn user_id(&self) -> crate::error::Result<Uuid> {
        self.user_id()
    }

    fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id()
    }
}

// ── Sealed impls ──────────────────────────────────────────────────────────────

impl Sealed for crate::function::QueryContext {}
impl Sealed for crate::function::MutationContext {}
impl Sealed for crate::job::JobContext {}
impl Sealed for crate::cron::CronContext {}
impl Sealed for crate::daemon::DaemonContext {}
impl Sealed for crate::webhook::WebhookContext {}
impl Sealed for crate::workflow::WorkflowContext {}
impl Sealed for crate::mcp::McpToolContext {}
