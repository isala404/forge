//! Shared application context, the realtime event envelope carried over Forge
//! `pubsub`, and the kv-backed presence helper.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use forgelib::{Bytes, Forge};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);
pub const SESSION_ABSOLUTE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub const PRESENCE_TOPIC: &str = "presence";
pub const FANOUT_QUEUE: &str = "fanout";
/// Queue whose worker always nacks, so a triggered job dead-letters into `fail.dlq`.
pub const FAIL_QUEUE: &str = "fail";
/// A message's expiry schedules a job here via `forge schedule`; its worker
/// hard-deletes the row and blob.
pub const REAP_QUEUE: &str = "reap";

#[derive(Clone)]
pub struct AppCtx {
    pub forge: Forge,
    pub pool: PgPool,
    /// A throwaway argon2id hash minted once at startup. `login` verifies the
    /// submitted password against it when the username doesn't exist, so the
    /// username-miss path spends the same argon2 time as a real verify and the
    /// timing no longer reveals which usernames are registered.
    pub decoy_hash: forgelib::PhcString,
}

pub type Ctx = Arc<AppCtx>;

/// The authenticated principal for a request/socket. Built once at the edge and
/// inserted into the GraphQL context.
#[derive(Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    /// Raw session token, kept so `logout` can revoke exactly this session. Empty
    /// when the principal authenticated with an API key.
    pub token: String,
}

/// Realtime events. Published as JSON; subscribers filter by `type` and re-read the
/// referenced row rather than trusting the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Message { message_id: Uuid },
    Typing { user_id: Uuid, typing: bool },
    Receipt { message_id: Uuid, user_id: Uuid },
    Presence { user_id: Uuid, online: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageJob {
    pub message_id: Uuid,
}

pub fn chat_topic(chat_id: Uuid) -> String {
    format!("chat:{chat_id}")
}

/// Presence "is online" TTL; the key expiring is what marks a user offline.
pub fn presence_ttl() -> Duration {
    env_secs("APP_PRESENCE_TTL_SECS", 30)
}

/// Disappearing-message lifetime, snapshotted onto the chat at toggle time.
pub fn disappearing_secs() -> i32 {
    std::env::var("APP_DISAPPEARING_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(86_400)
}

pub fn scheduler_interval() -> Duration {
    std::env::var("APP_SCHEDULER_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(30_000))
}

fn env_secs(key: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
}

impl AppCtx {
    /// Best-effort publish; logs and swallows failures so a pubsub hiccup never
    /// blocks the request path.
    pub async fn publish(&self, topic: &str, event: &Event) {
        match serde_json::to_vec(event) {
            Ok(bytes) => {
                if let Err(e) = self.forge.pubsub().publish(topic, Bytes::from(bytes)).await {
                    tracing::warn!(error = %e, topic, "pubsub publish failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "event serialize failed"),
        }
    }

    pub async fn touch_presence(&self, user_id: Uuid) -> Result<()> {
        self.forge
            .kv()
            .set(
                &format!("online:{user_id}"),
                Bytes::from_static(b"1"),
                forgelib::SetOpts::new().with_ttl(presence_ttl()),
            )
            .await?;
        Ok(())
    }

    pub async fn max_upload_bytes(&self) -> u64 {
        use forgelib::ConfigExt;
        self.forge
            .config()
            .get::<u64>("max_upload_bytes")
            .await
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES)
    }

    pub async fn set_reactions_rollout(&self, percent: u8) -> Result<()> {
        self.forge
            .config()
            .set_flag("reactions_v2", forgelib::FlagRule::Percent(percent))
            .await?;
        Ok(())
    }

    /// Evaluate the `reactions_v2` flag for a user (stable percentage bucketing keyed
    /// by user id). `flag` never errors; it falls back to the default on any failure.
    pub async fn reactions_enabled(&self, user_id: Uuid) -> bool {
        self.forge
            .config()
            .flag(
                "reactions_v2",
                false,
                &forgelib::EvalCtx::user(user_id.to_string()),
            )
            .await
    }

    pub async fn online_count(&self) -> i64 {
        self.forge
            .kv()
            .scan("online:", None, 1000)
            .await
            .map(|(keys, _)| keys.len() as i64)
            .unwrap_or(0)
    }

    pub async fn enqueue_failing(&self) -> Result<()> {
        self.forge
            .queue()
            .enqueue(
                FAIL_QUEUE,
                Bytes::from_static(b"boom"),
                forgelib::EnqueueOpts::new().with_max_attempts(1),
            )
            .await?;
        Ok(())
    }

    /// Gauge `fail.dlq` depth via the queue's depth primitive (a point-in-time
    /// estimate that doesn't touch the jobs it measures).
    pub async fn dlq_count(&self) -> i64 {
        let dlq = format!("{FAIL_QUEUE}.dlq");
        self.forge
            .queue()
            .depth(&dlq)
            .await
            .map(|d| d.total() as i64)
            .unwrap_or(0)
    }
}
