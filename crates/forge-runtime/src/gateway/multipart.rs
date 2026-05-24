use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bytes::BytesMut;

use forge_core::function::AuthContext;
use forge_core::types::Upload;

use super::rpc::RpcHandler;

const MAX_FIELD_NAME_LENGTH: usize = 255;
const MAX_FILENAME_LENGTH: usize = 255;
const MAX_JSON_FIELD_SIZE: usize = 1024 * 1024;
const JSON_FIELD_NAME: &str = "_json";

/// Sanitize a raw upload filename for safe persistence and logging.
///
/// Strips path components, neutralizes traversal sequences after the basename
/// is isolated, rejects null bytes / control chars, and rewrites Windows
/// reserved device names (CON, PRN, NUL, AUX, COM[1-9], LPT[1-9]) so that
/// downstream code that mirrors the upload to disk on Windows can't trip the
/// reserved-name handling. Returns `None` when nothing salvageable remains.
fn sanitize_filename(raw: &str) -> Option<String> {
    // Basename first: take the last path component for either separator, then
    // collapse traversal sequences inside the basename so `foo..bar` keeps the
    // double-dot but `..` alone becomes `_`.
    let basename = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let basename = basename.trim();
    if basename.is_empty() || basename == "." || basename == ".." {
        return None;
    }

    // Reject controls and null bytes outright. A null byte in a filename is a
    // classic log-truncation / path-confusion vector.
    if basename.chars().any(|c| c == '\0' || c.is_control()) {
        return None;
    }

    let mut name: String = basename
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .collect();

    // Windows reserved device names match against the basename without its
    // extension. Comparison is case-insensitive.
    let stem = name.split('.').next().unwrap_or(&name).to_ascii_uppercase();
    let is_reserved_dev = |prefix: &str| -> bool {
        stem.strip_prefix(prefix)
            .and_then(|rest| rest.parse::<u8>().ok())
            .is_some_and(|n| (1..=9).contains(&n))
    };
    let is_reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || is_reserved_dev("COM")
        || is_reserved_dev("LPT");
    if is_reserved {
        name = format!("_{name}");
    }

    if name.len() > MAX_FILENAME_LENGTH {
        name.truncate(MAX_FILENAME_LENGTH);
    }

    if name.is_empty() { None } else { Some(name) }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_filename;

    #[test]
    fn strips_path_components() {
        assert_eq!(sanitize_filename("/etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            sanitize_filename("C:\\Windows\\system.ini").as_deref(),
            Some("system.ini")
        );
    }

    #[test]
    fn preserves_legitimate_double_dots() {
        assert_eq!(sanitize_filename("foo..bar").as_deref(), Some("foo..bar"));
    }

    #[test]
    fn rejects_traversal_only_basename() {
        assert!(sanitize_filename("../../etc/passwd").is_some());
        assert_eq!(
            sanitize_filename("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert!(sanitize_filename("..").is_none());
        assert!(sanitize_filename("foo/..").is_none());
    }

    #[test]
    fn rejects_control_chars_and_nulls() {
        assert!(sanitize_filename("foo\0bar").is_none());
        assert!(sanitize_filename("foo\nbar").is_none());
    }

    #[test]
    fn rewrites_windows_reserved_names() {
        assert_eq!(sanitize_filename("CON").as_deref(), Some("_CON"));
        assert_eq!(sanitize_filename("nul.txt").as_deref(), Some("_nul.txt"));
        assert_eq!(sanitize_filename("COM1.log").as_deref(), Some("_COM1.log"));
    }
}

/// Lightweight magic-byte check: for a small set of well-known media types,
/// the declared `Content-Type` must match the file's leading bytes. Types
/// outside this list pass through (we don't have a signature library, and
/// rejecting unknown types would break legitimate uploads).
fn mime_matches_magic(declared: &str, bytes: &[u8]) -> bool {
    let declared = declared.split(';').next().unwrap_or(declared).trim();
    match declared {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" | "image/jpg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()),
        "image/bmp" => bytes.starts_with(b"BM"),
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "application/zip" => {
            bytes.starts_with(b"PK\x03\x04")
                || bytes.starts_with(b"PK\x05\x06")
                || bytes.starts_with(b"PK\x07\x08")
        }
        "application/gzip" | "application/x-gzip" => bytes.starts_with(b"\x1f\x8b"),
        _ => true,
    }
}

/// Configurable limits for multipart uploads, injected via Axum extension.
///
/// `max_body_size_bytes` caps the total request body when a mutation does
/// not declare its own `max_size`. `max_file_size_bytes` caps any single
/// file under the same conditions; per-mutation `max_size` overrides both
/// (it is treated as an explicit opt-in for large single files).
#[derive(Debug, Clone)]
pub struct MultipartConfig {
    pub max_body_size_bytes: usize,
    pub max_file_size_bytes: usize,
    pub max_upload_fields: usize,
}

/// Resolve the effective (total, per-file) upload limits for a request.
///
/// When a mutation declares `max_size`, the value acts as an explicit
/// opt-in: it caps both the total body and any single file. Without an
/// override, the total falls back to `max_body_size_bytes` and any single
/// file is capped by `max_file_size_bytes` (clamped to the total).
fn resolve_upload_limits(per_mutation: Option<usize>, config: &MultipartConfig) -> (usize, usize) {
    match per_mutation {
        Some(limit) => (limit, limit),
        None => (
            config.max_body_size_bytes,
            config.max_file_size_bytes.min(config.max_body_size_bytes),
        ),
    }
}

/// Create a multipart error response.
fn multipart_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        status,
        axum::Json(serde_json::json!({
            "success": false,
            "error": {
                "code": code,
                "message": message.into()
            }
        })),
    )
}

