use std::convert::Infallible;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use forge::{
    Bytes, EnqueueOpts, EvalCtx, FailMode, Forge, Limit, PhcString, PutOpts, ScheduleOpts,
    SessionOpts, SetMode, SetOpts,
};
use futures_util::StreamExt as _;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{ContentType, Header, Status};
use rocket::request::{FromRequest, Outcome};
use rocket::response::stream::{Event, EventStream};
use rocket::response::{Redirect, status};
use rocket::serde::json::Json;
use rocket::{Build, Request, Rocket, State, catch, catchers, delete, get, options, post};
use uuid::Uuid;

use crate::types::{
    ApiError, AuthResponse, ClicksQueueDepth, CreateLinkBody, Credentials, ErrorBody, Features,
    Link, LinkRecord, LinksResponse, MeResponse, MetaResponse, OwnedLink, PublicUser, ReportLine,
    UserRecord,
};
use crate::util::{
    bearer_token, click_topic, clicks_key, env_or, invalid_login, link_slug_key, owner_key,
    qr_blob_key, random_slug, user_email_key, user_id_key, valid_slug, validate_credentials,
    validate_url,
};

pub const CLICKS_QUEUE: &str = "clicks";
pub const EXPIRE_QUEUE: &str = "link-expire";

const SESSION_IDLE_SECS: u64 = 30 * 60;
const SESSION_ABSOLUTE_SECS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_MAX_LINKS: usize = 100;

pub struct AppState {
    pub forge: Forge,
}

struct AuthHeader(Option<String>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthHeader {
    type Error = Infallible;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self(
            request
                .headers()
                .get_one("Authorization")
                .map(str::to_string),
        ))
    }
}

pub struct Cors;

#[rocket::async_trait]
impl Fairing for Cors {
    fn info(&self) -> Info {
        Info {
            name: "CORS headers",
            kind: Kind::Response,
        }
    }

    async fn on_response<'r>(
        &self,
        _request: &'r Request<'_>,
        response: &mut rocket::Response<'r>,
    ) {
        let origin = env_or("CORS_ORIGIN", "*");
        response.set_header(Header::new("Access-Control-Allow-Origin", origin));
        response.set_header(Header::new(
            "Access-Control-Allow-Methods",
            "GET, POST, PATCH, DELETE, OPTIONS",
        ));
        response.set_header(Header::new(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization",
        ));
        response.set_header(Header::new("Access-Control-Max-Age", "86400"));
    }
}

// helpers

async fn validate_session_or_401(
    forge: &Forge,
    auth: &AuthHeader,
) -> Result<forge::Session, ApiError> {
    let token = bearer_token(auth.0.as_deref())?;
    forge
        .auth()
        .validate_session(&token)
        .await?
        .ok_or_else(ApiError::unauthorized)
}

fn generate_qr_svg(text: &str) -> Result<String, ApiError> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| {
        tracing::warn!(error = ?e, "qr generation failed");
        ApiError::new(Status::InternalServerError, "qr generation failed")
    })?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(120, 120)
        .build())
}

// auth

#[post("/api/signup", data = "<input>")]
async fn signup(
    state: &State<AppState>,
    input: Json<Credentials>,
) -> Result<status::Custom<Json<AuthResponse>>, ApiError> {
    let (email, password) = validate_credentials(&input)?;

    let limit = state
        .forge
        .ratelimit()
        .check_with(
            "links-auth",
            &email,
            Limit::per_duration(20, Duration::from_secs(60)),
            FailMode::Open,
        )
        .await?;
    if !limit.allowed {
        return Err(ApiError::new(
            Status::TooManyRequests,
            "too many auth attempts; try again soon",
        ));
    }

    let user = UserRecord {
        id: Uuid::new_v4().to_string(),
        email: email.clone(),
        password_hash: state
            .forge
            .auth()
            .hash_password(&password)
            .await?
            .as_str()
            .to_string(),
    };

    let inserted = state
        .forge
        .kv()
        .set(
            &user_email_key(&email),
            Bytes::from(serde_json::to_vec(&user)?),
            SetOpts::new().with_mode(SetMode::IfNotExists),
        )
        .await?;
    if !inserted {
        return Err(ApiError::conflict("email already registered"));
    }

    state
        .forge
        .kv()
        .set(
            &user_id_key(&user.id),
            Bytes::from(serde_json::to_vec(&user)?),
            SetOpts::new(),
        )
        .await?;

    let token = state
        .forge
        .auth()
        .create_session(
            &user.id,
            SessionOpts::new()
                .with_idle_timeout(Duration::from_secs(SESSION_IDLE_SECS))
                .with_absolute_timeout(Duration::from_secs(SESSION_ABSOLUTE_SECS)),
        )
        .await?;

    Ok(status::Custom(
        Status::Created,
        Json(AuthResponse {
            token: token.as_str().to_string(),
            user: PublicUser::from(&user),
        }),
    ))
}

