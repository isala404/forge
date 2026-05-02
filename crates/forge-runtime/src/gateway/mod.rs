mod auth;
pub mod jwks;
mod mcp;
mod multipart;
pub mod oauth;
mod request;
mod response;
mod rpc;
mod server;
mod sse;
mod tls;
mod tracing;

pub use auth::{AuthConfig, AuthMiddleware, HmacTokenIssuer, build_auth_context_from_claims};
pub use jwks::{JwksClient, JwksError};
pub use mcp::{McpState, mcp_get_handler, mcp_post_handler};
pub use multipart::rpc_multipart_handler;
pub use oauth::OAuthState;
pub use request::{BatchRpcRequest, BatchRpcResponse, RpcRequest};
pub use response::{RpcError, RpcResponse};
pub use rpc::RpcHandler;
pub use server::{GatewayConfig, GatewayServer, TrustedProxies};
pub use sse::{
    SseConfig, SsePayload, SseQuery, SseState, sse_handler, sse_job_subscribe_handler,
    sse_subscribe_handler, sse_unsubscribe_handler, sse_workflow_subscribe_handler,
};
pub use tls::{
    GatewayConn, GatewayListener, PeerAddr, TlsListenConfig, bind_listener, load_rustls_config,
};
pub use tracing::TracingMiddleware;

/// Resolved client IP after applying trusted proxy rules.
/// Injected as an Extension by the client IP middleware.
#[derive(Debug, Clone)]
pub struct ResolvedClientIp(pub Option<String>);

/// Resolve the real client IP using the trusted proxy chain.
///
/// When `trusted_proxies` is empty, returns `peer_addr` directly (headers
/// are never trusted). When the peer matches a trusted range,
/// `X-Forwarded-For` is walked right-to-left and the first non-proxy IP is
/// returned.
pub(crate) fn resolve_client_ip(
    headers: &axum::http::HeaderMap,
    peer_addr: Option<std::net::IpAddr>,
    trusted_proxies: &[ipnet::IpNet],
) -> Option<String> {
    let peer = peer_addr?;

    if trusted_proxies.is_empty() {
        return Some(peer.to_string());
    }

    let peer_is_trusted = trusted_proxies.iter().any(|net| net.contains(&peer));
    if !peer_is_trusted {
        return Some(peer.to_string());
    }

    // Walk X-Forwarded-For right-to-left, return the rightmost non-proxy IP
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        for ip_str in xff.split(',').map(|s| s.trim()).rev() {
            if let Ok(ip) = ip_str.parse::<std::net::IpAddr>()
                && !trusted_proxies.iter().any(|net| net.contains(&ip))
            {
                return Some(ip.to_string());
            }
        }
    }

    // Fallback to X-Real-IP
    if let Some(real_ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = real_ip.trim();
        if let Ok(ip) = trimmed.parse::<std::net::IpAddr>()
            && !trusted_proxies.iter().any(|net| net.contains(&ip))
        {
            return Some(ip.to_string());
        }
    }

    Some(peer.to_string())
}

/// Legacy header-based IP extraction (no proxy validation).
/// Used only in contexts where peer address is unavailable.
pub(crate) fn extract_client_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

pub(crate) fn extract_header(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};
    use std::net::IpAddr;

    fn parse_nets(cidrs: &[&str]) -> Vec<ipnet::IpNet> {
        cidrs
            .iter()
            .map(|s| s.parse().expect("valid CIDR"))
            .collect()
    }

    #[tokio::test]
    async fn no_trusted_proxies_returns_peer_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));
        let peer: IpAddr = "10.0.0.1".parse().expect("valid IP");
        let result = resolve_client_ip(&headers, Some(peer), &[]);
        assert_eq!(result, Some("10.0.0.1".to_string()));
    }

    #[tokio::test]
    async fn trusted_proxy_returns_xff_client_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.50"));
        let peer: IpAddr = "10.0.0.1".parse().expect("valid IP");
        let trusted = parse_nets(&["10.0.0.0/8"]);
        let result = resolve_client_ip(&headers, Some(peer), &trusted);
        assert_eq!(result, Some("203.0.113.50".to_string()));
    }

    #[tokio::test]
    async fn untrusted_peer_ignores_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));
        let peer: IpAddr = "192.168.1.1".parse().expect("valid IP");
        let trusted = parse_nets(&["10.0.0.0/8"]);
        let result = resolve_client_ip(&headers, Some(peer), &trusted);
        assert_eq!(result, Some("192.168.1.1".to_string()));
    }

    #[tokio::test]
    async fn chained_xff_returns_rightmost_non_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.50, 10.0.0.2, 10.0.0.3"),
        );
        let peer: IpAddr = "10.0.0.1".parse().expect("valid IP");
        let trusted = parse_nets(&["10.0.0.0/8"]);
        let result = resolve_client_ip(&headers, Some(peer), &trusted);
        assert_eq!(result, Some("203.0.113.50".to_string()));
    }

    #[tokio::test]
    async fn all_xff_ips_trusted_falls_back_to_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.5, 10.0.0.6"),
        );
        let peer: IpAddr = "10.0.0.1".parse().expect("valid IP");
        let trusted = parse_nets(&["10.0.0.0/8"]);
        let result = resolve_client_ip(&headers, Some(peer), &trusted);
        assert_eq!(result, Some("10.0.0.1".to_string()));
    }

    #[tokio::test]
    async fn no_peer_returns_none() {
        let headers = HeaderMap::new();
        let result = resolve_client_ip(&headers, None, &[]);
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn trusted_proxy_falls_back_to_x_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.50"));
        let peer: IpAddr = "10.0.0.1".parse().expect("valid IP");
        let trusted = parse_nets(&["10.0.0.0/8"]);
        let result = resolve_client_ip(&headers, Some(peer), &trusted);
        assert_eq!(result, Some("203.0.113.50".to_string()));
    }
}