/// Handle multipart form data for RPC calls with file uploads.
pub async fn rpc_multipart_handler(
    State(handler): State<Arc<RpcHandler>>,
    Extension(auth): Extension<AuthContext>,
    Extension(mp_config): Extension<MultipartConfig>,
    Path(function): Path<String>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Validate function name to prevent log injection and path traversal
    if function.is_empty()
        || function.len() > 256
        || !function
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':' || c == '-')
    {
        return multipart_error(
            StatusCode::BAD_REQUEST,
            "INVALID_FUNCTION",
            "Invalid function name: must be 1-256 alphanumeric characters, underscores, dots, colons, or hyphens",
        );
    }

    let per_mutation = handler
        .function_info(&function)
        .and_then(|info| info.max_upload_size_bytes);
    let (max_total, max_file) = resolve_upload_limits(per_mutation, &mp_config);

    let mut json_args: Option<serde_json::Value> = None;
    let mut uploads: HashMap<String, Upload> = HashMap::new();
    let mut total_read: usize = 0;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return multipart_error(StatusCode::BAD_REQUEST, "MULTIPART_ERROR", e.to_string());
            }
        };

        let name = match field.name().map(String::from).filter(|n| !n.is_empty()) {
            Some(n) => n,
            None => {
                return multipart_error(
                    StatusCode::BAD_REQUEST,
                    "INVALID_FIELD",
                    "Field name is required",
                );
            }
        };

        if name.len() > MAX_FIELD_NAME_LENGTH {
            return multipart_error(
                StatusCode::BAD_REQUEST,
                "INVALID_FIELD",
                format!("Field name too long (max {} chars)", MAX_FIELD_NAME_LENGTH),
            );
        }

        if name.contains("..")
            || name.contains('/')
            || name.contains('\\')
            || name.contains(|c: char| c.is_control())
        {
            return multipart_error(
                StatusCode::BAD_REQUEST,
                "INVALID_FIELD",
                "Field name contains invalid characters",
            );
        }

        // Check upload count before processing to prevent bypass via _json field ordering
        if name != JSON_FIELD_NAME && uploads.len() >= mp_config.max_upload_fields {
            return multipart_error(
                StatusCode::BAD_REQUEST,
                "TOO_MANY_FIELDS",
                format!(
                    "Maximum {} upload fields allowed",
                    mp_config.max_upload_fields
                ),
            );
        }

        if name == JSON_FIELD_NAME {
            // Duplicate `_json` would silently let the last value win, which
            // is a parameter-smuggling avenue when upstream validators only
            // inspected the first occurrence.
            if json_args.is_some() {
                return multipart_error(
                    StatusCode::BAD_REQUEST,
                    "DUPLICATE_FIELD",
                    "Multiple `_json` fields submitted; only one is allowed",
                );
            }

            let mut buffer = BytesMut::new();
            let mut json_field = field;

            loop {
                match json_field.chunk().await {
                    Ok(Some(chunk)) => {
                        if total_read + chunk.len() > max_total {
                            return multipart_error(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "PAYLOAD_TOO_LARGE",
                                format!(
                                    "Multipart payload exceeds maximum size of {max_total} bytes (field `_json`)"
                                ),
                            );
                        }
                        if buffer.len() + chunk.len() > MAX_JSON_FIELD_SIZE {
                            return multipart_error(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "JSON_TOO_LARGE",
                                format!(
                                    "_json field exceeds maximum size of {} bytes",
                                    MAX_JSON_FIELD_SIZE
                                ),
                            );
                        }
                        total_read += chunk.len();
                        buffer.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        return multipart_error(
                            StatusCode::BAD_REQUEST,
                            "READ_ERROR",
                            format!("Failed to read _json field: {}", e),
                        );
                    }
                }
            }

            let text = match std::str::from_utf8(&buffer) {
                Ok(s) => s,
                Err(_) => {
                    return multipart_error(
                        StatusCode::BAD_REQUEST,
                        "INVALID_JSON",
                        "Invalid UTF-8 in _json field",
                    );
                }
            };

            match serde_json::from_str(text) {
                Ok(value) => json_args = Some(value),
                Err(e) => {
                    return multipart_error(
                        StatusCode::BAD_REQUEST,
                        "INVALID_JSON",
                        format!("Invalid JSON in _json field: {}", e),
                    );
                }
            }
        } else {
            let raw_filename = field
                .file_name()
                .map(String::from)
                .unwrap_or_else(|| name.clone());
            let filename = match sanitize_filename(&raw_filename) {
                Some(f) => f,
                None => {
                    return multipart_error(
                        StatusCode::BAD_REQUEST,
                        "INVALID_FILENAME",
                        "Filename empty or contains disallowed characters after sanitization",
                    );
                }
            };
            let content_type = field
                .content_type()
                .map(String::from)
                .unwrap_or_else(|| "application/octet-stream".to_string());

            let mut buffer = BytesMut::new();
            let mut field = field;

            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if total_read + chunk.len() > max_total {
                            return multipart_error(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "PAYLOAD_TOO_LARGE",
                                format!(
                                    "Multipart payload exceeds maximum size of {max_total} bytes (field `{name}`, file `{filename}`)"
                                ),
                            );
                        }
                        if buffer.len() + chunk.len() > max_file {
                            return multipart_error(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "FILE_TOO_LARGE",
                                format!(
                                    "File '{}' exceeds maximum size of {} bytes",
                                    filename, max_file
                                ),
                            );
                        }
                        total_read += chunk.len();
                        buffer.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        return multipart_error(
                            StatusCode::BAD_REQUEST,
                            "READ_ERROR",
                            format!("Failed to read file field: {}", e),
                        );
                    }
                }
            }

            if !mime_matches_magic(&content_type, &buffer) {
                return multipart_error(
                    StatusCode::BAD_REQUEST,
                    "MIME_MISMATCH",
                    format!(
                        "File '{filename}' content does not match declared Content-Type '{content_type}'"
                    ),
                );
            }

            let upload = Upload::new(filename, content_type, buffer.freeze());
            uploads.insert(name, upload);
        }
    }

    let mut args = json_args.unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    if let serde_json::Value::Object(ref mut map) = args {
        for (name, upload) in uploads {
            // Prevent upload fields from overwriting JSON args (parameter tampering)
            if map.contains_key(&name) {
                return multipart_error(
                    StatusCode::BAD_REQUEST,
                    "DUPLICATE_FIELD",
                    format!("Upload field '{}' conflicts with JSON argument", name),
                );
            }
            match serde_json::to_value(&upload) {
                Ok(value) => {
                    map.insert(name, value);
                }
                Err(e) => {
                    return multipart_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "SERIALIZE_ERROR",
                        format!("Failed to serialize upload: {}", e),
                    );
                }
            }
        }
    }

    let request = super::request::RpcRequest::new(function, args);
    let metadata = forge_core::function::RequestMetadata::new();

    let response = handler.handle(request, auth, metadata).await;

    // Use RpcResponse's IntoResponse to preserve correct HTTP status codes
    let status = if response.success {
        StatusCode::OK
    } else {
        response
            .error
            .as_ref()
            .map(|e| e.status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    };
    match serde_json::to_value(&response) {
        Ok(value) => (status, axum::Json(value)),
        Err(e) => multipart_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE_ERROR",
            format!("Failed to serialize response: {}", e),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn config(body: usize, file: usize) -> MultipartConfig {
        MultipartConfig {
            max_body_size_bytes: body,
            max_file_size_bytes: file,
            max_upload_fields: 20,
        }
    }

    #[test]
    fn test_json_field_name_constant() {
        assert_eq!(JSON_FIELD_NAME, "_json");
    }

    #[test]
    fn per_mutation_limit_overrides_both_total_and_file() {
        let cfg = config(20 * MB, 10 * MB);
        let (total, file) = resolve_upload_limits(Some(200 * MB), &cfg);
        assert_eq!(total, 200 * MB);
        assert_eq!(file, 200 * MB);
    }

    #[test]
    fn without_override_uses_global_body_and_file_limits() {
        let cfg = config(50 * MB, 10 * MB);
        let (total, file) = resolve_upload_limits(None, &cfg);
        assert_eq!(total, 50 * MB);
        assert_eq!(file, 10 * MB);
    }

    #[test]
    fn file_limit_clamped_to_body_limit() {
        let cfg = config(5 * MB, 50 * MB);
        let (total, file) = resolve_upload_limits(None, &cfg);
        assert_eq!(total, 5 * MB);
        assert_eq!(file, 5 * MB);
    }

    #[test]
    fn zero_per_mutation_is_still_respected() {
        let cfg = config(20 * MB, 10 * MB);
        let (total, file) = resolve_upload_limits(Some(0), &cfg);
        assert_eq!(total, 0);
        assert_eq!(file, 0);
    }

    const MB: usize = 1024 * 1024;

    #[test]
    fn mime_matches_known_signatures() {
        assert!(mime_matches_magic("image/png", b"\x89PNG\r\n\x1a\n..."));
        assert!(mime_matches_magic("image/jpeg", b"\xff\xd8\xff\xe0junk"));
        assert!(mime_matches_magic("application/pdf", b"%PDF-1.7\n"));
        assert!(mime_matches_magic(
            "image/webp",
            b"RIFF\x10\x00\x00\x00WEBP...."
        ));
    }

    #[test]
    fn mime_rejects_mismatched_signatures() {
        assert!(!mime_matches_magic("image/png", b"GIF89a"));
        assert!(!mime_matches_magic("application/pdf", b"<html>"));
    }

    #[test]
    fn mime_passes_through_unknown_types() {
        assert!(mime_matches_magic("application/octet-stream", b"\x00\x01"));
        assert!(mime_matches_magic("text/plain", b"hello"));
    }

    #[test]
    fn mime_handles_content_type_parameters() {
        assert!(mime_matches_magic(
            "image/png; charset=binary",
            b"\x89PNG\r\n\x1a\n"
        ));
    }
}