#[post("/api/login", data = "<input>")]
async fn login(
    state: &State<AppState>,
    input: Json<Credentials>,
) -> Result<Json<AuthResponse>, ApiError> {
    let (email, password) = validate_credentials(&input)?;

    let limit = state
        .forge
        .ratelimit()
        .check_with(
            "links-auth",
            &email,
            Limit::per_duration(20, Duration::from_secs(60)),
            FailMode::Open,
        )
        .await?;
    if !limit.allowed {
        return Err(ApiError::new(
            Status::TooManyRequests,
            "too many auth attempts; try again soon",
        ));
    }

    let Some(user_bytes) = state.forge.kv().get(&user_email_key(&email)).await? else {
        return Err(invalid_login());
    };
    let user: UserRecord = serde_json::from_slice(&user_bytes)?;
    let ok = state
        .forge
        .auth()
        .verify_password(&password, &PhcString::new(user.password_hash.clone()))
        .await?;
    if !ok {
        return Err(invalid_login());
    }

    let token = state
        .forge
        .auth()
        .create_session(
            &user.id,
            SessionOpts::new()
                .with_idle_timeout(Duration::from_secs(SESSION_IDLE_SECS))
                .with_absolute_timeout(Duration::from_secs(SESSION_ABSOLUTE_SECS)),
        )
        .await?;

    Ok(Json(AuthResponse {
        token: token.as_str().to_string(),
        user: PublicUser::from(&user),
    }))
}

#[post("/api/logout")]
async fn logout(state: &State<AppState>, auth: AuthHeader) -> Result<Status, ApiError> {
    let token = bearer_token(auth.0.as_deref())?;
    state.forge.auth().revoke_session(&token).await?;
    Ok(Status::NoContent)
}

#[get("/api/me")]
async fn me(state: &State<AppState>, auth: AuthHeader) -> Result<Json<MeResponse>, ApiError> {
    let session = validate_session_or_401(&state.forge, &auth).await?;
    let Some(bytes) = state.forge.kv().get(&user_id_key(&session.user_id)).await? else {
        return Err(ApiError::unauthorized());
    };
    let user: UserRecord = serde_json::from_slice(&bytes)?;
    Ok(Json(MeResponse {
        user: PublicUser::from(&user),
    }))
}

// links

#[get("/api/links")]
async fn list_links(
    state: &State<AppState>,
    auth: AuthHeader,
) -> Result<Json<LinksResponse>, ApiError> {
    let session = validate_session_or_401(&state.forge, &auth).await?;

    let owned: Vec<OwnedLink> = match state.forge.kv().get(&owner_key(&session.user_id)).await? {
        Some(b) => serde_json::from_slice(&b)?,
        None => Vec::new(),
    };

    let links = if owned.is_empty() {
        Vec::new()
    } else {
        let clicks_keys: Vec<String> = owned.iter().map(|l| clicks_key(&l.slug)).collect();
        let key_refs: Vec<&str> = clicks_keys.iter().map(String::as_str).collect();
        let clicks_vals = state.forge.kv().mget(&key_refs).await?;

        owned
            .into_iter()
            .zip(clicks_vals)
            .map(|(ol, cb)| {
                let clicks = cb
                    .and_then(|b| std::str::from_utf8(&b).ok().and_then(|s| s.parse().ok()))
                    .unwrap_or(0);
                Link {
                    slug: ol.slug,
                    url: ol.url,
                    created_at: ol.created_at,
                    expires_at: ol.expires_at,
                    clicks,
                }
            })
            .collect()
    };

    Ok(Json(LinksResponse { links }))
}

