use std::time::Duration;

use async_graphql::{Context, ID, Object, Result};
use chrono::Utc;
use forge::{Bytes, PhcString, SessionOpts};
use uuid::Uuid;

use crate::context::{
    Ctx, CurrentUser, Event, FANOUT_QUEUE, MessageJob, PRESENCE_TOPIC, REAP_QUEUE,
    SESSION_ABSOLUTE, SESSION_IDLE, chat_topic, disappearing_secs,
};
use crate::db;
use crate::gql::helpers::{
    app, err, map_db, map_forge, me, parse_id, require_member, validate_credentials,
};
use crate::gql::types::{
    ApiKeyPayload, ChatKind, GqlChat, GqlMessage, GqlUser, SessionPayload, UploadTicket,
};

pub struct Mutation;

#[Object]
impl Mutation {
    async fn signup(
        &self,
        ctx: &Context<'_>,
        username: String,
        display_name: String,
        password: String,
    ) -> Result<SessionPayload> {
        let c = app(ctx)?;
        // Normalize first so " alice" and "alice" collapse to one bucket and one
        // stored user, and validate before spending a rate-limit token so invalid
        // input can't burn another user's bucket.
        let username = username.trim();
        validate_credentials(username, &password).map_err(|m| err("INVALID", m))?;
        // signup is abuse-sensitive: fail CLOSED (a backend error surfaces, never a free pass).
        let d = c
            .forge
            .ratelimit()
            .check_with("otp", username, otp_limit(), forge::FailMode::Closed)
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
            .check_with("otp", username, otp_limit(), forge::FailMode::Closed)
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "too many login attempts; try again later"));
        }
        let Some((user_id, hash)) = db::credentials(&c.pool, username).await.map_err(map_db)?
        else {
            return Err(err("UNAUTHENTICATED", "invalid username or password"));
        };
        let phc = PhcString::new(hash);
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

    async fn create_chat(
        &self,
        ctx: &Context<'_>,
        kind: ChatKind,
        title: Option<String>,
        member_usernames: Vec<String>,
    ) -> Result<GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let mut ids = vec![user.id];
        for uname in &member_usernames {
            let u = db::user_by_username(&c.pool, uname)
                .await
                .map_err(map_db)?
                .ok_or_else(|| err("NOT_FOUND", format!("no such user: {uname}")))?;
            if !ids.contains(&u.id) {
                ids.push(u.id);
            }
        }
        let kind_str = match kind {
            ChatKind::Direct => "direct",
            ChatKind::Group => "group",
        };
        if kind == ChatKind::Direct && ids.len() != 2 {
            return Err(err(
                "INVALID",
                "a direct chat needs exactly one other member",
            ));
        }
        let chat_id = db::create_chat(&c.pool, kind_str, title.as_deref(), user.id, &ids)
            .await
            .map_err(map_db)?;
        db::chat(&c.pool, chat_id)
            .await
            .map_err(map_db)?
            .map(GqlChat)
            .ok_or_else(|| err("BACKEND", "chat vanished after create"))
    }

    async fn add_member(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        username: String,
    ) -> Result<GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let u = db::user_by_username(&c.pool, &username)
            .await
            .map_err(map_db)?
            .ok_or_else(|| err("NOT_FOUND", format!("no such user: {username}")))?;
        db::add_member(&c.pool, cid, u.id).await.map_err(map_db)?;
        db::chat(&c.pool, cid)
            .await
            .map_err(map_db)?
            .map(GqlChat)
            .ok_or_else(|| err("NOT_FOUND", "chat not found"))
    }

    async fn request_upload(&self, ctx: &Context<'_>, chat_id: ID) -> Result<UploadTicket> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        // Presigns are abuse-sensitive: fail CLOSED before minting one.
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "upload",
                &user.id.to_string(),
                upload_limit(),
                forge::FailMode::Closed,
            )
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "too many upload requests; slow down"));
        }
        let max_bytes = c.max_upload_bytes().await;
        let key = format!("media/{cid}/{}", Uuid::new_v4());
        let url = c
            .forge
            .blob()
            .presign_upload(&key, Duration::from_secs(600), max_bytes)
            .await
            .map_err(map_forge)?;
        Ok(UploadTicket {
            key,
            upload_url: url,
            max_bytes: max_bytes.min(i32::MAX as u64) as i32,
        })
    }

    async fn send_message(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        body: String,
        media_key: Option<String>,
        idempotency_key: Option<String>,
    ) -> Result<GqlMessage> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;

        // Send is high-volume: fail OPEN (a backend hiccup must not block messaging).
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "send",
                &user.id.to_string(),
                send_limit(),
                forge::FailMode::Open,
            )
            .await
            .map_err(map_forge)?;
        if !d.allowed {
            return Err(err("LIMIT", "you are sending too fast; slow down"));
        }
        if body.trim().is_empty() && media_key.is_none() {
            return Err(err("INVALID", "message must have text or an attachment"));
        }

        let content_type = match &media_key {
            Some(k) => {
                // A media key is only usable in the chat it was minted for
                // (requestUpload mints `media/<chatId>/<uuid>`); reject any other.
                if !k.starts_with(&format!("media/{cid}/")) {
                    return Err(err("INVALID", "media_key does not belong to this chat"));
                }
                c.forge
                    .blob()
                    .head(k)
                    .await
                    .map_err(map_forge)?
                    .map(|i| i.content_type)
            }
            None => None,
        };
        let expires_at = db::chat(&c.pool, cid)
            .await
            .map_err(map_db)?
            .and_then(|ch| ch.disappearing_seconds)
            .map(|s| Utc::now() + chrono::Duration::seconds(s as i64));

        let msg_id = Uuid::new_v4();

        // Client-supplied idempotency: a resend after a lost response returns the
        // original message instead of inserting a duplicate. SET NX reserves the key
        // for the first send; a loser polls for the winner's just-inserted row.
        if let Some(idem) = idempotency_key.as_deref().filter(|k| !k.is_empty()) {
            let key = format!("idem:send:{}:{}", user.id, idem);
            let won = c
                .forge
                .kv()
                .set(
                    &key,
                    Bytes::from(msg_id.to_string()),
                    forge::SetOpts::new()
                        .with_ttl(Duration::from_secs(86_400))
                        .with_mode(forge::SetMode::IfNotExists),
                )
                .await
                .map_err(map_forge)?;
            if !won {
                for _ in 0..5 {
                    if let Some(existing) = c.forge.kv().get(&key).await.map_err(map_forge)?
                        && let Ok(existing_id) =
                            Uuid::parse_str(&String::from_utf8_lossy(&existing))
                        && let Some(row) =
                            db::message(&c.pool, existing_id).await.map_err(map_db)?
                    {
                        return Ok(GqlMessage(row));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                return Err(err("INVALID", "duplicate send in progress; retry"));
            }
        }

        db::insert_message(
            &c.pool,
            msg_id,
            cid,
            user.id,
            &body,
            media_key.as_deref(),
            content_type.as_deref(),
            expires_at,
        )
        .await
        .map_err(map_db)?;

        c.publish(&chat_topic(cid), &Event::Message { message_id: msg_id })
            .await;
        enqueue_fanout(c, msg_id).await?;

        if let Some(when) = expires_at {
            let payload = serde_json::to_vec(&MessageJob { message_id: msg_id })
                .map_err(|e| err("BACKEND", e.to_string()))?;
            c.forge
                .schedule()
                .at(when.into(), REAP_QUEUE, Bytes::from(payload))
                .await
                .map_err(map_forge)?;
        }

        db::message(&c.pool, msg_id)
            .await
            .map_err(map_db)?
            .map(GqlMessage)
            .ok_or_else(|| err("BACKEND", "message vanished after insert"))
    }

    async fn set_disappearing(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        enabled: bool,
    ) -> Result<GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let seconds = enabled.then(disappearing_secs);
        db::set_disappearing(&c.pool, cid, seconds)
            .await
            .map_err(map_db)?;
        // Turning off recalls not-yet-expired messages: clear their expiry so the
        // already-scheduled reap jobs find them no longer due and no-op.
        if !enabled {
            db::cancel_pending_reaps(&c.pool, cid)
                .await
                .map_err(map_db)?;
        }
        db::chat(&c.pool, cid)
            .await
            .map_err(map_db)?
            .map(GqlChat)
            .ok_or_else(|| err("NOT_FOUND", "chat not found"))
    }

    async fn set_typing(&self, ctx: &Context<'_>, chat_id: ID, typing: bool) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        // The indicator rides the pubsub `typing` event below; nothing reads a kv key.
        c.publish(
            &chat_topic(cid),
            &Event::Typing {
                user_id: user.id,
                typing,
            },
        )
        .await;
        Ok(true)
    }

    async fn mark_read(&self, ctx: &Context<'_>, chat_id: ID, message_id: ID) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        let mid = parse_id(&message_id)?;
        require_member(c, cid, user.id).await?;
        let updated = db::mark_read(&c.pool, cid, mid, user.id)
            .await
            .map_err(map_db)?;
        // No separate unread counter to clear: mark_read set receipts.read_at, which is
        // now the single source of truth for unread.
        if updated {
            c.publish(
                &chat_topic(cid),
                &Event::Receipt {
                    message_id: mid,
                    user_id: user.id,
                },
            )
            .await;
        }
        Ok(true)
    }

    async fn heartbeat(&self, ctx: &Context<'_>) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        c.touch_presence(user.id).await.map_err(map_db)?;
        c.publish(
            PRESENCE_TOPIC,
            &Event::Presence {
                user_id: user.id,
                online: true,
            },
        )
        .await;
        Ok(true)
    }

    async fn create_api_key(&self, ctx: &Context<'_>, label: String) -> Result<ApiKeyPayload> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        // Key minting is abuse-sensitive: fail CLOSED before issuing one.
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "apikey",
                &user.id.to_string(),
                apikey_limit(),
                forge::FailMode::Closed,
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

    async fn set_reactions_rollout(&self, ctx: &Context<'_>, percent: i32) -> Result<bool> {
        let c = app(ctx)?;
        me(ctx)?;
        c.set_reactions_rollout(percent.clamp(0, 100) as u8)
            .await
            .map_err(map_db)?;
        Ok(true)
    }

    async fn trigger_failing_job(&self, ctx: &Context<'_>) -> Result<bool> {
        let c = app(ctx)?;
        me(ctx)?;
        c.enqueue_failing().await.map_err(map_db)?;
        Ok(true)
    }
}

async fn issue_session(c: &Ctx, user_id: Uuid) -> Result<SessionPayload> {
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

async fn enqueue_fanout(c: &Ctx, message_id: Uuid) -> Result<()> {
    let payload = serde_json::to_vec(&MessageJob { message_id })
        .map_err(|e| err("BACKEND", e.to_string()))?;
    // Dedup on the message id so a retried sendMessage resolver can't double-enqueue.
    c.forge
        .queue()
        .enqueue(
            FANOUT_QUEUE,
            Bytes::from(payload),
            forge::EnqueueOpts::new().with_dedup_id(message_id.to_string()),
        )
        .await
        .map_err(map_forge)?;
    Ok(())
}

fn send_limit() -> forge::Limit {
    forge::Limit::per_duration(5, Duration::from_secs(10))
}

fn otp_limit() -> forge::Limit {
    forge::Limit::per_duration(10, Duration::from_secs(60))
}

fn upload_limit() -> forge::Limit {
    forge::Limit::per_duration(30, Duration::from_secs(60))
}

fn apikey_limit() -> forge::Limit {
    forge::Limit::per_duration(5, Duration::from_secs(3600))
}
