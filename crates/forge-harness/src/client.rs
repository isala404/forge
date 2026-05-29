use serde::de::DeserializeOwned;
use serde_json::json;

use crate::error::HarnessError;

/// Wire-compatible HTTP client. Matches the exact request shape the SvelteKit
/// (`forge-svelte/client.ts`) and Dioxus (`forge-dioxus/src/client.rs`) clients
/// produce: `POST /_api/rpc/{fn}` with `Accept: application/vnd.forge.v1+json`,
/// JSON body `{ "args": <args> }`, and optional `Authorization: Bearer <token>`.
#[derive(Clone)]
pub struct HarnessClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

/// Raw RPC envelope as returned by the gateway.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RpcEnvelope {
    pub success: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcEnvelopeError>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RpcEnvelopeError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

impl HarnessClient {
    pub(crate) fn new(http: reqwest::Client, base_url: String, token: Option<String>) -> Self {
        Self {
            http,
            base_url,
            token,
        }
    }

    /// Return a copy of this client with the given bearer token.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Drop authentication from this client.
    pub fn anonymous(mut self) -> Self {
        self.token = None;
        self
    }

    /// The bearer token in use, if any.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Base URL the client is configured against.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Invoke an RPC and deserialize the success payload.
    ///
    /// Pass `()` for no-arg functions, `serde_json::json!(...)` for ad-hoc
    /// shapes, or any `Serialize` type for typed args.
    pub async fn call<A, R>(&self, function: &str, args: A) -> Result<R, HarnessError>
    where
        A: serde::Serialize,
        R: DeserializeOwned,
    {
        let (status, envelope) = self.call_raw_with_status(function, args).await?;
        if !envelope.success {
            return Err(envelope_to_error(envelope, status));
        }
        let data = envelope.data.unwrap_or(serde_json::Value::Null);
        Ok(serde_json::from_value(data)?)
    }

    /// Like `call` but returns the full envelope, so tests can assert on
    /// success/error variants without serde shoehorning. Status is captured
    /// in `RpcEnvelope.error.code` and the response status code is folded in
    /// when the envelope is missing (e.g. middleware rejection).
    pub async fn call_raw<A>(&self, function: &str, args: A) -> Result<RpcEnvelope, HarnessError>
    where
        A: serde::Serialize,
    {
        let (_status, envelope) = self.call_raw_with_status(function, args).await?;
        Ok(envelope)
    }

    /// Same as [`call_raw`] but also returns the HTTP status code. Used
    /// internally so error envelopes can carry the real status (401 vs 403 vs
    /// 500) instead of collapsing to 0.
    pub async fn call_raw_with_status<A>(
        &self,
        function: &str,
        args: A,
    ) -> Result<(u16, RpcEnvelope), HarnessError>
    where
        A: serde::Serialize,
    {
        let url = format!("{}/_api/rpc/{}", self.base_url, function);
        let body = json!({ "args": args });
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/vnd.forge.v1+json")
            .header("x-forge-platform", "harness")
            .json(&body);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;

        // Empty body with non-2xx status: synthesize an envelope so callers
        // get a uniform error type rather than a serde failure on `null`.
        if bytes.is_empty() && !status.is_success() {
            return Ok((
                status.as_u16(),
                RpcEnvelope {
                    success: false,
                    data: None,
                    error: Some(RpcEnvelopeError {
                        code: format!("HTTP_{}", status.as_u16()),
                        message: status.canonical_reason().unwrap_or("unknown").to_string(),
                        retry_after_secs: None,
                        details: None,
                    }),
                    request_id: None,
                },
            ));
        }

        let envelope: RpcEnvelope =
            serde_json::from_slice(&bytes).map_err(|e| HarnessError::Rpc {
                code: "MALFORMED_RESPONSE".to_string(),
                message: format!(
                    "could not parse response (status={}, len={}): {e}",
                    status.as_u16(),
                    bytes.len()
                ),
                status: status.as_u16(),
            })?;
        Ok((status.as_u16(), envelope))
    }

    /// Invoke an RPC and assert that it failed. Returns the error envelope
    /// for assertion. Returns Err if the call unexpectedly succeeded.
    pub async fn expect_error<A>(
        &self,
        function: &str,
        args: A,
    ) -> Result<RpcEnvelopeError, HarnessError>
    where
        A: serde::Serialize,
    {
        let (status, envelope) = self.call_raw_with_status(function, args).await?;
        if envelope.success {
            return Err(HarnessError::Rpc {
                code: "UNEXPECTED_SUCCESS".to_string(),
                message: format!(
                    "expected {function} to fail, got success: {:?}",
                    envelope.data
                ),
                status,
            });
        }
        envelope.error.ok_or_else(|| HarnessError::Rpc {
            code: "MALFORMED_RESPONSE".to_string(),
            message: "error envelope without `error` field".to_string(),
            status,
        })
    }
}

fn envelope_to_error(envelope: RpcEnvelope, status: u16) -> HarnessError {
    match envelope.error {
        Some(err) => HarnessError::Rpc {
            code: err.code,
            message: err.message,
            status,
        },
        None => HarnessError::Rpc {
            code: "MALFORMED_RESPONSE".to_string(),
            message: "success=false but no error".to_string(),
            status,
        },
    }
}
