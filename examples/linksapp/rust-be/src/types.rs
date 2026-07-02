use rocket::Request;
use rocket::http::Status;
use rocket::response::{Responder, status};
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};

// Stored at link:user:email:{email} and link:user:id:{id}. Never returned to clients.
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

/// Full record stored at link:slug:{slug}.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LinkRecord {
    pub slug: String,
    pub url: String,
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
}

/// Subset stored in the owner's link list at link:owner:{userId}.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OwnedLink {
    pub slug: String,
    pub url: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
}

/// The shape returned to API callers (clicks field added at read time).
#[derive(Debug, Serialize)]
pub struct Link {
    pub slug: String,
    pub url: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
    pub clicks: i64,
}

#[derive(Debug, Serialize)]
pub struct LinksResponse {
    pub links: Vec<Link>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLinkBody {
    pub url: String,
    pub slug: Option<String>,
    #[serde(rename = "ttlSeconds")]
    pub ttl_seconds: Option<u64>,
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

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Status::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Status::Conflict, message)
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
        tracing::warn!(error = %err, "json error");
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
pub struct ClicksQueueDepth {
    pub visible: u64,
    #[serde(rename = "inFlight")]
    pub in_flight: u64,
    pub delayed: u64,
}

#[derive(Debug, Serialize)]
pub struct Features {
    #[serde(rename = "customSlugs")]
    pub custom_slugs: bool,
}

#[derive(Debug, Serialize)]
pub struct MetaResponse {
    pub backend: &'static str,
    pub forge: Vec<ReportLine>,
    pub features: Features,
    #[serde(rename = "clicksQueueDepth")]
    pub clicks_queue_depth: ClicksQueueDepth,
}
