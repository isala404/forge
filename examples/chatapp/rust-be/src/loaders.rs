use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::Loader;
use uuid::Uuid;

use crate::context::AppCtx;
use crate::db::{self, MessageRow, ReceiptRow, UserRow};

pub struct AppLoader {
    pub ctx: Arc<AppCtx>,
}

#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct UserId(pub Uuid);
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct ChatMembersKey(pub Uuid);
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct LastMessageKey(pub Uuid);
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct UnreadKey {
    pub chat_id: Uuid,
    pub user_id: Uuid,
}
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct MessageReceiptsKey(pub Uuid);
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct PresenceKey(pub Uuid);

impl Loader<UserId> for AppLoader {
    type Value = UserRow;
    type Error = Arc<anyhow::Error>;

    async fn load(&self, keys: &[UserId]) -> Result<HashMap<UserId, UserRow>, Self::Error> {
        let ids: Vec<Uuid> = keys.iter().map(|k| k.0).collect();
        let rows = db::users_by_ids(&self.ctx.pool, &ids)
            .await
            .map_err(Arc::new)?;
        Ok(rows.into_iter().map(|u| (UserId(u.id), u)).collect())
    }
}

impl Loader<ChatMembersKey> for AppLoader {
    type Value = Vec<UserRow>;
    type Error = Arc<anyhow::Error>;

    async fn load(
        &self,
        keys: &[ChatMembersKey],
    ) -> Result<HashMap<ChatMembersKey, Vec<UserRow>>, Self::Error> {
        let ids: Vec<Uuid> = keys.iter().map(|k| k.0).collect();
        let rows = db::members_for_chats(&self.ctx.pool, &ids)
            .await
            .map_err(Arc::new)?;
        let mut out: HashMap<ChatMembersKey, Vec<UserRow>> = HashMap::new();
        for (chat_id, user) in rows {
            out.entry(ChatMembersKey(chat_id)).or_default().push(user);
        }
        Ok(out)
    }
}

impl Loader<LastMessageKey> for AppLoader {
    type Value = MessageRow;
    type Error = Arc<anyhow::Error>;

    async fn load(
        &self,
        keys: &[LastMessageKey],
    ) -> Result<HashMap<LastMessageKey, MessageRow>, Self::Error> {
        let ids: Vec<Uuid> = keys.iter().map(|k| k.0).collect();
        let rows = db::last_messages_for_chats(&self.ctx.pool, &ids)
            .await
            .map_err(Arc::new)?;
        Ok(rows
            .into_iter()
            .map(|m| (LastMessageKey(m.chat_id), m))
            .collect())
    }
}

impl Loader<MessageReceiptsKey> for AppLoader {
    type Value = Vec<ReceiptRow>;
    type Error = Arc<anyhow::Error>;

    async fn load(
        &self,
        keys: &[MessageReceiptsKey],
    ) -> Result<HashMap<MessageReceiptsKey, Vec<ReceiptRow>>, Self::Error> {
        let ids: Vec<Uuid> = keys.iter().map(|k| k.0).collect();
        let rows = db::receipts_for_messages(&self.ctx.pool, &ids)
            .await
            .map_err(Arc::new)?;
        let mut out: HashMap<MessageReceiptsKey, Vec<ReceiptRow>> = HashMap::new();
        for r in rows {
            out.entry(MessageReceiptsKey(r.message_id))
                .or_default()
                .push(r);
        }
        Ok(out)
    }
}

impl Loader<UnreadKey> for AppLoader {
    type Value = i32;
    type Error = Arc<anyhow::Error>;

    /// Unread is derived from receipts, not a kv counter. A request resolves `unread`
    /// for one viewer, so the keys share a `user_id`; we group by viewer (almost always
    /// one) and issue the batched count per group, defaulting absent chats to 0.
    async fn load(&self, keys: &[UnreadKey]) -> Result<HashMap<UnreadKey, i32>, Self::Error> {
        let mut by_viewer: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for k in keys {
            by_viewer.entry(k.user_id).or_default().push(k.chat_id);
        }
        let mut out: HashMap<UnreadKey, i32> = keys.iter().map(|k| (*k, 0)).collect();
        for (user_id, chat_ids) in by_viewer {
            let counts = db::unread_for_chats(&self.ctx.pool, user_id, &chat_ids)
                .await
                .map_err(Arc::new)?;
            for (chat_id, n) in counts {
                out.insert(UnreadKey { chat_id, user_id }, n);
            }
        }
        Ok(out)
    }
}

impl Loader<PresenceKey> for AppLoader {
    type Value = bool;
    type Error = Arc<anyhow::Error>;

    async fn load(&self, keys: &[PresenceKey]) -> Result<HashMap<PresenceKey, bool>, Self::Error> {
        let names: Vec<String> = keys.iter().map(|k| format!("online:{}", k.0)).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let values = self.ctx.forge.kv().mget(&refs).await.map_err(|e| {
            let e: anyhow::Error = e.into();
            Arc::new(e)
        })?;
        Ok(keys
            .iter()
            .zip(values)
            .map(|(k, v)| (*k, v.is_some()))
            .collect())
    }
}
