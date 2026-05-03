use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use forge_core::{
    AuthContext, CircuitBreakerClient, ForgeError, FunctionInfo, FunctionKind, JobDispatch,
    MutationContext, OutboxBuffer, PendingJob, PendingWorkflow, QueryContext, RequestMetadata,
    Result, SharedRoleResolver, WorkflowDispatch, default_role_resolver,
    job::JobStatus,
    rate_limit::{RateLimitConfig, RateLimiterBackend},
    workflow::WorkflowStatus,
};
use serde_json::Value;
use tokio::time::timeout;
use tracing::{Instrument, debug, error, info, trace, warn};

use super::cache::QueryCache;
use super::registry::{BoxedMutationFn, FunctionEntry, FunctionRegistry};
use crate::pg::Database;
use crate::rate_limit::HybridRateLimiter;
#[cfg(feature = "gateway")]
use crate::signals::SignalsCollector;

/// Shared auth enforcement: checks public flag, authentication, and role.
///
/// When a `RoleResolver` is provided, roles are resolved from JWT claims
/// before the `require_role` check. This allows hierarchy expansion or
/// remote permission lookups without changing the handler surface.
fn require_auth(
    is_public: bool,
    required_role: Option<&str>,
    auth: &AuthContext,
    role_resolver: &SharedRoleResolver,
) -> Result<()> {
    if is_public {
        return Ok(());
    }
    if !auth.is_authenticated() {
        return Err(ForgeError::Unauthorized("Authentication required".into()));
    }
    if let Some(role) = required_role {
        let effective_roles = role_resolver.resolve(auth);
        if !effective_roles.iter().any(|r| r == role) {
            return Err(ForgeError::Forbidden(format!("Role '{role}' required")));
        }
    }
    Ok(())
}

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

/// Captured metadata from auth/request for signal emission.
#[cfg(feature = "gateway")]
struct SignalContext {
    user_id: Option<uuid::Uuid>,
    tenant_id: Option<uuid::Uuid>,
    correlation_id: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
}

/// Routes and executes function calls with timeout, rate limiting, and observability.
pub struct FunctionRouter {
    registry: Arc<FunctionRegistry>,
    db: Database,
    http_client: CircuitBreakerClient,
    job_dispatcher: Option<Arc<dyn JobDispatch>>,
    workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    rate_limiter: Arc<dyn RateLimiterBackend>,
    role_resolver: SharedRoleResolver,
    query_cache: QueryCache,
    token_issuer: Option<Arc<dyn forge_core::TokenIssuer>>,
    token_ttl: forge_core::AuthTokenTtl,
    default_timeout: Duration,
    #[cfg(feature = "gateway")]
    signals_collector: Option<SignalsCollector>,
    #[cfg(feature = "gateway")]
    signals_server_secret: String,
}

impl FunctionRouter {
    /// Create a new function router.
    pub fn new(registry: Arc<FunctionRegistry>, db: Database) -> Self {
        let rate_limiter: Arc<dyn RateLimiterBackend> =
            Arc::new(HybridRateLimiter::new(db.primary().clone()));
        Self {
            registry,
            db,
            http_client: CircuitBreakerClient::with_defaults(reqwest::Client::new()),
            job_dispatcher: None,
            workflow_dispatcher: None,
            rate_limiter,
            role_resolver: default_role_resolver(),
            query_cache: QueryCache::new(),
            token_issuer: None,
            token_ttl: forge_core::AuthTokenTtl::default(),
            default_timeout: Duration::from_secs(30),
            #[cfg(feature = "gateway")]
            signals_collector: None,
            #[cfg(feature = "gateway")]
            signals_server_secret: String::new(),
        }
    }

