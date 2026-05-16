use std::sync::Arc;
use std::time::Duration;

use forge_core::{
    AuthContext, CircuitBreakerClient, ForgeError, FunctionInfo, FunctionKind, JobDispatch,
    MutationContext, QueryContext, RequestMetadata, Result, SharedRoleResolver, WorkflowDispatch,
    default_role_resolver,
    rate_limit::{RateLimitConfig, RateLimiterBackend},
};
use serde_json::Value;
use tokio::time::timeout;
use tracing::Instrument;

use super::cache::QueryCacheCoordinator;
use super::execution_log::{level_for as log_level_for, log_completion};
use super::registry::{BoxedMutationFn, FunctionEntry, FunctionRegistry};
#[cfg(feature = "gateway")]
use super::rpc_signals::{RpcSignalContext, RpcSignalsEmitter};
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

/// Result of routing a function call paired with telemetry the executor
/// wants to forward to spans/metrics. The cache flag is meaningful only for
/// queries; every other variant returns `cache_hit = false`.
pub struct RouteOutcome {
    pub result: RouteResult,
    pub cache_hit: bool,
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
    cache: Arc<QueryCacheCoordinator>,
    token_issuer: Option<Arc<dyn forge_core::TokenIssuer>>,
    token_ttl: forge_core::AuthTokenTtl,
    default_timeout: Duration,
    #[cfg(feature = "gateway")]
    signals: Option<RpcSignalsEmitter>,
}

impl FunctionRouter {
    /// Create a new function router.
    pub fn new(registry: Arc<FunctionRegistry>, db: Database) -> Self {
        Self::with_http_client(
            registry,
            db,
            CircuitBreakerClient::with_defaults(reqwest::Client::new()),
        )
    }

