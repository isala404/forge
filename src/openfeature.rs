//! Official OpenFeature provider and application-scoped telemetry hook.

use crate::{EvalCtx, FlagEvaluation, Forge};
use ::open_feature::provider::{FeatureProvider, ProviderMetadata, ResolutionDetails};
use ::open_feature::{
    EvaluationContext, EvaluationDetails, EvaluationError, EvaluationErrorCode, EvaluationReason,
    EvaluationResult, Hook, HookContext, HookHints, StructValue, Value,
};

/// OpenFeature provider over an application-owned Forge handle.
///
/// It registers no provider, hook, or global evaluation context. Applications choose those scopes
/// through the official SDK.
pub struct ForgeProvider {
    forge: Forge,
    metadata: ProviderMetadata,
}

impl ForgeProvider {
    /// Wrap an initialized Forge handle.
    pub fn new(forge: Forge) -> Self {
        Self {
            forge,
            metadata: ProviderMetadata::new("forge"),
        }
    }

    async fn evaluate(
        &self,
        key: &str,
        default: serde_json::Value,
        context: &EvaluationContext,
    ) -> EvaluationResult<FlagEvaluation> {
        let context = context
            .targeting_key
            .as_ref()
            .map_or_else(EvalCtx::new, |key| EvalCtx::user(key.clone()));
        let details = self
            .forge
            .config()
            .flag_details(key, &default, &context)
            .await;
        match details.reason.as_str() {
            "default_missing" => Err(evaluation_error(
                EvaluationErrorCode::FlagNotFound,
                "flag was not found",
            )),
            "default_no_key" => Err(evaluation_error(
                EvaluationErrorCode::TargetingKeyMissing,
                "flag requires a targeting key",
            )),
            _ if details.error_code.is_some() => Err(evaluation_error(
                EvaluationErrorCode::General("GENERAL".into()),
                "Forge evaluation failed",
            )),
            _ => Ok(details),
        }
    }
}

#[::open_feature::async_trait]
impl FeatureProvider for ForgeProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn resolve_bool_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<bool>> {
        let details = self
            .evaluate(flag_key, serde_json::Value::Bool(false), evaluation_context)
            .await?;
        let value = serde_json::from_str::<bool>(&details.value_json)
            .map_err(|_| type_mismatch("flag value is not boolean"))?;
        Ok(resolution(value, details))
    }

    async fn resolve_int_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<i64>> {
        let details = self
            .evaluate(flag_key, serde_json::Value::from(0), evaluation_context)
            .await?;
        let value = serde_json::from_str::<i64>(&details.value_json)
            .map_err(|_| type_mismatch("flag value is not an integer"))?;
        Ok(resolution(value, details))
    }

    async fn resolve_float_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<f64>> {
        let details = self
            .evaluate(flag_key, serde_json::Value::from(0.0), evaluation_context)
            .await?;
        let value = serde_json::from_str::<f64>(&details.value_json)
            .map_err(|_| type_mismatch("flag value is not a float"))?;
        if details.value_type != "float" {
            return Err(type_mismatch("flag value is not a float"));
        }
        Ok(resolution(value, details))
    }

    async fn resolve_string_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<String>> {
        let details = self
            .evaluate(
                flag_key,
                serde_json::Value::String(String::new()),
                evaluation_context,
            )
            .await?;
        let value = serde_json::from_str::<String>(&details.value_json)
            .map_err(|_| type_mismatch("flag value is not a string"))?;
        Ok(resolution(value, details))
    }

    async fn resolve_struct_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<StructValue>> {
        let details = self
            .evaluate(flag_key, serde_json::json!({}), evaluation_context)
            .await?;
        let json: serde_json::Value = serde_json::from_str(&details.value_json)
            .map_err(|_| type_mismatch("flag value is not an object"))?;
        let Value::Struct(value) = Value::try_from(json)? else {
            return Err(type_mismatch("flag value is not an object"));
        };
        Ok(resolution(value, details))
    }
}

fn resolution<T>(value: T, details: FlagEvaluation) -> ResolutionDetails<T> {
    ResolutionDetails {
        value,
        variant: details.variant,
        reason: Some(reason(&details.reason)),
        flag_metadata: None,
    }
}

fn reason(value: &str) -> EvaluationReason {
    match value {
        "static" => EvaluationReason::Static,
        "percent_in" | "percent_out" => EvaluationReason::Split,
        "targeting_match" | "targeting_miss" => EvaluationReason::TargetingMatch,
        "default_error" | "default_closed" => EvaluationReason::Error,
        _ => EvaluationReason::Default,
    }
}