#[post("/api/links", data = "<input>")]
async fn create_link(
    state: &State<AppState>,
    auth: AuthHeader,
    input: Json<CreateLinkBody>,
) -> Result<status::Custom<Json<Link>>, ApiError> {
    let session = validate_session_or_401(&state.forge, &auth).await?;
    let url = validate_url(&input.url)?;

    // Load current owner list to check the limit and to append later.
    let mut owned: Vec<OwnedLink> = match state.forge.kv().get(&owner_key(&session.user_id)).await?
    {
        Some(b) => serde_json::from_slice(&b)?,
        None => Vec::new(),
    };

    let max_links: usize = state
        .forge
        .config()
        .get_raw("max_links_per_user")
        .await?
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_LINKS);

    if owned.len() >= max_links {
        return Err(ApiError::conflict("link limit reached"));
    }

    let now = Utc::now();
    let created_at = now.to_rfc3339();
    let expires_at = input
        .ttl_seconds
        .filter(|&s| s > 0)
        .map(|s| (now + chrono::Duration::seconds(s as i64)).to_rfc3339());

    // Custom slugs require the flag enabled for the requesting user.
    let slug: String = match &input.slug {
        Some(requested)
            if {
                state
                    .forge
                    .config()
                    .flag("custom_slugs", false, &EvalCtx::user(&session.user_id))
                    .await
            } =>
        {
            if !valid_slug(requested) {
                return Err(ApiError::bad_request("invalid slug"));
            }
            let rec = LinkRecord {
                slug: requested.clone(),
                url: url.clone(),
                owner_id: session.user_id.clone(),
                created_at: created_at.clone(),
                expires_at: expires_at.clone(),
            };
            let inserted = state
                .forge
                .kv()
                .set(
                    &link_slug_key(requested),
                    Bytes::from(serde_json::to_vec(&rec)?),
                    SetOpts::new().with_mode(SetMode::IfNotExists),
                )
                .await?;
            if !inserted {
                return Err(ApiError::conflict("slug already taken"));
            }
            requested.clone()
        }
        _ => {
            // Generate a random slug; retry a few times in case of collision.
            let mut chosen = None;
            for _ in 0..5u8 {
                let candidate = random_slug();
                let rec = LinkRecord {
                    slug: candidate.clone(),
                    url: url.clone(),
                    owner_id: session.user_id.clone(),
                    created_at: created_at.clone(),
                    expires_at: expires_at.clone(),
                };
                let inserted = state
                    .forge
                    .kv()
                    .set(
                        &link_slug_key(&candidate),
                        Bytes::from(serde_json::to_vec(&rec)?),
                        SetOpts::new().with_mode(SetMode::IfNotExists),
                    )
                    .await?;
                if inserted {
                    chosen = Some(candidate);
                    break;
                }
            }
            chosen.ok_or_else(|| ApiError::conflict("could not reserve slug; try again"))?
        }
    };

    // Prepend to the owner list (newest first).
    owned.insert(
        0,
        OwnedLink {
            slug: slug.clone(),
            url: url.clone(),
            created_at: created_at.clone(),
            expires_at: expires_at.clone(),
        },
    );
    state
        .forge
        .kv()
        .set(
            &owner_key(&session.user_id),
            Bytes::from(serde_json::to_vec(&owned)?),
            SetOpts::new(),
        )
        .await?;

    // Generate a QR code for the redirect path and persist to blob.
    let qr_text = format!("/{slug}");
    let svg = generate_qr_svg(&qr_text)?;
    state
        .forge
        .blob()
        .put(
            &qr_blob_key(&slug),
            Bytes::from(svg),
            PutOpts::new().with_content_type("image/svg+xml"),
        )
        .await?;

    // Schedule deletion if a TTL was requested.
    if let Some(ttl) = input.ttl_seconds.filter(|&s| s > 0) {
        let when = SystemTime::now() + Duration::from_secs(ttl);
        let payload = serde_json::json!({"slug": &slug});
        state
            .forge
            .schedule()
            .at(
                when,
                EXPIRE_QUEUE,
                Bytes::from(payload.to_string()),
                ScheduleOpts::new(),
            )
            .await?;
    }

    Ok(status::Custom(
        Status::Created,
        Json(Link {
            slug,
            url,
            created_at,
            expires_at,
            clicks: 0,
        }),
    ))
}

#[delete("/api/links/<slug>")]
async fn delete_link(
    state: &State<AppState>,
    auth: AuthHeader,
    slug: &str,
) -> Result<Status, ApiError> {
    let session = validate_session_or_401(&state.forge, &auth).await?;

    let Some(bytes) = state.forge.kv().get(&link_slug_key(slug)).await? else {
        return Err(ApiError::not_found("link not found"));
    };
    let rec: LinkRecord = serde_json::from_slice(&bytes)?;
    if rec.owner_id != session.user_id {
        return Err(ApiError::not_found("link not found"));
    }

    crate::worker::delete_link(&state.forge, slug).await;
    Ok(Status::NoContent)
}

// qr & live

