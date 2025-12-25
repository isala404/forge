use std::sync::Arc;
use std::time::Duration;

use forge_core::{AuthContext, ForgeError, RequestMetadata, Result};
use serde_json::Value;
use tokio::time::timeout;

use super::registry::FunctionRegistry;
use super::router::{FunctionRouter, RouteResult};

/// Executes functions with timeout and error handling.
pub struct FunctionExecutor {
    router: FunctionRouter,
    default_timeout: Duration,
}

impl FunctionExecutor {
    /// Create a new function executor.
    pub fn new(registry: Arc<FunctionRegistry>, db_pool: sqlx::PgPool) -> Self {
        Self {
            router: FunctionRouter::new(registry, db_pool),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Create a new function executor with custom timeout.
    pub fn with_timeout(
        registry: Arc<FunctionRegistry>,
        db_pool: sqlx::PgPool,
        default_timeout: Duration,
    ) -> Self {
        Self {
            router: FunctionRouter::new(registry, db_pool),
            default_timeout,
        }
    }

    /// Execute a function call.
    pub async fn execute(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<ExecutionResult> {
        let start = std::time::Instant::now();

        // Get function-specific timeout or use default
        let fn_timeout = self.get_function_timeout(function_name);

        // Execute with timeout
        let result = match timeout(
            fn_timeout,
            self.router.route(function_name, args, auth, request),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                return Err(ForgeError::Timeout(format!(
                    "Function '{}' timed out after {:?}",
                    function_name, fn_timeout
                )));
            }
        };

        let duration = start.elapsed();

        match result {
            Ok(route_result) => {
                let (kind, value) = match route_result {
                    RouteResult::Query(v) => ("query", v),
                    RouteResult::Mutation(v) => ("mutation", v),
                    RouteResult::Action(v) => ("action", v),
                };

                Ok(ExecutionResult {
                    function_name: function_name.to_string(),
                    function_kind: kind.to_string(),
                    result: value,
                    duration,
                    success: true,
                    error: None,
                })
            }
            Err(e) => Ok(ExecutionResult {
                function_name: function_name.to_string(),
                function_kind: self
                    .router
                    .get_function_kind(function_name)
                    .map(|k| k.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                result: Value::Null,
                duration,
                success: false,
                error: Some(e.to_string()),
            }),
        }
    }

    /// Get the timeout for a specific function.
    fn get_function_timeout(&self, _function_name: &str) -> Duration {
        // TODO: Look up function-specific timeout from registry
        self.default_timeout
    }

    /// Check if a function exists.
    pub fn has_function(&self, function_name: &str) -> bool {
        self.router.has_function(function_name)
    }
}

/// Result of executing a function.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionResult {
    /// Function name that was executed.
    pub function_name: String,
    /// Kind of function (query, mutation, action).
    pub function_kind: String,
    /// The result value (or null on error).
    pub result: Value,
    /// Execution duration.
    #[serde(with = "duration_millis")]
    pub duration: Duration,
    /// Whether execution succeeded.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
}

mod duration_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_result_serialization() {
        let result = ExecutionResult {
            function_name: "get_user".to_string(),
            function_kind: "query".to_string(),
            result: serde_json::json!({"id": "123"}),
            duration: Duration::from_millis(42),
            success: true,
            error: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"duration\":42"));
        assert!(json.contains("\"success\":true"));
    }
}
