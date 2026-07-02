use rocket::Request;
use rocket::http::Status;
use rocket::response::{Responder, status};
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserRecord {
    pub id: String,
    pub email: String,
    pub password_hash: String,
}

#[derive(Debug, Serialize)]
pub struct PublicUser {
    pub id: String,
    pub email: String,
}

impl From<&UserRecord> for PublicUser {
    fn from(user: &UserRecord) -> Self {
        Self {
            id: user.id.clone(),
            email: user.email.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: PublicUser,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: PublicUser,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Todo {
    pub id: String,
    pub title: String,
    pub completed: bool,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct TodoList {
    pub todos: Vec<Todo>,
}

#[derive(Debug, Deserialize)]
pub struct TodoCreate {
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub struct TodoPatch {
    pub title: Option<String>,
    pub completed: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: Status,
    pub message: String,
}

impl ApiError {
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(Status::BadRequest, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(Status::Unauthorized, "authentication required")
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Status::Conflict, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Status::NotFound, message)
    }
}

impl<'r> Responder<'r, 'static> for ApiError {
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'static> {
        status::Custom(
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
        .respond_to(request)
    }
}

impl From<forgelib::ForgeError> for ApiError {
    fn from(err: forgelib::ForgeError) -> Self {
        tracing::warn!(error = %err, "forge operation failed");
        Self::new(Status::InternalServerError, "forge operation failed")
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        tracing::warn!(error = %err, "json operation failed");
        Self::new(Status::InternalServerError, "json operation failed")
    }
}

#[derive(Debug, Serialize)]
pub struct ReportLine {
    pub primitive: String,
    pub provider: String,
    pub durable: bool,
    pub caveats: String,
}

#[derive(Debug, Serialize)]
pub struct AuditDepth {
    pub visible: u64,
    #[serde(rename = "inFlight")]
    pub in_flight: u64,
    pub delayed: u64,
}

#[derive(Debug, Serialize)]
pub struct MetaResponse {
    pub backend: &'static str,
    pub forge: Vec<ReportLine>,
    #[serde(rename = "auditDepth")]
    pub audit_depth: AuditDepth,
}