fn type_mismatch(message: &str) -> EvaluationError {
    evaluation_error(EvaluationErrorCode::TypeMismatch, message)
}

fn evaluation_error(code: EvaluationErrorCode, message: &str) -> EvaluationError {
    EvaluationError {
        code,
        message: Some(message.into()),
    }
}

/// Application-scoped hook that emits OpenTelemetry semantic-convention evaluation span events
/// through the configured `tracing` OpenTelemetry layer.
#[derive(Debug, Default)]
pub struct OpenTelemetryHook;

#[::open_feature::async_trait]
impl Hook for OpenTelemetryHook {
    async fn before<'a>(
        &self,
        _context: &HookContext<'a>,
        _hints: Option<&'a HookHints>,
    ) -> Result<Option<EvaluationContext>, EvaluationError> {
        Ok(None)
    }

    async fn after<'a>(
        &self,
        _context: &HookContext<'a>,
        _details: &EvaluationDetails<Value>,
        _hints: Option<&'a HookHints>,
    ) -> Result<(), EvaluationError> {
        Ok(())
    }

    async fn error<'a>(
        &self,
        context: &HookContext<'a>,
        error: &EvaluationError,
        _hints: Option<&'a HookHints>,
    ) {
        emit_event(
            context,
            context.default_value.as_ref(),
            &EvaluationReason::Error,
            None,
            Some(error.code.to_string()),
        );
    }

    async fn finally<'a>(
        &self,
        context: &HookContext<'a>,
        details: &EvaluationDetails<Value>,
        _hints: Option<&'a HookHints>,
    ) {
        if details.reason == Some(EvaluationReason::Error) {
            return;
        }
        emit_event(
            context,
            Some(&details.value),
            details
                .reason
                .as_ref()
                .unwrap_or(&EvaluationReason::Unknown),
            details.variant.as_deref(),
            None,
        );
    }
}

fn emit_event(
    context: &HookContext<'_>,
    value: Option<&Value>,
    reason: &EvaluationReason,
    variant: Option<&str>,
    error_type: Option<String>,
) {
    let value = value.map(value_json).unwrap_or_default();
    tracing::event!(
        name: "feature_flag.evaluation",
        tracing::Level::INFO,
        feature_flag.key = %context.flag_key,
        feature_flag.provider.name = %context.provider_metadata.name,
        feature_flag.result.value = %value,
        feature_flag.result.variant = variant.unwrap_or_default(),
        feature_flag.result.reason = %reason.to_string().to_ascii_lowercase(),
        feature_flag.context.id = context.evaluation_context.targeting_key.as_deref().unwrap_or_default(),
        error.type = error_type.as_deref().unwrap_or_default(),
    );
}

fn value_json(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => serde_json::Value::String(value.clone()).to_string(),
        Value::Array(values) => {
            let values: Vec<serde_json::Value> = values.iter().map(value_to_json).collect();
            serde_json::Value::Array(values).to_string()
        }
        Value::Struct(value) => serde_json::Value::Object(
            value
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        )
        .to_string(),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::from(*value),
        Value::Float(value) => serde_json::Value::from(*value),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(value_to_json).collect())
        }
        Value::Struct(value) => serde_json::Value::Object(
            value
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), value_to_json(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlagRule;
    use std::time::UNIX_EPOCH;

    #[tokio::test]
    async fn official_provider_returns_typed_details_without_hooks() {
        let forge = Forge::init_memory_for_testing(
            "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n",
            UNIX_EPOCH,
            1,
        )
        .await
        .expect("memory Forge");
        forge
            .config()
            .set_flag(
                "theme",
                FlagRule::Value {
                    value: serde_json::json!("dark"),
                    variant: "theme-v1".into(),
                },
            )
            .await
            .expect("set flag");
        let provider = ForgeProvider::new(forge);
        let details = provider
            .resolve_string_value(
                "theme",
                &EvaluationContext::default().with_targeting_key("user-1"),
            )
            .await
            .expect("evaluate");
        assert_eq!(details.value, "dark");
        assert_eq!(details.variant.as_deref(), Some("theme-v1"));
        assert_eq!(details.reason, Some(EvaluationReason::Static));
        assert!(provider.hooks().is_empty());
    }
}
