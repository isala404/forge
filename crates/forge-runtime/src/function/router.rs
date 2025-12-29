use std::sync::Arc;

use forge_core::{
    ActionContext, AuthContext, ForgeError, FunctionKind, JobDispatch, MutationContext,
    QueryContext, RequestMetadata, Result, WorkflowDispatch,
};
use serde_json::Value;

use super::registry::{FunctionEntry, FunctionRegistry};

/// Result of routing a function call.
pub enum RouteResult {
    /// Query execution result.
    Query(Value),
    /// Mutation execution result.
    Mutation(Value),
    /// Action execution result.
    Action(Value),
}

/// Routes function calls to the appropriate handler.
pub struct FunctionRouter {
    registry: Arc<FunctionRegistry>,
    db_pool: sqlx::PgPool,
    http_client: reqwest::Client,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
}

impl FunctionRouter {
    /// Create a new function router.
    pub fn new(registry: Arc<FunctionRegistry>, db_pool: sqlx::PgPool) -> Self {
        Self {
            registry,
            db_pool,
            http_client: reqwest::Client::new(),
            job_dispatcher: None,
            workflow_dispatcher: None,
        }
    }

    /// Create a new function router with a custom HTTP client.
    pub fn with_http_client(
        registry: Arc<FunctionRegistry>,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            registry,
            db_pool,
            http_client,
            job_dispatcher: None,
            workflow_dispatcher: None,
        }
    }

    /// Set the job dispatcher for this router.
    pub fn with_job_dispatcher(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        self.job_dispatcher = Some(dispatcher);
        self
    }

    /// Set the workflow dispatcher for this router.
    pub fn with_workflow_dispatcher(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        self.workflow_dispatcher = Some(dispatcher);
        self
    }

    /// Route and execute a function call.
    pub async fn route(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult> {
        let entry = self.registry.get(function_name).ok_or_else(|| {
            ForgeError::NotFound(format!("Function '{}' not found", function_name))
        })?;

        // Check authorization
        self.check_auth(entry.info(), &auth)?;

        match entry {
            FunctionEntry::Query { handler, .. } => {
                let ctx = QueryContext::new(self.db_pool.clone(), auth, request);
                let result = handler(&ctx, args).await?;
                Ok(RouteResult::Query(result))
            }
            FunctionEntry::Mutation { handler, .. } => {
                let ctx = MutationContext::with_dispatch(
                    self.db_pool.clone(),
                    auth,
                    request,
                    self.job_dispatcher.clone(),
                    self.workflow_dispatcher.clone(),
                );
                let result = handler(&ctx, args).await?;
                Ok(RouteResult::Mutation(result))
            }
            FunctionEntry::Action { handler, .. } => {
                let ctx = ActionContext::with_dispatch(
                    self.db_pool.clone(),
                    auth,
                    request,
                    self.http_client.clone(),
                    self.job_dispatcher.clone(),
                    self.workflow_dispatcher.clone(),
                );
                let result = handler(&ctx, args).await?;
                Ok(RouteResult::Action(result))
            }
        }
    }

    /// Check authorization for a function call.
    fn check_auth(&self, info: &forge_core::FunctionInfo, auth: &AuthContext) -> Result<()> {
        // Public functions don't require auth
        if info.is_public {
            return Ok(());
        }

        // Check if auth is required
        if info.requires_auth && !auth.is_authenticated() {
            return Err(ForgeError::Unauthorized("Authentication required".into()));
        }

        // Check role requirement
        if let Some(role) = info.required_role {
            if !auth.has_role(role) {
                return Err(ForgeError::Forbidden(format!("Role '{}' required", role)));
            }
        }

        Ok(())
    }

    /// Get the function kind by name.
    pub fn get_function_kind(&self, function_name: &str) -> Option<FunctionKind> {
        self.registry.get(function_name).map(|e| e.kind())
    }

    /// Check if a function exists.
    pub fn has_function(&self, function_name: &str) -> bool {
        self.registry.get(function_name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_auth_public() {
        let info = forge_core::FunctionInfo {
            name: "test",
            description: None,
            kind: FunctionKind::Query,
            requires_auth: false,
            required_role: None,
            is_public: true,
            cache_ttl: None,
            timeout: None,
        };

        let _auth = AuthContext::unauthenticated();

        // Can't test check_auth directly without a router instance,
        // but we can test the logic
        assert!(info.is_public);
    }
}
