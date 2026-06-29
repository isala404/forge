use std::time::Duration;

use async_graphql::{Context, ID, Object, Result, Subscription};
use chrono::Utc;
use forgelib::Bytes;
use futures_util::{Stream, StreamExt};
use uuid::Uuid;

use crate::context::{Event, FANOUT_QUEUE, MessageJob, REAP_QUEUE, chat_topic, disappearing_secs};
use crate::db;
use crate::gql::helpers::{app, err, guarded, map_db, map_forge, me, parse_id, require_member};
use crate::gql::types::{GqlMessage, UploadTicket};

#[derive(Default)]
pub struct MessageQuery;

#[Object]
impl MessageQuery {
    async fn messages(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
        before: Option<chrono::DateTime<Utc>>,
        #[graphql(default = 50)] limit: i32,
    ) -> Result<Vec<GqlMessage>> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let limit = limit.clamp(1, 200) as i64;
        Ok(db::list_messages(&c.pool, cid, before, limit)
            .await
            .map_err(map_db)?
            .into_iter()
            .map(GqlMessage)
            .collect())
    }
}

#[derive(Default)]
pub struct MessageMutation;

#[Object]
impl MessageMutation {
    async fn request_upload(&self, ctx: &Context<'_>, chat_id: ID) -> Result<UploadTicket> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        // fail CLOSED before minting a presign.
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "upload",
                &user.id.to_string(),
                upload_limit(),
                forgelib::FailMode::Closed,
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

        // fail OPEN: a backend hiccup must not block messaging.
        let d = c
            .forge
            .ratelimit()
            .check_with(
                "send",
                &user.id.to_string(),
                send_limit(),
                forgelib::FailMode::Open,
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
        // original message instead of inserting a duplicate.
        if let Some(idem) = idempotency_key.as_deref().filter(|k| !k.is_empty()) {
            let key = format!("idem:send:{}:{}", user.id, idem);
            let won = c
                .forge
                .kv()
                .set(
                    &key,
                    Bytes::from(msg_id.to_string()),
                    forgelib::SetOpts::new()
                        .with_ttl(Duration::from_secs(86_400))
                        .with_mode(forgelib::SetMode::IfNotExists),
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
                .at(
                    when.into(),
                    REAP_QUEUE,
                    Bytes::from(payload),
                    forgelib::ScheduleOpts::new(),
                )
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
    ) -> Result<crate::gql::types::GqlChat> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        let seconds = enabled.then(disappearing_secs);
        db::set_disappearing(&c.pool, cid, seconds)
            .await
            .map_err(map_db)?;
        // Turning off recalls not-yet-expired messages.
        if !enabled {
            db::cancel_pending_reaps(&c.pool, cid)
                .await
                .map_err(map_db)?;
        }
        db::chat(&c.pool, cid)
            .await
            .map_err(map_db)?
            .map(crate::gql::types::GqlChat)
            .ok_or_else(|| err("NOT_FOUND", "chat not found"))
    }
}

#[derive(Default)]
pub struct MessageSubscription;

#[Subscription]
impl MessageSubscription {
    async fn message_added(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
    ) -> Result<impl Stream<Item = GqlMessage> + use<>> {
        let c = app(ctx)?.clone();
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(&c, cid, user.id).await?;
        let raw = c
            .forge
            .pubsub()
            .subscribe(&chat_topic(cid))
            .await
            .map_err(map_forge)?;
        let raw = guarded(c.clone(), user, raw);
        Ok(raw.filter_map(move |item| {
            let c = c.clone();
            async move {
                let bytes = item.ok()?;
                match serde_json::from_slice::<Event>(&bytes).ok()? {
                    Event::Message { message_id } => {
                        db::message(&c.pool, message_id).await.ok()?.map(GqlMessage)
                    }
                    _ => None,
                }
            }
        }))
    }
}

async fn enqueue_fanout(c: &crate::context::Ctx, message_id: Uuid) -> Result<()> {
    let payload = serde_json::to_vec(&MessageJob { message_id })
        .map_err(|e| err("BACKEND", e.to_string()))?;
    c.forge
        .queue()
        .enqueue(
            FANOUT_QUEUE,
            Bytes::from(payload),
            forgelib::EnqueueOpts::new().with_dedup_id(message_id.to_string()),
        )
        .await
        .map_err(map_forge)?;
    Ok(())
}

fn send_limit() -> forgelib::Limit {
    forgelib::Limit::per_duration(5, Duration::from_secs(10))
}

fn upload_limit() -> forgelib::Limit {
    forgelib::Limit::per_duration(30, Duration::from_secs(60))
}
