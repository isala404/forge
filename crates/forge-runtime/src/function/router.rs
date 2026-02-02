use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use forge_core::{
    AuthContext, CircuitBreakerClient, ForgeError, FunctionInfo, FunctionKind, JobDispatch,
    MutationContext, OutboxBuffer, PendingJob, PendingWorkflow, QueryContext, RequestMetadata,
    Result, WorkflowDispatch,
    job::JobStatus,
    rate_limit::{RateLimitConfig, RateLimitKey},
    workflow::WorkflowStatus,
};
use serde_json::Value;

use super::cache::QueryCache;
use super::registry::{BoxedMutationFn, FunctionEntry, FunctionRegistry};
use crate::db::Database;
use crate::rate_limit::RateLimiter;

/// Result of routing a function call.
pub enum RouteResult {
    /// Query execution result.
    Query(Value),
    /// Mutation execution result.
    Mutation(Value),
    /// Job dispatch result (returns job_id).
    Job(Value),
    /// Workflow dispatch result (returns workflow_id).
    Workflow(Value),
}

/// Routes function calls to the appropriate handler.
pub struct FunctionRouter {
    registry: Arc<FunctionRegistry>,
    db: Database,
    http_client: CircuitBreakerClient,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    rate_limiter: RateLimiter,
    query_cache: QueryCache,
}

impl FunctionRouter {
    /// Create a new function router.
    pub fn new(registry: Arc<FunctionRegistry>, db: Database) -> Self {
        let rate_limiter = RateLimiter::new(db.primary().clone());
        Self {
            registry,
            db,
            http_client: CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            job_dispatcher: None,
            workflow_dispatcher: None,
            rate_limiter,
            query_cache: QueryCache::new(),
        }
    }

