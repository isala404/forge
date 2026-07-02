use rocket::http::Status;

use crate::types::{ApiError, Credentials};

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn validate_credentials(input: &Credentials) -> Result<(String, String), ApiError> {
    let email = input.email.trim().to_ascii_lowercase();
    let password = input.password.trim().to_string();
    if !email.contains('@') || email.len() > 254 {
        return Err(ApiError::bad_request("enter a valid email"));
    }
    if password.len() < 8 {
        return Err(ApiError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    Ok((email, password))
}

pub fn validate_url(raw: &str) -> Result<String, ApiError> {
    let url = raw.trim().to_string();
    if (!url.starts_with("http://") && !url.starts_with("https://")) || url.len() > 2048 {
        return Err(ApiError::bad_request("enter a valid http(s) url"));
    }
    Ok(url)
}

pub const RESERVED_SLUGS: &[&str] = &["api", "healthz", "favicon.ico"];

/// Validates slug format: 3-32 chars, [A-Za-z0-9_-], not reserved.
pub fn valid_slug(slug: &str) -> bool {
    let len = slug.len();
    (3..=32).contains(&len)
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !RESERVED_SLUGS.contains(&slug)
}

pub fn bearer_token(raw: Option<&str>) -> Result<String, ApiError> {
    let raw = raw.ok_or_else(ApiError::unauthorized)?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim();
    if token.is_empty() {
        return Err(ApiError::unauthorized());
    }
    Ok(token.to_string())
}

pub fn invalid_login() -> ApiError {
    ApiError::new(Status::Unauthorized, "invalid email or password")
}

/// Returns a random 7-character slug using hex chars from a UUID v4.
pub fn random_slug() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(7)
        .collect()
}

pub fn user_email_key(email: &str) -> String {
    format!("link:user:email:{email}")
}

pub fn user_id_key(id: &str) -> String {
    format!("link:user:id:{id}")
}

pub fn owner_key(user_id: &str) -> String {
    format!("link:owner:{user_id}")
}

pub fn link_slug_key(slug: &str) -> String {
    format!("link:slug:{slug}")
}

pub fn clicks_key(slug: &str) -> String {
    format!("clicks:{slug}")
}

pub fn qr_blob_key(slug: &str) -> String {
    format!("qr:{slug}")
}

pub fn click_topic(slug: &str) -> String {
    format!("clicks:{slug}")
}