#[get("/api/links/<slug>/qr.svg")]
async fn link_qr(state: &State<AppState>, slug: &str) -> Result<(ContentType, Vec<u8>), ApiError> {
    let Some(data) = state.forge.blob().get(&qr_blob_key(slug)).await? else {
        return Err(ApiError::not_found("qr not found"));
    };
    Ok((ContentType::new("image", "svg+xml"), data.to_vec()))
}

#[get("/api/links/<slug>/live")]
fn link_live(state: &State<AppState>, slug: &str) -> EventStream![] {
    let forge = state.forge.clone();
    let slug = slug.to_string();
    EventStream! {
        let sub = forge.pubsub().subscribe(&click_topic(&slug)).await;
        match sub {
            Err(err) => {
                tracing::warn!(error = %err, slug, "pubsub subscribe failed");
            }
            Ok(mut stream) => {
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(bytes) => yield Event::data(String::from_utf8_lossy(&bytes).to_string()),
                        Err(err) => {
                            tracing::warn!(error = %err, slug, "pubsub stream error");
                            break;
                        }
                    }
                }
            }
        }
    }
}

// redirect hot path

/// Matches any single path segment at low priority. Returns 302 on success,
/// 404/429 on failure. Must be ranked after all /api/* routes.
#[get("/<slug>", rank = 20)]
async fn redirect(state: &State<AppState>, slug: &str) -> Result<Redirect, ApiError> {
    if !valid_slug(slug) {
        return Err(ApiError::not_found("link not found"));
    }

    let Some(bytes) = state.forge.kv().get(&link_slug_key(slug)).await? else {
        return Err(ApiError::not_found("link not found"));
    };
    let rec: LinkRecord = serde_json::from_slice(&bytes)?;

    if let Some(ref exp) = rec.expires_at {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(exp) {
            if dt < Utc::now() {
                return Err(ApiError::not_found("link not found"));
            }
        }
    }

    let rl = state
        .forge
        .ratelimit()
        .check_with(
            "redirect",
            slug,
            Limit::per_duration(600, Duration::from_secs(60)),
            FailMode::Open,
        )
        .await?;
    if !rl.allowed {
        return Err(ApiError::new(
            Status::TooManyRequests,
            "too many requests; try again soon",
        ));
    }

    state.forge.kv().incr(&clicks_key(slug), 1).await?;

    let payload = serde_json::json!({"slug": slug});
    state
        .forge
        .queue()
        .enqueue(
            CLICKS_QUEUE,
            Bytes::from(payload.to_string()),
            EnqueueOpts::new().with_max_attempts(3),
        )
        .await?;

    Ok(Redirect::found(rec.url))
}

// meta & health

#[get("/api/meta")]
async fn meta(state: &State<AppState>) -> Result<Json<MetaResponse>, ApiError> {
    let depth = state.forge.queue().depth(CLICKS_QUEUE).await?;
    let custom_slugs = state
        .forge
        .config()
        .flag("custom_slugs", false, &EvalCtx::new())
        .await;
    let forge_lines = state
        .forge
        .backend_report()
        .backends
        .into_iter()
        .map(|line| ReportLine {
            primitive: line.primitive.as_str().to_string(),
            provider: line.provider.to_string(),
            durable: line.durable,
            caveats: line.caveats.to_string(),
        })
        .collect();

    Ok(Json(MetaResponse {
        backend: "rust",
        forge: forge_lines,
        features: Features { custom_slugs },
        clicks_queue_depth: ClicksQueueDepth {
            visible: depth.visible,
            in_flight: depth.in_flight,
            delayed: depth.delayed,
        },
    }))
}

#[get("/healthz")]
fn healthz() -> &'static str {
    "ok"
}

// CORS preflight

#[options("/<_path..>")]
fn preflight(_path: PathBuf) -> Status {
    Status::NoContent
}

// error catcher

#[catch(default)]
fn default_catcher(status: Status, _request: &Request<'_>) -> status::Custom<Json<ErrorBody>> {
    let error = match status.code {
        400 | 422 => "invalid request",
        401 => "authentication required",
        404 => "not found",
        405 => "method not allowed",
        413 => "request body too large",
        code if code >= 500 => "internal error",
        _ => "request failed",
    };
    status::Custom(
        status,
        Json(ErrorBody {
            error: error.to_string(),
        }),
    )
}

pub fn mount_routes(rocket: Rocket<Build>) -> Rocket<Build> {
    rocket
        .attach(Cors)
        .mount(
            "/",
            rocket::routes![
                healthz,
                meta,
                signup,
                login,
                logout,
                me,
                list_links,
                create_link,
                delete_link,
                link_qr,
                link_live,
                redirect,
                preflight,
            ],
        )
        .register("/", catchers![default_catcher])
}
