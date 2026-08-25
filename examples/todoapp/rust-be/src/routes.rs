use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use forgelib::{
    Bytes, EnqueueOpts, FailMode, Forge, Limit, PhcString, Priority, SessionOpts, SetMode, SetOpts,
};
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{Header, Status};
use rocket::request::{FromRequest, Outcome};
use rocket::response::status;
use rocket::serde::json::Json;
use rocket::{Build, Request, Rocket, State, catch, catchers, delete, get, options, patch, post};
use uuid::Uuid;

use crate::types::{
    ApiError, AuditDepth, AuthResponse, Credentials, ErrorBody, MeResponse, MetaResponse,
    PublicUser, ReportLine, Todo, TodoCreate, TodoList, TodoPatch, UserRecord,
};
use crate::util::{
    bearer_token, env_or, invalid_login, todos_key, user_email_key, user_id_key,
    validate_credentials, validate_title,
};

const AUDIT_QUEUE: &str = "todo-audit";
const SESSION_IDLE_SECS: u64 = 30 * 60;
const SESSION_ABSOLUTE_SECS: u64 = 7 * 24 * 60 * 60;

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

#[post("/api/signup", data = "<input>")]
async fn signup(
    state: &State<AppState>,
    input: Json<Credentials>,
) -> Result<status::Custom<Json<AuthResponse>>, ApiError> {
    let (email, password) = validate_credentials(&input)?;

    let auth_limit = state
        .forge
        .ratelimit()
        .check_with(
            "todo-auth",
            &email,
            Limit::per_duration(20, Duration::from_secs(60)),
            FailMode::Open,
        )
        .await?;
    if !auth_limit.allowed {
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

    let auth_limit = state
        .forge
        .ratelimit()
        .check_with(
            "todo-auth",
            &email,
            Limit::per_duration(20, Duration::from_secs(60)),
            FailMode::Open,
        )
        .await?;
    if !auth_limit.allowed {
        return Err(ApiError::new(
            Status::TooManyRequests,
            "too many auth attempts; try again soon",
        ));
    }

    let Some(user_bytes) = state.forge.kv().get(&user_email_key(&email)).await? else {
        return Err(invalid_login());
    };
    let user: UserRecord = serde_json::from_slice(&user_bytes)?;
    let password_ok = state
        .forge
        .auth()
        .verify_password(&password, &PhcString::new(user.password_hash.clone()))
        .await?;
    if !password_ok {
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
    state
        .forge
        .auth()
        .revoke_session(&bearer_token(auth.0.as_deref())?)
        .await?;
    Ok(Status::NoContent)
}

#[get("/api/me")]
async fn me(state: &State<AppState>, auth: AuthHeader) -> Result<Json<MeResponse>, ApiError> {
    let Some(session) = state
        .forge
        .auth()
        .validate_session(&bearer_token(auth.0.as_deref())?)
        .await?
    else {
        return Err(ApiError::unauthorized());
    };
    let Some(user_bytes) = state.forge.kv().get(&user_id_key(&session.user_id)).await? else {
        return Err(ApiError::unauthorized());
    };
    let user: UserRecord = serde_json::from_slice(&user_bytes)?;

    Ok(Json(MeResponse {
        user: PublicUser::from(&user),
    }))
}

#[get("/api/todos")]
async fn list_todos(state: &State<AppState>, auth: AuthHeader) -> Result<Json<TodoList>, ApiError> {
    let Some(session) = state
        .forge
        .auth()
        .validate_session(&bearer_token(auth.0.as_deref())?)
        .await?
    else {
        return Err(ApiError::unauthorized());
    };

    let todos = match state.forge.kv().get(&todos_key(&session.user_id)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => Vec::new(),
    };
    Ok(Json(TodoList { todos }))
}

#[post("/api/todos", data = "<input>")]
async fn create_todo(
    state: &State<AppState>,
    auth: AuthHeader,
    input: Json<TodoCreate>,
) -> Result<status::Custom<Json<Todo>>, ApiError> {
    let Some(session) = state
        .forge
        .auth()
        .validate_session(&bearer_token(auth.0.as_deref())?)
        .await?
    else {
        return Err(ApiError::unauthorized());
    };

    let mut todos: Vec<Todo> = match state.forge.kv().get(&todos_key(&session.user_id)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => Vec::new(),
    };

    let now = Utc::now().to_rfc3339();
    let todo = Todo {
        id: Uuid::new_v4().to_string(),
        title: validate_title(&input.title)?,
        completed: false,
        created_at: now.clone(),
        updated_at: now,
    };
    todos.insert(0, todo.clone());

    state
        .forge
        .kv()
        .set(
            &todos_key(&session.user_id),
            Bytes::from(serde_json::to_vec(&todos)?),
            SetOpts::new(),
        )
        .await?;

    let audit = serde_json::json!({
        "userId": session.user_id,
        "action": "created",
        "todoId": todo.id,
        "at": Utc::now().to_rfc3339(),
    });
    state
        .forge
        .queue()
        .enqueue(
            AUDIT_QUEUE,
            Bytes::from(audit.to_string()),
            EnqueueOpts::new()
                .with_max_attempts(3)
                .with_priority(Priority::Low)
                .with_concurrency_key(session.user_id.to_string())
                .with_dedup_id(format!("created:{}", todo.id)),
        )
        .await?;

    Ok(status::Custom(Status::Created, Json(todo)))
}

#[patch("/api/todos/<id>", data = "<input>")]
async fn update_todo(
    state: &State<AppState>,
    auth: AuthHeader,
    id: &str,
    input: Json<TodoPatch>,
) -> Result<Json<Todo>, ApiError> {
    let Some(session) = state
        .forge
        .auth()
        .validate_session(&bearer_token(auth.0.as_deref())?)
        .await?
    else {
        return Err(ApiError::unauthorized());
    };

    let mut todos: Vec<Todo> = match state.forge.kv().get(&todos_key(&session.user_id)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => Vec::new(),
    };

    let todo = todos
        .iter_mut()
        .find(|todo| todo.id == id)
        .ok_or_else(|| ApiError::not_found("todo not found"))?;
    if let Some(title) = &input.title {
        todo.title = validate_title(title)?;
    }
    if let Some(completed) = input.completed {
        todo.completed = completed;
    }
    todo.updated_at = Utc::now().to_rfc3339();
    let updated = todo.clone();

    state
        .forge
        .kv()
        .set(
            &todos_key(&session.user_id),
            Bytes::from(serde_json::to_vec(&todos)?),
            SetOpts::new(),
        )
        .await?;

    let audit = serde_json::json!({
        "userId": session.user_id,
        "action": "updated",
        "todoId": updated.id,
        "at": Utc::now().to_rfc3339(),
    });
    state
        .forge
        .queue()
        .enqueue(
            AUDIT_QUEUE,
            Bytes::from(audit.to_string()),
            EnqueueOpts::new()
                .with_max_attempts(3)
                .with_dedup_id(format!("updated:{}", updated.id)),
        )
        .await?;

    Ok(Json(updated))
}

#[delete("/api/todos/<id>")]
async fn delete_todo(
    state: &State<AppState>,
    auth: AuthHeader,
    id: &str,
) -> Result<Status, ApiError> {
    let Some(session) = state
        .forge
        .auth()
        .validate_session(&bearer_token(auth.0.as_deref())?)
        .await?
    else {
        return Err(ApiError::unauthorized());
    };

    let mut todos: Vec<Todo> = match state.forge.kv().get(&todos_key(&session.user_id)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => Vec::new(),
    };
    let before = todos.len();
    todos.retain(|todo| todo.id != id);
    if todos.len() == before {
        return Err(ApiError::not_found("todo not found"));
    }

    state
        .forge
        .kv()
        .set(
            &todos_key(&session.user_id),
            Bytes::from(serde_json::to_vec(&todos)?),
            SetOpts::new(),
        )
        .await?;

    let audit = serde_json::json!({
        "userId": session.user_id,
        "action": "deleted",
        "todoId": id,
        "at": Utc::now().to_rfc3339(),
    });
    state
        .forge
        .queue()
        .enqueue(
            AUDIT_QUEUE,
            Bytes::from(audit.to_string()),
            EnqueueOpts::new()
                .with_max_attempts(3)
                .with_dedup_id(format!("deleted:{id}")),
        )
        .await?;

    Ok(Status::NoContent)
}

#[get("/api/meta")]
async fn meta(state: &State<AppState>) -> Result<Json<MetaResponse>, ApiError> {
    let depth = state.forge.queue().depth(AUDIT_QUEUE).await?;
    let forge = state
        .forge
        .backend_capabilities()
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
        forge,
        audit_depth: AuditDepth {
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

#[options("/<_path..>")]
fn preflight(_path: PathBuf) -> Status {
    Status::NoContent
}

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
                list_todos,
                create_todo,
                update_todo,
                delete_todo,
                preflight,
            ],
        )
        .register("/", catchers![default_catcher])
}
