use async_graphql::{Context, ID, Object, Result, Subscription};
use futures_util::{Stream, StreamExt};

use crate::context::{Event, PRESENCE_TOPIC, chat_topic};
use crate::db;
use crate::gql::helpers::{app, guarded, map_db, me, parse_id, require_member};
use crate::gql::types::{GqlUser, TypingEvent};

#[derive(Default)]
pub struct PresenceQuery;

#[Object]
impl PresenceQuery {
    async fn presence(&self, ctx: &Context<'_>, user_ids: Vec<ID>) -> Result<Vec<GqlUser>> {
        let c = app(ctx)?;
        me(ctx)?;
        let mut ids = Vec::with_capacity(user_ids.len());
        for id in &user_ids {
            ids.push(parse_id(id)?);
        }
        Ok(db::users_by_ids(&c.pool, &ids)
            .await
            .map_err(map_db)?
            .into_iter()
            .map(GqlUser)
            .collect())
    }
}

#[derive(Default)]
pub struct PresenceMutation;

#[Object]
impl PresenceMutation {
    async fn set_typing(&self, ctx: &Context<'_>, chat_id: ID, typing: bool) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(c, cid, user.id).await?;
        // The indicator rides the pubsub `typing` event; nothing reads a kv key.
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
}

#[derive(Default)]
pub struct PresenceSubscription;

#[Subscription]
impl PresenceSubscription {
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

fn map_forge(e: forgelib::ForgeError) -> async_graphql::Error {
    crate::gql::helpers::map_forge(e)
}
