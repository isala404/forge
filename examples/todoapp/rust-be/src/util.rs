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

pub fn validate_title(raw: &str) -> Result<String, ApiError> {
    let title = raw.trim();
    if title.is_empty() || title.len() > 160 {
        return Err(ApiError::bad_request("title must be 1 to 160 characters"));
    }
    Ok(title.to_string())
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

pub fn user_email_key(email: &str) -> String {
    format!("todo:user:email:{email}")
}

pub fn user_id_key(id: &str) -> String {
    format!("todo:user:id:{id}")
}

pub fn todos_key(user_id: &str) -> String {
    format!("todo:todos:{user_id}")
}