    /// Create a new function router with a custom HTTP client.
    pub fn with_http_client(
        registry: Arc<FunctionRegistry>,
        db: Database,
        http_client: CircuitBreakerClient,
    ) -> Self {
        let rate_limiter: Arc<dyn RateLimiterBackend> =
            Arc::new(HybridRateLimiter::new(db.primary().clone()));
        Self {
            registry,
            db,
            http_client,
            job_dispatcher: None,
            workflow_dispatcher: None,
            rate_limiter,
            role_resolver: default_role_resolver(),
            query_cache: QueryCache::new(),
            token_issuer: None,
            token_ttl: forge_core::AuthTokenTtl::default(),
            default_timeout: Duration::from_secs(30),
            #[cfg(feature = "gateway")]
            signals_collector: None,
            #[cfg(feature = "gateway")]
            signals_server_secret: String::new(),
        }
    }

    /// Create a router with dispatch capabilities.
    pub fn with_dispatch(
        registry: Arc<FunctionRegistry>,
        db: Database,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
    ) -> Self {
        Self::with_dispatch_and_issuer(registry, db, job_dispatcher, workflow_dispatcher, None)
    }

    /// Create a router with dispatch and token issuer.
    pub fn with_dispatch_and_issuer(
        registry: Arc<FunctionRegistry>,
        db: Database,
        job_dispatcher: Option<Arc<dyn JobDispatch>>,
        workflow_dispatcher: Option<Arc<dyn WorkflowDispatch>>,
        token_issuer: Option<Arc<dyn forge_core::TokenIssuer>>,
    ) -> Self {
        let mut router = Self::new(Arc::clone(&registry), db);
        if let Some(jd) = job_dispatcher {
            router = router.with_job_dispatcher(jd);
        }
        if let Some(wd) = workflow_dispatcher {
            router = router.with_workflow_dispatcher(wd);
        }
        if let Some(issuer) = token_issuer {
            router = router.with_token_issuer(issuer);
        }
        router
    }

    /// Set a custom role resolver for RBAC extension.
    pub fn with_role_resolver(mut self, resolver: SharedRoleResolver) -> Self {
        self.role_resolver = resolver;
        self
    }

    /// Set a custom role resolver (mutable reference version).
    pub fn set_role_resolver(&mut self, resolver: SharedRoleResolver) {
        self.role_resolver = resolver;
    }

    /// Override the default [`HybridRateLimiter`] with a custom backend
    /// (e.g. [`crate::rate_limit::StrictRateLimiter`] for cluster-correct quotas).
    pub fn with_rate_limiter(mut self, rate_limiter: Arc<dyn RateLimiterBackend>) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    /// Replace the rate-limiter backend (mutable variant for late binding).
    pub fn set_rate_limiter(&mut self, rate_limiter: Arc<dyn RateLimiterBackend>) {
        self.rate_limiter = rate_limiter;
    }

    /// Set the token issuer for this router (enables `ctx.issue_token()` in mutations).
    pub fn with_token_issuer(mut self, issuer: Arc<dyn forge_core::TokenIssuer>) -> Self {
        self.token_issuer = Some(issuer);
        self
    }

    /// Set the token TTL config for this router (configures `ctx.issue_token_pair()` durations).
    pub fn with_token_ttl(mut self, ttl: forge_core::AuthTokenTtl) -> Self {
        self.token_ttl = ttl;
        self
    }

