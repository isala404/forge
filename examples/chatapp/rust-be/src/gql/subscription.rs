use std::time::{Duration, Instant};

use async_graphql::{Context, ID, Result, Subscription};
use forge::{Bytes, ForgeError};
use futures_util::{Stream, StreamExt};

use crate::context::{Ctx, CurrentUser, Event, PRESENCE_TOPIC, chat_topic};
use crate::db;
use crate::gql::helpers::{app, map_forge, me, parse_id, require_member};
use crate::gql::types::{GqlMessage, GqlReceipt, GqlUser, TypingEvent};

pub struct Subscription;

/// How often a long-lived subscription re-validates its principal's session.
const REVALIDATE_EVERY: Duration = Duration::from_secs(60);

/// Wrap a raw pubsub stream so it ENDS once the principal's session no longer
/// validates. Re-checks at most once per [`REVALIDATE_EVERY`] (gated on delivered
/// items), so the happy path pays one extra `validate_session` per minute at most.
/// API-key principals carry no session token and are never force-ended here.
fn guarded<S>(
    c: Ctx,
    user: CurrentUser,
    raw: S,
) -> impl Stream<Item = std::result::Result<Bytes, ForgeError>>
where
    S: Stream<Item = std::result::Result<Bytes, ForgeError>>,
{
    let mut checked_at = Instant::now();
    raw.take_while(move |_item| {
        let c = c.clone();
        let token = user.token.clone();
        let due = checked_at.elapsed() >= REVALIDATE_EVERY;
        if due {
            checked_at = Instant::now();
        }
        async move {
            // Only session-backed principals are re-validated; api keys have no token.
            if !due || token.is_empty() {
                return true;
            }
            matches!(c.forge.auth().validate_session(&token).await, Ok(Some(_)))
        }
    })
}

#[Subscription]
impl Subscription {
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

    async fn typing(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
    ) -> Result<impl Stream<Item = TypingEvent> + use<>> {
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
        let me_id = user.id;
        let raw = guarded(c.clone(), user, raw);
        Ok(raw.filter_map(move |item| {
            let c = c.clone();
            async move {
                let bytes = item.ok()?;
                match serde_json::from_slice::<Event>(&bytes).ok()? {
                    Event::Typing { user_id, typing } if user_id != me_id => {
                        let u = db::users_by_ids(&c.pool, &[user_id]).await.ok()?.pop()?;
                        Some(TypingEvent {
                            user: GqlUser(u),
                            typing,
                        })
                    }
                    _ => None,
                }
            }
        }))
    }

    async fn receipt_changed(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
    ) -> Result<impl Stream<Item = GqlReceipt> + use<>> {
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
                    Event::Receipt {
                        message_id,
                        user_id,
                    } => db::receipts_for_messages(&c.pool, &[message_id])
                        .await
                        .ok()?
                        .into_iter()
                        .find(|r| r.user_id == user_id)
                        .map(GqlReceipt),
                    _ => None,
                }
            }
        }))
    }

    async fn presence_changed(
        &self,
        ctx: &Context<'_>,
        user_ids: Vec<ID>,
    ) -> Result<impl Stream<Item = GqlUser> + use<>> {
        let c = app(ctx)?.clone();
        let user = me(ctx)?;
        let mut wanted = Vec::with_capacity(user_ids.len());
        for id in &user_ids {
            wanted.push(parse_id(id)?);
        }
        let raw = c
            .forge
            .pubsub()
            .subscribe(PRESENCE_TOPIC)
            .await
            .map_err(map_forge)?;
        let raw = guarded(c.clone(), user, raw);
        Ok(raw.filter_map(move |item| {
            let c = c.clone();
            let wanted = wanted.clone();
            async move {
                let bytes = item.ok()?;
                match serde_json::from_slice::<Event>(&bytes).ok()? {
                    Event::Presence { user_id, .. } if wanted.contains(&user_id) => {
                        db::users_by_ids(&c.pool, &[user_id])
                            .await
                            .ok()?
                            .pop()
                            .map(GqlUser)
                    }
                    _ => None,
                }
            }
        }))
    }
}
