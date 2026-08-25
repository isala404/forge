use crate::{ForgeError, Result};
use std::collections::HashSet;

pub const MAX_TRACEPARENT_BYTES: usize = 512;
pub const MAX_TRACESTATE_BYTES: usize = 512;
pub const MAX_BAGGAGE_BYTES: usize = 1024;
pub const MAX_BAGGAGE_ITEMS: usize = 16;

/// Reserved W3C context carried separately from queue payloads. Baggage is filtered
/// through an explicit allow-list when this value is constructed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TraceContext {
    traceparent: String,
    tracestate: Option<String>,
    baggage: Option<String>,
}

impl TraceContext {
    pub fn from_headers(
        traceparent: impl Into<String>,
        tracestate: Option<String>,
        baggage: Option<String>,
        baggage_allowlist: &[String],
    ) -> Result<Self> {
        let traceparent = traceparent.into();
        validate_traceparent(&traceparent)?;
        let tracestate = tracestate
            .map(|value| validate_ascii_header("tracestate", value, MAX_TRACESTATE_BYTES))
            .transpose()?;
        let baggage = filter_baggage(baggage, baggage_allowlist)?;
        Ok(Self {
            traceparent,
            tracestate,
            baggage,
        })
    }

    pub fn traceparent(&self) -> &str {
        &self.traceparent
    }

    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }

    pub fn baggage(&self) -> Option<&str> {
        self.baggage.as_deref()
    }

    pub fn headers(&self) -> Vec<(&'static str, &str)> {
        let mut headers = vec![("traceparent", self.traceparent.as_str())];
        if let Some(value) = &self.tracestate {
            headers.push(("tracestate", value));
        }
        if let Some(value) = &self.baggage {
            headers.push(("baggage", value));
        }
        headers
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_traceparent(&self.traceparent)?;
        if let Some(value) = &self.tracestate {
            validate_ascii_header("tracestate", value.clone(), MAX_TRACESTATE_BYTES)?;
        }
        if let Some(value) = &self.baggage {
            validate_ascii_header("baggage", value.clone(), MAX_BAGGAGE_BYTES)?;
        }
        Ok(())
    }

    /// Capture the current OpenTelemetry context for a queue producer. Returns `None`
    /// when the current span has no valid remote-propagatable context.
    #[cfg(feature = "otel")]
    pub fn capture_current(baggage_allowlist: &[String]) -> Result<Option<Self>> {
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        let context = tracing::Span::current().context();
        let mut carrier = std::collections::HashMap::new();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut carrier);
        });
        let Some(traceparent) = carrier.remove("traceparent") else {
            return Ok(None);
        };
        Self::from_headers(
            traceparent,
            carrier.remove("tracestate"),
            carrier.remove("baggage"),
            baggage_allowlist,
        )
        .map(Some)
    }

    #[cfg(feature = "otel")]
    pub(crate) fn apply_to_span(&self, span: &tracing::Span, link: bool) {
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        let carrier: std::collections::HashMap<String, String> = self
            .headers()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let context = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&carrier)
        });
        if link {
            span.add_link(context.span().span_context().clone());
        } else {
            let _ = span.set_parent(context);
        }
    }
}

fn validate_traceparent(value: &str) -> Result<()> {
    if value.len() > MAX_TRACEPARENT_BYTES || !value.is_ascii() || value.contains(['\r', '\n']) {
        return Err(ForgeError::invalid("traceparent is malformed"));
    }
    let mut parts = value.split('-');
    let version = parts.next();
    let trace_id = parts.next();
    let parent_id = parts.next();
    let flags = parts.next();
    if parts.next().is_some()
        || version.is_none_or(|part| part.len() != 2 || !is_lower_hex(part) || part == "ff")
        || trace_id.is_none_or(|part| {
            part.len() != 32 || !is_lower_hex(part) || part.bytes().all(|byte| byte == b'0')
        })
        || parent_id.is_none_or(|part| {
            part.len() != 16 || !is_lower_hex(part) || part.bytes().all(|byte| byte == b'0')
        })
        || flags.is_none_or(|part| part.len() != 2 || !is_lower_hex(part))
    {
        return Err(ForgeError::invalid("traceparent is malformed"));
    }
    Ok(())
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_ascii_header(name: &str, value: String, max: usize) -> Result<String> {
    if value.len() > max || !value.is_ascii() || value.contains(['\r', '\n']) {
        return Err(ForgeError::invalid(format!("{name} is malformed")));
    }
    Ok(value)
}

fn filter_baggage(value: Option<String>, allowlist: &[String]) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    validate_ascii_header("baggage", value.clone(), MAX_BAGGAGE_BYTES)?;
    let allowed: HashSet<&str> = allowlist.iter().map(String::as_str).collect();
    let mut kept = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let Some((key, _)) = item.split_once('=') else {
            return Err(ForgeError::invalid("baggage is malformed"));
        };
        if allowed.contains(key.trim()) {
            kept.push(item);
            if kept.len() > MAX_BAGGAGE_ITEMS {
                return Err(ForgeError::limit("baggage has too many allowed items"));
            }
        }
    }
    if kept.is_empty() {
        return Ok(None);
    }
    let filtered = kept.join(",");
    if filtered.len() > MAX_BAGGAGE_BYTES {
        return Err(ForgeError::limit("allowed baggage is too large"));
    }
    Ok(Some(filtered))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn validates_and_filters_w3c_context() {
        let context = TraceContext::from_headers(
            PARENT,
            Some("vendor=value".to_string()),
            Some("tenant=kept,secret=dropped".to_string()),
            &["tenant".to_string()],
        )
        .expect("valid context");
        assert_eq!(context.traceparent(), PARENT);
        assert_eq!(context.baggage(), Some("tenant=kept"));
        assert!(!format!("{context:?}").contains("secret=dropped"));
    }

    #[test]
    fn rejects_invalid_trace_ids_and_header_injection() {
        assert!(TraceContext::from_headers("00-0-0-00", None, None, &[]).is_err());
        assert!(
            TraceContext::from_headers(PARENT, Some("x=y\r\nsecret=z".into()), None, &[]).is_err()
        );
    }
}