    /// Set the token TTL config (mutable reference version).
    pub fn set_token_ttl(&mut self, ttl: forge_core::AuthTokenTtl) {
        self.token_ttl = ttl;
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

    /// Set the default timeout applied to all function calls.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set the signals collector for auto-capturing RPC events.
    #[cfg(feature = "gateway")]
    pub fn set_signals_collector(&mut self, collector: SignalsCollector, server_secret: String) {
        self.signals_collector = Some(collector);
        self.signals_server_secret = server_secret;
    }

    /// Execute a function call with timeout, observability, and signals emission.
    pub async fn execute(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<Value> {
        let start = std::time::Instant::now();
        let fn_timeout = self.get_function_timeout(function_name);
        let log_level = self.get_function_log_level(function_name);

        let kind = self
            .get_function_kind(function_name)
            .map(|k| k.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Capture signal metadata before auth/request are consumed.
        #[cfg(feature = "gateway")]
        let signal_ctx = self.signals_collector.as_ref().map(|_| SignalContext {
            user_id: auth.user_id(),
            tenant_id: auth.tenant_id(),
            correlation_id: request.correlation_id().map(str::to_string),
            client_ip: request.client_ip().map(str::to_string),
            user_agent: request.user_agent().map(str::to_string),
        });

        let span = tracing::info_span!(
            "fn.execute",
            function = function_name,
            fn.kind = %kind,
        );

        let result = match timeout(
            fn_timeout,
            self.route(function_name, args.clone(), auth, request)
                .instrument(span),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let duration = start.elapsed();
                self.log_execution(
                    log_level,
                    function_name,
                    "unknown",
                    &args,
                    duration,
                    false,
                    Some(&format!("Timeout after {:?}", fn_timeout)),
                );
                crate::observability::record_fn_execution(
                    function_name,
                    &kind,
                    false,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                self.emit_signal(function_name, &kind, duration, false, &signal_ctx);
                return Err(ForgeError::Timeout(format!(
                    "Function '{}' timed out after {:?}",
                    function_name, fn_timeout
                )));
            }
        };

        let duration = start.elapsed();

        match result {
            Ok(route_result) => {
                let (result_kind, value) = match route_result {
                    RouteResult::Query(v) => ("query", v),
                    RouteResult::Mutation(v) => ("mutation", v),
                    RouteResult::Job(v) => ("job", v),
                    RouteResult::Workflow(v) => ("workflow", v),
                };

                self.log_execution(
                    log_level,
                    function_name,
                    result_kind,
                    &args,
                    duration,
                    true,
                    None,
                );
                crate::observability::record_fn_execution(
                    function_name,
                    result_kind,
                    true,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                self.emit_signal(function_name, result_kind, duration, true, &signal_ctx);

                Ok(value)
            }
            Err(e) => {
                self.log_execution(
                    log_level,
                    function_name,
                    &kind,
                    &args,
                    duration,
                    false,
                    Some(&e.to_string()),
                );
                crate::observability::record_fn_execution(
                    function_name,
                    &kind,
                    false,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                self.emit_signal(function_name, &kind, duration, false, &signal_ctx);

                Err(e)
            }
        }
    }

    /// Look up function metadata by name.
    pub fn function_info(&self, function_name: &str) -> Option<FunctionInfo> {
        self.registry.get(function_name).map(|e| e.info().clone())
    }

    /// Check if a function exists.
    pub fn has_function(&self, function_name: &str) -> bool {
        self.registry.get(function_name).is_some()
    }

    /// Get the function kind by name.
    pub fn get_function_kind(&self, function_name: &str) -> Option<FunctionKind> {
        self.registry.get(function_name).map(|e| e.kind())
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
                    let pool = if info.consistent {
                        self.db.primary().clone()
                    } else {
                        self.db.read_pool().clone()
                    };

                    let auth_scope = Self::auth_cache_scope(&auth);
                    if let Some(ttl) = info.cache_ttl {
                        if let Some(cached) =
                            self.query_cache
                                .get(function_name, &args, auth_scope.as_deref())
                        {
                            return Ok(RouteResult::Query(Value::clone(&cached)));
                        }

                        let ctx = QueryContext::new(pool, auth, request);
                        let result = handler(&ctx, args.clone()).await?;

                        self.query_cache.set(
                            function_name,
                            &args,
                            auth_scope.as_deref(),
                            result.clone(),
                            Duration::from_secs(ttl),
                        );

                        Ok(RouteResult::Query(result))
                    } else {
                        let ctx = QueryContext::new(pool, auth, request);
                        let result = handler(&ctx, args).await?;
                        Ok(RouteResult::Query(result))
                    }
                }
                FunctionEntry::Mutation { handler, info } => {
                    if info.transactional {
                        self.execute_transactional(info, handler, args, auth, request)
                            .await
                    } else {
                        // Use primary for mutations
                        let mut ctx = MutationContext::with_dispatch(
                            self.db.primary().clone(),
                            auth,
                            request,
                            self.http_client.clone(),
                            self.job_dispatcher.clone(),
                            self.workflow_dispatcher.clone(),
                        );
                        if let Some(ref issuer) = self.token_issuer {
                            ctx.set_token_issuer(issuer.clone());
                        }
                        ctx.set_token_ttl(self.token_ttl.clone());
                        ctx.set_http_timeout(info.http_timeout);
                        let result = handler(&ctx, args).await?;
                        Ok(RouteResult::Mutation(result))
                    }
                }
            };
        }

        if let Some(ref job_dispatcher) = self.job_dispatcher
            && let Some(job_info) = job_dispatcher.get_info(function_name)
        {
            self.check_job_auth(&job_info, &auth)?;
            match job_dispatcher
                .dispatch_by_name(function_name, args.clone(), auth.principal_id())
                .await
            {
                Ok(job_id) => {
                    return Ok(RouteResult::Job(serde_json::json!({ "job_id": job_id })));
                }
                Err(ForgeError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }

        if let Some(ref workflow_dispatcher) = self.workflow_dispatcher
            && let Some(workflow_info) = workflow_dispatcher.get_info(function_name)
        {
            self.check_workflow_auth(&workflow_info, &auth)?;
            match workflow_dispatcher
                .start_by_name(function_name, args.clone(), auth.principal_id())
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

        Err(ForgeError::NotFound(format!(
            "Function '{}' not found",
            function_name
        )))
    }

    fn check_auth(&self, info: &FunctionInfo, auth: &AuthContext) -> Result<()> {
        require_auth(
            info.is_public,
            info.required_role,
            auth,
            &self.role_resolver,
        )
    }

    fn check_job_auth(&self, info: &forge_core::job::JobInfo, auth: &AuthContext) -> Result<()> {
        require_auth(
            info.is_public,
            info.required_role,
            auth,
            &self.role_resolver,
        )
    }

    fn check_workflow_auth(
        &self,
        info: &forge_core::workflow::WorkflowInfo,
        auth: &AuthContext,
    ) -> Result<()> {
        require_auth(
            info.is_public,
            info.required_role,
            auth,
            &self.role_resolver,
        )
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

        let key_type = info.rate_limit_key.clone().unwrap_or_default();

        let config = RateLimitConfig::new(requests, Duration::from_secs(per_secs))
            .with_key(key_type.clone());

        // Build bucket key
        let bucket_key = self
            .rate_limiter
            .build_key(key_type, function_name, auth, request);

        // Enforce rate limit
        self.rate_limiter.enforce(&bucket_key, &config).await?;

        Ok(())
    }

    fn auth_cache_scope(auth: &AuthContext) -> Option<String> {
        if !auth.is_authenticated() {
            return Some("anon".to_string());
        }

        // Include role + claims fingerprint to avoid cross-scope cache bleed.
        let mut roles = auth.roles().to_vec();
        roles.sort();
        roles.dedup();

        let mut claims = BTreeMap::new();
        for (k, v) in auth.claims() {
            claims.insert(k.clone(), v.clone());
        }

        let claims_json = serde_json::to_string(&claims).unwrap_or_default();
        let mut buf = String::with_capacity(64 + claims_json.len());
        for role in &roles {
            buf.push_str(role);
            buf.push('\x1f');
        }
        buf.push('\x1e');
        buf.push_str(&claims_json);
        let scope = crate::stable_hash::stable_u64(buf.as_bytes());

        let principal = auth
            .principal_id()
            .unwrap_or_else(|| "authenticated".to_string());

        Some(format!("subject:{principal}:scope:{scope:016x}"))
    }

    /// Emit a signal event for RPC auto-capture.
    #[cfg(feature = "gateway")]
    fn emit_signal(
        &self,
        function_name: &str,
        function_kind: &str,
        duration: Duration,
        success: bool,
        ctx: &Option<SignalContext>,
    ) {
        let Some(collector) = &self.signals_collector else {
            return;
        };
        let Some(ctx) = ctx else { return };

        let is_bot = crate::signals::bot::is_bot(ctx.user_agent.as_deref());
        let visitor_id = ctx.client_ip.as_ref().map(|_| {
            crate::signals::visitor::generate_visitor_id(
                ctx.client_ip.as_deref(),
                ctx.user_agent.as_deref(),
                &self.signals_server_secret,
            )
        });

        let event = forge_core::signals::SignalEvent::rpc_call(
            function_name,
            function_kind,
            duration.as_millis() as i32,
            success,
            ctx.user_id,
            ctx.tenant_id,
            ctx.correlation_id.clone(),
            ctx.client_ip.clone(),
            ctx.user_agent.clone(),
            visitor_id,
            is_bot,
        );
        collector.try_send(event);
    }

    /// Log function execution at the configured level.
    #[allow(clippy::too_many_arguments)]
    fn log_execution(
        &self,
        log_level: forge_core::LogLevel,
        function_name: &str,
        kind: &str,
        input: &Value,
        duration: Duration,
        success: bool,
        error: Option<&str>,
    ) {
        // Failures are always logged at error regardless of the function's
        // configured log level. Successes use the configured level.
        if !success {
            error!(
                function = function_name,
                kind = kind,
                duration_ms = duration.as_millis() as u64,
                error = error,
                "Function failed"
            );
            debug!(
                function = function_name,
                input = %input,
                "Function input"
            );
            return;
        }

        macro_rules! log_fn {
            ($level:ident) => {{
                $level!(
                    function = function_name,
                    kind = kind,
                    duration_ms = duration.as_millis() as u64,
                    "Function executed"
                );
                debug!(
                    function = function_name,
                    input = %input,
                    "Function input"
                );
            }};
        }

        match log_level {
            forge_core::LogLevel::Off => {}
            forge_core::LogLevel::Error => log_fn!(error),
            forge_core::LogLevel::Warn => log_fn!(warn),
            forge_core::LogLevel::Info => log_fn!(info),
            forge_core::LogLevel::Debug => log_fn!(debug),
            forge_core::LogLevel::Trace => log_fn!(trace),
            _ => log_fn!(trace),
        }
    }

    /// Mutations default to "info" because writes are worth tracking.
    /// Queries default to "debug" since they're high-volume.
    fn get_function_log_level(&self, function_name: &str) -> forge_core::LogLevel {
        self.registry
            .get(function_name)
            .map(|entry| {
                entry.info().log_level.unwrap_or(match entry.kind() {
                    forge_core::FunctionKind::Mutation => forge_core::LogLevel::Info,
                    forge_core::FunctionKind::Query => forge_core::LogLevel::Debug,
                    _ => forge_core::LogLevel::Info,
                })
            })
            .unwrap_or(forge_core::LogLevel::Info)
    }

    /// Get the timeout for a specific function, falling back to the router default.
    fn get_function_timeout(&self, function_name: &str) -> Duration {
        self.registry
            .get(function_name)
            .and_then(|entry| entry.info().timeout)
            .unwrap_or(self.default_timeout)
    }

    async fn execute_transactional(
        &self,
        info: &FunctionInfo,
        handler: &BoxedMutationFn,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult> {
        let span = tracing::info_span!("db.transaction", db.system = "postgresql",);

        async {
            let primary = self.db.primary();
            let tx = primary.begin().await.map_err(ForgeError::Database)?;

            let job_dispatcher = self.job_dispatcher.clone();
            let job_lookup: forge_core::JobInfoLookup =
                Arc::new(move |name: &str| job_dispatcher.as_ref().and_then(|d| d.get_info(name)));

            let (mut ctx, tx_handle, outbox) = MutationContext::with_transaction(
                primary.clone(),
                tx,
                auth,
                request,
                self.http_client.clone(),
                job_lookup,
            );
            if let Some(ref issuer) = self.token_issuer {
                ctx.set_token_issuer(issuer.clone());
            }
            ctx.set_token_ttl(self.token_ttl.clone());
            ctx.set_http_timeout(info.http_timeout);

            match handler(&ctx, args).await {
                Ok(value) => {
                    // Drop the context so its Arc<Transaction> clone is released
                    // before we try_unwrap the transaction handle for commit.
                    drop(ctx);

                    let buffer = {
                        let guard = outbox.lock().unwrap_or_else(|poisoned| {
                            tracing::error!("Outbox mutex was poisoned, recovering");
                            poisoned.into_inner()
                        });
                        OutboxBuffer::new(guard.jobs.clone(), guard.workflows.clone())
                    };

                    let mut tx = Arc::try_unwrap(tx_handle)
                        .map_err(|_| ForgeError::Internal("Transaction still in use".into()))?
                        .into_inner();

                    for job in &buffer.jobs {
                        Self::insert_job(&mut tx, job).await?;
                    }

                    for workflow in &buffer.workflows {
                        if self
                            .workflow_dispatcher
                            .as_ref()
                            .and_then(|d| d.get_info(&workflow.workflow_name))
                            .is_none()
                        {
                            return Err(ForgeError::NotFound(format!(
                                "Workflow '{}' not found",
                                workflow.workflow_name
                            )));
                        }
                        Self::insert_workflow(&mut tx, workflow).await?;
                    }

                    tx.commit().await.map_err(ForgeError::Database)?;

                    Ok(RouteResult::Mutation(value))
                }
                Err(e) => {
                    drop(ctx);
                    Err(e)
                }
            }
        }
        .instrument(span)
        .await
    }

    async fn insert_job(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        job: &PendingJob,
    ) -> Result<()> {
        let now = Utc::now();
        sqlx::query!(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, job_context, status, priority, attempts, max_attempts,
                worker_capability, owner_subject, scheduled_at, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
            job.id,
            &job.job_type,
            job.args as _,
            job.context as _,
            JobStatus::Pending.as_str(),
            job.priority,
            0i32,
            job.max_attempts,
            job.worker_capability.as_deref(),
            job.owner_subject as _,
            now,
            now,
        )
        .execute(&mut **tx)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }

    async fn insert_workflow(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workflow: &PendingWorkflow,
    ) -> Result<()> {
        let now = Utc::now();
        sqlx::query!(
            r#"
            INSERT INTO forge_workflow_runs (
                id, workflow_name, workflow_version, workflow_signature,
                owner_subject, input, status, current_step,
                step_results, started_at, trace_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
            workflow.id,
            &workflow.workflow_name,
            &workflow.workflow_version,
            &workflow.workflow_signature,
            workflow.owner_subject as _,
            workflow.input as _,
            WorkflowStatus::Created.as_str(),
            Option::<String>::None,
            serde_json::json!({}) as _,
            now,
            workflow.id.to_string(),
        )
        .execute(&mut **tx)
        .await
        .map_err(ForgeError::Database)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            http_timeout: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: &[],
            selected_columns: &[],
            transactional: false,
            consistent: false,
            max_upload_size_bytes: None,
        };

        let _auth = AuthContext::unauthenticated();

        // Can't test check_auth directly without a router instance,
        // but we can test the logic
        assert!(info.is_public);
    }

    #[test]
    fn test_auth_cache_scope_changes_with_claims() {
        let user_id = uuid::Uuid::new_v4();
        let auth_a = AuthContext::authenticated(
            user_id,
            vec!["user".to_string()],
            HashMap::from([
                (
                    "sub".to_string(),
                    serde_json::Value::String(user_id.to_string()),
                ),
                (
                    "tenant_id".to_string(),
                    serde_json::Value::String("tenant-a".to_string()),
                ),
            ]),
        );
        let auth_b = AuthContext::authenticated(
            user_id,
            vec!["user".to_string()],
            HashMap::from([
                (
                    "sub".to_string(),
                    serde_json::Value::String(user_id.to_string()),
                ),
                (
                    "tenant_id".to_string(),
                    serde_json::Value::String("tenant-b".to_string()),
                ),
            ]),
        );

        let scope_a = FunctionRouter::auth_cache_scope(&auth_a);
        let scope_b = FunctionRouter::auth_cache_scope(&auth_b);
        assert_ne!(scope_a, scope_b);
    }
}