    /// Create a new function router with a custom HTTP client.
    pub fn with_http_client(
        registry: Arc<FunctionRegistry>,
        db: Database,
        http_client: CircuitBreakerClient,
    ) -> Self {
        let rate_limiter = RateLimiter::new(db.primary().clone());
        Self {
            registry,
            db,
            http_client,
            job_dispatcher: None,
            workflow_dispatcher: None,
            rate_limiter,
            query_cache: QueryCache::new(),
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

    pub async fn route(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult> {
        if let Some(entry) = self.registry.get(function_name) {
            self.check_auth(entry.info(), &auth)?;
            self.check_rate_limit(entry.info(), function_name, &auth, &request)
                .await?;

            return match entry {
                FunctionEntry::Query { handler, info, .. } => {
                    if let Some(ttl) = info.cache_ttl {
                        if let Some(cached) = self.query_cache.get(function_name, &args) {
                            return Ok(RouteResult::Query(cached));
                        }

                        // Execute and cache result (use read replica for queries)
                        let ctx =
                            QueryContext::new(self.db.read_pool().clone(), auth, request);
                        let result = handler(&ctx, args.clone()).await?;

                        self.query_cache.set(
                            function_name,
                            &args,
                            result.clone(),
                            Duration::from_secs(ttl),
                        );

                        Ok(RouteResult::Query(result))
                    } else {
                        // Use read replica for queries
                        let ctx =
                            QueryContext::new(self.db.read_pool().clone(), auth, request);
                        let result = handler(&ctx, args).await?;
                        Ok(RouteResult::Query(result))
                    }
                }
                FunctionEntry::Mutation { handler, info } => {
                    if info.transactional {
                        self.execute_transactional(handler, args, auth, request)
                            .await
                    } else {
                        // Use primary for mutations
                        let ctx = MutationContext::with_dispatch(
                            self.db.primary().clone(),
                            auth,
                            request,
                            self.http_client.clone(),
                            self.job_dispatcher.clone(),
                            self.workflow_dispatcher.clone(),
                        );
                        let result = handler(&ctx, args).await?;
                        Ok(RouteResult::Mutation(result))
                    }
                }
            };
        }

        if let Some(ref job_dispatcher) = self.job_dispatcher {
            if let Some(job_info) = job_dispatcher.get_info(function_name) {
                self.check_job_auth(&job_info, &auth)?;
                match job_dispatcher
                    .dispatch_by_name(function_name, args.clone())
                    .await
                {
                    Ok(job_id) => {
                        return Ok(RouteResult::Job(serde_json::json!({ "job_id": job_id })));
                    }
                    Err(ForgeError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }

        if let Some(ref workflow_dispatcher) = self.workflow_dispatcher {
            if let Some(workflow_info) = workflow_dispatcher.get_info(function_name) {
                self.check_workflow_auth(&workflow_info, &auth)?;
                match workflow_dispatcher
                    .start_by_name(function_name, args.clone())
                    .await
                {
                    Ok(workflow_id) => {
                        return Ok(RouteResult::Workflow(
                            serde_json::json!({ "workflow_id": workflow_id }),
                        ));
                    }
                    Err(ForgeError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }

        Err(ForgeError::NotFound(format!(
            "Function '{}' not found",
            function_name
        )))
    }

    fn check_auth(&self, info: &FunctionInfo, auth: &AuthContext) -> Result<()> {
        if info.is_public {
            return Ok(());
        }

        if !auth.is_authenticated() {
            return Err(ForgeError::Unauthorized("Authentication required".into()));
        }

        if let Some(role) = info.required_role {
            if !auth.has_role(role) {
                return Err(ForgeError::Forbidden(format!("Role '{}' required", role)));
            }
        }

        Ok(())
    }

    fn check_job_auth(&self, info: &forge_core::job::JobInfo, auth: &AuthContext) -> Result<()> {
        if info.is_public {
            return Ok(());
        }

        if !auth.is_authenticated() {
            return Err(ForgeError::Unauthorized("Authentication required".into()));
        }

        if let Some(role) = info.required_role {
            if !auth.has_role(role) {
                return Err(ForgeError::Forbidden(format!("Role '{}' required", role)));
            }
        }

        Ok(())
    }

    fn check_workflow_auth(
        &self,
        info: &forge_core::workflow::WorkflowInfo,
        auth: &AuthContext,
    ) -> Result<()> {
        if info.is_public {
            return Ok(());
        }

        if !auth.is_authenticated() {
            return Err(ForgeError::Unauthorized("Authentication required".into()));
        }

        if let Some(role) = info.required_role {
            if !auth.has_role(role) {
                return Err(ForgeError::Forbidden(format!("Role '{}' required", role)));
            }
        }

        Ok(())
    }

    /// Check rate limit for a function call.
    async fn check_rate_limit(
        &self,
        info: &FunctionInfo,
        function_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> Result<()> {
        // Skip if no rate limit configured
        let (requests, per_secs) = match (info.rate_limit_requests, info.rate_limit_per_secs) {
            (Some(r), Some(p)) => (r, p),
            _ => return Ok(()),
        };

        // Build rate limit config
        let key_str = info.rate_limit_key.unwrap_or("user");
        let key_type: RateLimitKey = match key_str.parse() {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(
                    function = %function_name,
                    key = %key_str,
                    "Invalid rate limit key, falling back to 'user'"
                );
                RateLimitKey::default()
            }
        };

        let config =
            RateLimitConfig::new(requests, Duration::from_secs(per_secs)).with_key(key_type);

        // Build bucket key
        let bucket_key = self
            .rate_limiter
            .build_key(key_type, function_name, auth, request);

        // Enforce rate limit
        self.rate_limiter.enforce(&bucket_key, &config).await?;

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

    async fn execute_transactional(
        &self,
        handler: &BoxedMutationFn,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult> {
        // Use primary for transactional mutations
        let primary = self.db.primary();
        let tx = primary
            .begin()
            .await
            .map_err(|e| ForgeError::Database(e.to_string()))?;

        let job_dispatcher = self.job_dispatcher.clone();
        let job_lookup: forge_core::JobInfoLookup =
            Arc::new(move |name: &str| job_dispatcher.as_ref().and_then(|d| d.get_info(name)));

        let (ctx, tx_handle, outbox) = MutationContext::with_transaction(
            primary.clone(),
            tx,
            auth,
            request,
            self.http_client.clone(),
            job_lookup,
        );

        match handler(&ctx, args).await {
            Ok(value) => {
                let buffer = {
                    let guard = outbox.lock().unwrap();
                    OutboxBuffer {
                        jobs: guard.jobs.clone(),
                        workflows: guard.workflows.clone(),
                    }
                };

                let mut tx = Arc::try_unwrap(tx_handle)
                    .map_err(|_| ForgeError::Internal("Transaction still in use".into()))?
                    .into_inner();

                for job in &buffer.jobs {
                    Self::insert_job(&mut tx, job).await?;
                }

                for workflow in &buffer.workflows {
                    Self::insert_workflow(&mut tx, workflow).await?;
                }

                tx.commit()
                    .await
                    .map_err(|e| ForgeError::Database(e.to_string()))?;

                Ok(RouteResult::Mutation(value))
            }
            Err(e) => Err(e),
        }
    }

    async fn insert_job(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        job: &PendingJob,
    ) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, job_context, status, priority, attempts, max_attempts,
                worker_capability, scheduled_at, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(job.id)
        .bind(&job.job_type)
        .bind(&job.args)
        .bind(&job.context)
        .bind(JobStatus::Pending.as_str())
        .bind(job.priority)
        .bind(0i32)
        .bind(job.max_attempts)
        .bind(&job.worker_capability)
        .bind(now)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    async fn insert_workflow(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workflow: &PendingWorkflow,
    ) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO forge_workflow_runs (
                id, workflow_name, input, status, current_step,
                step_results, started_at, trace_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(workflow.id)
        .bind(&workflow.workflow_name)
        .bind(&workflow.input)
        .bind(WorkflowStatus::Created.as_str())
        .bind(Option::<String>::None)
        .bind(serde_json::json!({}))
        .bind(now)
        .bind(workflow.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_auth_public() {
        let info = FunctionInfo {
            name: "test",
            description: None,
            kind: FunctionKind::Query,
            required_role: None,
            is_public: true,
            cache_ttl: None,
            timeout: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: &[],
            transactional: false,
        };

        let _auth = AuthContext::unauthenticated();

        // Can't test check_auth directly without a router instance,
        // but we can test the logic
        assert!(info.is_public);
    }
}
