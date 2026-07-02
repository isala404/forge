use std::time::Duration;

use async_graphql::{Context, Enum, ID, Object, Result, SimpleObject};
use chrono::{DateTime, Utc};

use crate::context::CurrentUser;
use crate::db::{ChatRow, MessageRow, ReceiptRow, UserRow};
use crate::gql::helpers::{app, err, load_user, loader, map_forge};
use crate::loaders::{ChatMembersKey, LastMessageKey, MessageReceiptsKey, PresenceKey, UnreadKey};

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum ChatKind {
    Direct,
    Group,
}

pub struct GqlUser(pub UserRow);

#[Object(name = "User")]
impl GqlUser {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }
    async fn username(&self) -> &str {
        &self.0.username
    }
    async fn display_name(&self) -> &str {
        &self.0.display_name
    }
    async fn online(&self, ctx: &Context<'_>) -> Result<bool> {
        Ok(loader(ctx)?
            .load_one(PresenceKey(self.0.id))
            .await
            .map_err(|e| err("BACKEND", e.to_string()))?
            .unwrap_or(false))
    }
}

pub struct GqlChat(pub ChatRow);

#[Object(name = "Chat")]
impl GqlChat {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }
    async fn kind(&self) -> ChatKind {
        if self.0.kind == "group" {
            ChatKind::Group
        } else {
            ChatKind::Direct
        }
    }
    async fn title(&self) -> Option<&str> {
        self.0.title.as_deref()
    }
    async fn members(&self, ctx: &Context<'_>) -> Result<Vec<GqlUser>> {
        Ok(loader(ctx)?
            .load_one(ChatMembersKey(self.0.id))
            .await
            .map_err(|e| err("BACKEND", e.to_string()))?
            .unwrap_or_default()
            .into_iter()
            .map(GqlUser)
            .collect())
    }
    async fn last_message(&self, ctx: &Context<'_>) -> Result<Option<GqlMessage>> {
        Ok(loader(ctx)?
            .load_one(LastMessageKey(self.0.id))
            .await
            .map_err(|e| err("BACKEND", e.to_string()))?
            .map(GqlMessage))
    }
    async fn unread(&self, ctx: &Context<'_>) -> Result<i32> {
        let Some(user) = ctx.data_opt::<CurrentUser>() else {
            return Ok(0);
        };
        Ok(loader(ctx)?
            .load_one(UnreadKey {
                chat_id: self.0.id,
                user_id: user.id,
            })
            .await
            .map_err(|e| err("BACKEND", e.to_string()))?
            .unwrap_or(0))
    }
    async fn disappearing_seconds(&self) -> Option<i32> {
        self.0.disappearing_seconds
    }
}

pub struct GqlMessage(pub MessageRow);

#[Object(name = "Message")]
impl GqlMessage {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }
    async fn body(&self) -> &str {
        &self.0.body
    }
    async fn created_at(&self) -> DateTime<Utc> {
        self.0.created_at
    }
    async fn chat_id(&self) -> ID {
        ID(self.0.chat_id.to_string())
    }
    async fn sender(&self, ctx: &Context<'_>) -> Result<GqlUser> {
        load_user(ctx, self.0.sender_id).await.map(GqlUser)
    }
    async fn media(&self, ctx: &Context<'_>) -> Result<Option<Media>> {
        let Some(key) = self.0.media_key.as_deref() else {
            return Ok(None);
        };
        let url = app(ctx)?
            .forge
            .blob()
            .presign_download(key, Duration::from_secs(3600))
            .await
            .map_err(map_forge)?;
        Ok(Some(Media {
            key: key.to_string(),
            download_url: url,
            content_type: self.0.content_type.clone(),
        }))
    }
    async fn receipts(&self, ctx: &Context<'_>) -> Result<Vec<GqlReceipt>> {
        Ok(loader(ctx)?
            .load_one(MessageReceiptsKey(self.0.id))
            .await
            .map_err(|e| err("BACKEND", e.to_string()))?
            .unwrap_or_default()
            .into_iter()
            .map(GqlReceipt)
            .collect())
    }
}

#[derive(SimpleObject)]
pub struct Media {
    pub key: String,
    pub download_url: String,
    pub content_type: Option<String>,
}

pub struct GqlReceipt(pub ReceiptRow);

#[Object(name = "Receipt")]
impl GqlReceipt {
    async fn message_id(&self) -> ID {
        ID(self.0.message_id.to_string())
    }
    async fn user(&self, ctx: &Context<'_>) -> Result<GqlUser> {
        load_user(ctx, self.0.user_id).await.map(GqlUser)
    }
    async fn delivered_at(&self) -> Option<DateTime<Utc>> {
        self.0.delivered_at
    }
    async fn read_at(&self) -> Option<DateTime<Utc>> {
        self.0.read_at
    }
}

#[derive(SimpleObject)]
pub struct TypingEvent {
    pub user: GqlUser,
    pub typing: bool,
}

#[derive(SimpleObject)]
pub struct UploadTicket {
    pub key: String,
    pub upload_url: String,
    pub max_bytes: i32,
}

#[derive(SimpleObject)]
pub struct SessionPayload {
    pub token: String,
    pub user: GqlUser,
}

/// The `secret` is returned exactly once and never recoverable afterward.
#[derive(SimpleObject)]
pub struct ApiKeyPayload {
    pub id: String,
    pub secret: String,
}

#[derive(SimpleObject)]
pub struct OpsStats {
    pub online_count: i32,
    pub dlq_count: i32,
}
