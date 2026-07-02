use std::time::Duration;

use async_graphql::{Context, Object, Result};
use forgelib::SessionOpts;
use uuid::Uuid;

use crate::context::{Ctx, SESSION_ABSOLUTE, SESSION_IDLE};
use crate::db;
use crate::gql::helpers::{app, err, map_db, map_forge, me, validate_credentials};
use crate::gql::types::{ApiKeyPayload, GqlUser, SessionPayload};

#[derive(Default)]
pub struct AuthQuery;

#[Object]
impl AuthQuery {
    async fn me(&self, ctx: &Context<'_>) -> Result<Option<GqlUser>> {
        use crate::context::CurrentUser;
        use crate::gql::helpers::load_user;
        let Some(user) = ctx.data_opt::<CurrentUser>() else {
            return Ok(None);
        };
        Ok(load_user(ctx, user.id).await.ok().map(GqlUser))
    }
}

#[derive(Default)]
pub struct AuthMutation;

#[Object]
impl AuthMutation {
    async fn signup(
        &self,
        ctx: &Context<'_>,
        username: String,
        display_name: String,
        password: String,
    ) -> Result<SessionPayload> {
        let c = app(ctx)?;
        let username = username.trim();
        validate_credentials(username, &password).map_err(|m| err("INVALID", m))?;
        // fail CLOSED: a backend error surfaces, never a free pass.
        let d = c
            .forge
            .ratelimit()
            .check_with("otp", username, otp_limit(), forgelib::FailMode::Closed)
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "too many signup attempts; try again later"));
        }
        if db::username_taken(&c.pool, username)
            .await
            .map_err(map_db)?
        {
            return Err(err("PRECONDITION", "username already taken"));
        }
        let hash = c
            .forge
            .auth()
            .hash_password(&password)
            .await
            .map_err(map_forge)?;
        let user_id = db::create_user(&c.pool, username, &display_name, hash.as_str())
            .await
            .map_err(map_db)?;
        issue_session(c, user_id).await
    }

    async fn login(
        &self,
        ctx: &Context<'_>,
        username: String,
        password: String,
    ) -> Result<SessionPayload> {
        let c = app(ctx)?;
        let username = username.trim();
        let d = c
            .forge
            .ratelimit()
            .check_with("otp", username, otp_limit(), forgelib::FailMode::Closed)
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "too many login attempts; try again later"));
        }
        let Some((user_id, hash)) = db::credentials(&c.pool, username).await.map_err(map_db)?
        else {
            // Verify against the decoy so an unknown username costs the same argon2
            // time as a real one; otherwise the timing gap enumerates valid usernames.
            let _ = c
                .forge
                .auth()
                .verify_password(&password, &c.decoy_hash)
                .await;
            return Err(err("UNAUTHENTICATED", "invalid username or password"));
        };
        let phc = forgelib::PhcString::new(hash);
        let ok = c
            .forge
            .auth()
            .verify_password(&password, &phc)
            .await
            .map_err(map_forge)?;
        if !ok {
            return Err(err("UNAUTHENTICATED", "invalid username or password"));
        }
        // Transparently upgrade a hash minted under older argon2 params.
        if c.forge.auth().needs_rehash(&phc) {
            let fresh = c
                .forge
                .auth()
                .hash_password(&password)
                .await
                .map_err(map_forge)?;
            db::set_password_hash(&c.pool, user_id, fresh.as_str())
                .await
                .map_err(map_db)?;
        }
        issue_session(c, user_id).await
    }

    async fn logout(&self, ctx: &Context<'_>) -> Result<bool> {
        use crate::context::CurrentUser;
        let c = app(ctx)?;
        if let Some(user) = ctx.data_opt::<CurrentUser>()
            && !user.token.is_empty()
        {
            c.forge
                .auth()
                .revoke_session(&user.token)
                .await
                .map_err(map_forge)?;
        }
        Ok(true)
    }

    async fn logout_all(&self, ctx: &Context<'_>) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        c.forge
            .auth()
            .revoke_all_sessions(&user.id.to_string())
            .await
            .map_err(map_forge)?;
        Ok(true)
    }

    async fn create_api_key(&self, ctx: &Context<'_>, label: String) -> Result<ApiKeyPayload> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        // fail CLOSED before minting a key.
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "apikey",
                &user.id.to_string(),
                apikey_limit(),
                forgelib::FailMode::Closed,
            )
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "too many API keys created; try again later"));
        }
        let key = c
            .forge
            .auth()
            .create_api_key(&user.id.to_string(), &label)
            .await
            .map_err(map_forge)?;
        Ok(ApiKeyPayload {
            id: key.id,
            secret: key.secret.as_str().to_string(),
        })
    }
}

pub(super) async fn issue_session(c: &Ctx, user_id: Uuid) -> Result<SessionPayload> {
    let token = c
        .forge
        .auth()
        .create_session(
            &user_id.to_string(),
            SessionOpts::new()
                .with_idle_timeout(SESSION_IDLE)
                .with_absolute_timeout(SESSION_ABSOLUTE),
        )
        .await
        .map_err(map_forge)?;
    let user = db::users_by_ids(&c.pool, &[user_id])
        .await
        .map_err(map_db)?
        .into_iter()
        .next()
        .ok_or_else(|| err("BACKEND", "user vanished after create"))?;
    Ok(SessionPayload {
        token: token.as_str().to_string(),
        user: GqlUser(user),
    })
}

fn otp_limit() -> forgelib::Limit {
    forgelib::Limit::per_duration(10, Duration::from_secs(60))
}

fn apikey_limit() -> forgelib::Limit {
    forgelib::Limit::per_duration(5, Duration::from_secs(3600))
}