    /// Create a new function router with a custom HTTP client.
    pub fn with_http_client(
        registry: Arc<FunctionRegistry>,
        db: Database,
        http_client: CircuitBreakerClient,
    ) -> Self {
        let rate_limiter: Arc<dyn RateLimiterBackend> =
            Arc::new(HybridRateLimiter::new(db.primary().clone()));
        let cache = Arc::new(QueryCacheCoordinator::new(&registry));
        Self {
            registry,
            db,
            http_client,
            job_dispatcher: None,
            workflow_dispatcher: None,
            rate_limiter,
            role_resolver: default_role_resolver(),
            cache,
            token_issuer: None,
            token_ttl: forge_core::AuthTokenTtl::default(),
            default_timeout: Duration::from_secs(30),
            #[cfg(feature = "gateway")]
            signals: None,
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
        self.signals = Some(RpcSignalsEmitter::new(collector, server_secret));
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
        let info = self.registry.get(function_name).map(|e| e.info());
        let fn_timeout = info.and_then(|i| i.timeout).unwrap_or(self.default_timeout);
        let log_level = log_level_for(info);

        let kind = info
            .map(|i| i.kind.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Capture signal metadata before auth/request are consumed.
        #[cfg(feature = "gateway")]
        let signal_ctx = self
            .signals
            .as_ref()
            .map(|_| RpcSignalContext::capture(&auth, &request));

        // Declare cache.hit as Empty so the inner cache branch can fill it
        // via Span::current().record(...). Latency p99 reported for this span
        // is then attributable to either real handler work or a pure cache
        // round-trip without ambiguity.
        let span = tracing::info_span!(
            "fn.execute",
            function = function_name,
            fn.kind = %kind,
            cache.hit = tracing::field::Empty,
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
                log_completion(
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
                    false,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                if let (Some(emitter), Some(ctx)) = (&self.signals, &signal_ctx) {
                    emitter.emit(function_name, &kind, duration, false, ctx);
                }
                return Err(ForgeError::Timeout(format!(
                    "Function '{}' timed out after {:?}",
                    function_name, fn_timeout
                )));
            }
        };

        let duration = start.elapsed();

        match result {
            Ok(outcome) => {
                let RouteOutcome { result, cache_hit } = outcome;
                let (result_kind, value) = match result {
                    RouteResult::Query(v) => ("query", v),
                    RouteResult::Mutation(v) => ("mutation", v),
                    RouteResult::Job(v) => ("job", v),
                    RouteResult::Workflow(v) => ("workflow", v),
                };

                log_completion(
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
                    cache_hit,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                if let (Some(emitter), Some(ctx)) = (&self.signals, &signal_ctx) {
                    emitter.emit(function_name, result_kind, duration, true, ctx);
                }

                Ok(value)
            }
            Err(e) => {
                log_completion(
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
                    false,
                    duration.as_secs_f64(),
                );
                #[cfg(feature = "gateway")]
                if let (Some(emitter), Some(ctx)) = (&self.signals, &signal_ctx) {
                    emitter.emit(function_name, &kind, duration, false, ctx);
                }

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

    /// Return info for all registered query and mutation functions.
    pub fn function_infos(&self) -> Vec<FunctionInfo> {
        self.registry
            .functions()
            .map(|(_, entry)| entry.info().clone())
            .collect()
    }

    /// Shared handle to the query cache coordinator (used to wire cluster invalidation).
    pub fn cache(&self) -> Arc<QueryCacheCoordinator> {
        Arc::clone(&self.cache)
    }

    pub async fn route(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteOutcome> {
        if let Some(entry) = self.registry.get(function_name) {
            let info = entry.info();
            require_auth(
                info.is_public,
                info.required_role,
                &auth,
                &self.role_resolver,
            )?;
            self.check_rate_limit(info, function_name, &auth, &request)
                .await?;

            return match entry {
                FunctionEntry::Webhook { info } => {
                    // Webhooks are registered in the function registry for
                    // metadata access only. They must be called via their
                    // dedicated HTTP path which performs signature validation.
                    return Err(ForgeError::InvalidArgument(format!(
                        "Webhook '{}' cannot be called via RPC; use its dedicated HTTP endpoint",
                        info.name
                    )));
                }
                FunctionEntry::Query { handler, info, .. } => {
                    let pool = if info.consistent {
                        self.db.primary().clone()
                    } else {
                        self.db.read_pool().clone()
                    };

                    if let Some(ttl) = info.cache_ttl {
                        // Derive scope once, before auth is moved into ctx, so
                        // get/set agree on the same cache key.
                        let scope = QueryCacheCoordinator::auth_scope(&auth);
                        if let Some(cached) =
                            self.cache
                                .get_by_scope(function_name, &args, scope.as_deref())
                        {
                            tracing::Span::current().record("cache.hit", true);
                            crate::observability::record_fn_cache(function_name, true);
                            return Ok(RouteOutcome {
                                result: RouteResult::Query(Value::clone(&cached)),
                                cache_hit: true,
                            });
                        }
                        tracing::Span::current().record("cache.hit", false);
                        crate::observability::record_fn_cache(function_name, false);

                        let ctx = QueryContext::new(pool, auth, request);
                        let result = handler(&ctx, args.clone()).await?;

                        self.cache.set_by_scope(
                            function_name,
                            &args,
                            scope.as_deref(),
                            result.clone(),
                            Duration::from_secs(ttl),
                        );

                        Ok(RouteOutcome {
                            result: RouteResult::Query(result),
                            cache_hit: false,
                        })
                    } else {
                        let ctx = QueryContext::new(pool, auth, request);
                        let result = handler(&ctx, args).await?;
                        Ok(RouteOutcome {
                            result: RouteResult::Query(result),
                            cache_hit: false,
                        })
                    }
                }
                FunctionEntry::Mutation { handler, info } => {
                    let result = if info.transactional {
                        self.execute_transactional(info, handler, args, auth, request)
                            .await
                    } else {
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
                        let value = handler(&ctx, args).await?;
                        Ok(RouteResult::Mutation(value))
                    };
                    if result.is_ok() {
                        self.cache.invalidate_for_mutation(info);
                    }
                    result.map(|r| RouteOutcome {
                        result: r,
                        cache_hit: false,
                    })
                }
            };
        }

        if let Some(ref job_dispatcher) = self.job_dispatcher
            && let Some(job_info) = job_dispatcher.get_info(function_name)
        {
            require_auth(
                job_info.is_public,
                job_info.required_role,
                &auth,
                &self.role_resolver,
            )?;
            match job_dispatcher
                .dispatch_by_name(function_name, args.clone(), auth.principal_id())
                .await
            {
                Ok(job_id) => {
                    return Ok(RouteOutcome {
                        result: RouteResult::Job(serde_json::json!({ "job_id": job_id })),
                        cache_hit: false,
                    });
                }
                Err(ForgeError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }

        if let Some(ref workflow_dispatcher) = self.workflow_dispatcher
            && let Some(workflow_info) = workflow_dispatcher.get_info(function_name)
        {
            require_auth(
                workflow_info.is_public,
                workflow_info.required_role,
                &auth,
                &self.role_resolver,
            )?;
            match workflow_dispatcher
                .start_by_name(
                    function_name,
                    args.clone(),
                    auth.principal_id(),
                    Some(request.trace_id().to_string()),
                )
                .await
            {
                Ok(workflow_id) => {
                    return Ok(RouteOutcome {
                        result: RouteResult::Workflow(
                            serde_json::json!({ "workflow_id": workflow_id }),
                        ),
                        cache_hit: false,
                    });
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

    async fn execute_transactional(
        &self,
        info: &FunctionInfo,
        handler: &BoxedMutationFn,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult> {
        let span = tracing::info_span!("db.transaction", db.system = "postgresql",);
        let fn_timeout = info.timeout.unwrap_or(self.default_timeout);

        async {
            let primary = self.db.primary();
            let mut tx = primary.begin().await.map_err(ForgeError::Database)?;

            // Bind the per-function deadline to PostgreSQL via SET LOCAL so
            // PG cancels the in-flight query at the same instant the tokio
            // timeout fires. Without this the connection sits busy until the
            // pool-wide statement_timeout — wasting connections and producing
            // misleading "still running" backends after a 504. SET LOCAL
            // doesn't accept bind parameters, so the value is interpolated
            // directly; it's an integer derived from a Duration so injection
            // is impossible.
            let timeout_ms = fn_timeout.as_millis().min(i64::MAX as u128) as i64;
            #[allow(clippy::disallowed_methods)]
            sqlx::query(&format!("SET LOCAL statement_timeout = {timeout_ms}"))
                .execute(&mut *tx)
                .await
                .map_err(ForgeError::Database)?;

            let (mut ctx, tx_handle) = MutationContext::with_transaction(
                primary.clone(),
                tx,
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

            let result = handler(&ctx, args).await;
            drop(ctx);

            // After dropping ctx, the executor holds the only Arc to the
            // transaction. Take it out via `lock().await.take()` so we never
            // depend on `Arc::try_unwrap` succeeding — even if a handler
            // accidentally retained a clone of the Arc through a destructured
            // DbConn, the take() leaves a None behind that prevents further
            // misuse rather than leaking the transaction.
            let tx = tx_handle.lock().await.take().ok_or_else(|| {
                ForgeError::Internal("Transaction already taken from handle".into())
            })?;

            match result {
                Ok(value) => {
                    tx.commit().await.map_err(ForgeError::Database)?;
                    Ok(RouteResult::Mutation(value))
                }
                Err(e) => {
                    if let Err(rollback_err) = tx.rollback().await {
                        tracing::error!(
                            handler_error = %e,
                            rollback_error = %rollback_err,
                            "Mutation rollback failed; transaction will be released by Drop"
                        );
                    } else {
                        tracing::warn!(
                            handler_error = %e,
                            "Mutation rolled back"
                        );
                    }
                    Err(e)
                }
            }
        }
        .instrument(span)
        .await
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
            changed_columns: &[],
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

        let scope_a = QueryCacheCoordinator::auth_scope(&auth_a);
        let scope_b = QueryCacheCoordinator::auth_scope(&auth_b);
        assert_ne!(scope_a, scope_b);
    }
}
