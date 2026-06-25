use async_graphql::{Context, ID, Object, Result, Subscription};
use futures_util::{Stream, StreamExt};

use crate::context::{Event, chat_topic};
use crate::db;
use crate::gql::helpers::{app, guarded, map_db, me, parse_id, require_member};
use crate::gql::types::GqlReceipt;

#[derive(Default)]
pub struct ReceiptMutation;

#[Object]
impl ReceiptMutation {
    async fn mark_read(&self, ctx: &Context<'_>, chat_id: ID, message_id: ID) -> Result<bool> {
        let c = app(ctx)?;
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        let mid = parse_id(&message_id)?;
        require_member(c, cid, user.id).await?;
        let updated = db::mark_read(&c.pool, cid, mid, user.id).await.map_err(map_db)?;
        // mark_read set receipts.read_at, the single source of truth for unread.
        if updated {
            c.publish(&chat_topic(cid), &Event::Receipt { message_id: mid, user_id: user.id })
                .await;
        }
        Ok(true)
    }
}

#[derive(Default)]
pub struct ReceiptSubscription;

#[Subscription]
impl ReceiptSubscription {
    async fn receipt_changed(
        &self,
        ctx: &Context<'_>,
        chat_id: ID,
    ) -> Result<impl Stream<Item = GqlReceipt> + use<>> {
        let c = app(ctx)?.clone();
        let user = me(ctx)?;
        let cid = parse_id(&chat_id)?;
        require_member(&c, cid, user.id).await?;
        let raw = c.forge.pubsub().subscribe(&chat_topic(cid)).await.map_err(map_forge)?;
        let raw = guarded(c.clone(), user, raw);
        Ok(raw.filter_map(move |item| {
            let c = c.clone();
            async move {
                let bytes = item.ok()?;
                match serde_json::from_slice::<Event>(&bytes).ok()? {
                    Event::Receipt { message_id, user_id } => {
                        db::receipts_for_messages(&c.pool, &[message_id])
                            .await
                            .ok()?
                            .into_iter()
                            .find(|r| r.user_id == user_id)
                            .map(GqlReceipt)
                    }
                    _ => None,
                }
            }
        }))
    }
}

fn map_forge(e: forge::ForgeError) -> async_graphql::Error {
    crate::gql::helpers::map_forge(e)
}
