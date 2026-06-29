use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

#[derive(Clone, sqlx::FromRow)]
pub struct ChatRow {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub disappearing_seconds: Option<i32>,
}

#[derive(Clone, sqlx::FromRow)]
pub struct MessageRow {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub sender_id: Uuid,
    pub body: String,
    pub media_key: Option<String>,
    pub content_type: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, sqlx::FromRow)]
pub struct ReceiptRow {
    pub message_id: Uuid,
    pub user_id: Uuid,
    pub delivered_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(include_str!("../migrations.sql"))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn create_user(
    pool: &PgPool,
    username: &str,
    display_name: &str,
    password_hash: &str,
) -> Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, display_name, password_hash) VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(username)
    .bind(display_name)
    .bind(password_hash)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

pub async fn username_taken(pool: &PgPool, username: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
            .bind(username)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn credentials(pool: &PgPool, username: &str) -> Result<Option<(Uuid, String)>> {
    Ok(
        sqlx::query_as("SELECT id, password_hash FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(pool)
            .await?,
    )
}

/// Replace a user's stored password hash, e.g. after a transparent rehash on login.
pub async fn set_password_hash(pool: &PgPool, id: Uuid, password_hash: &str) -> Result<()> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn user_by_username(pool: &PgPool, username: &str) -> Result<Option<UserRow>> {
    Ok(sqlx::query_as::<_, UserRow>(
        "SELECT id, username, display_name FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?)
}

pub async fn users_by_ids(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<UserRow>> {
    Ok(sqlx::query_as::<_, UserRow>(
        "SELECT id, username, display_name FROM users WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?)
}

pub async fn is_member(pool: &PgPool, chat_id: Uuid, user_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM chat_members WHERE chat_id = $1 AND user_id = $2)",
    )
    .bind(chat_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?)
}

/// Chats the user belongs to, most-recent activity first (last message, else creation).
pub async fn chats_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<ChatRow>> {
    Ok(sqlx::query_as::<_, ChatRow>(
        "SELECT c.id, c.kind, c.title, c.disappearing_seconds \
         FROM chats c \
         JOIN chat_members m ON m.chat_id = c.id \
         LEFT JOIN LATERAL ( \
            SELECT max(created_at) AS last_at FROM messages \
            WHERE chat_id = c.id AND (expires_at IS NULL OR expires_at > now()) \
         ) lm ON true \
         WHERE m.user_id = $1 \
         ORDER BY COALESCE(lm.last_at, c.created_at) DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn chat(pool: &PgPool, id: Uuid) -> Result<Option<ChatRow>> {
    Ok(sqlx::query_as::<_, ChatRow>(
        "SELECT id, kind, title, disappearing_seconds FROM chats WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// `(chat_id, UserRow)` pairs for the given chats, ordered by display name within a
/// chat. Backs the `Chat.members` DataLoader.
pub async fn members_for_chats(pool: &PgPool, chat_ids: &[Uuid]) -> Result<Vec<(Uuid, UserRow)>> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
        "SELECT m.chat_id, u.id, u.username, u.display_name \
         FROM chat_members m JOIN users u ON u.id = m.user_id \
         WHERE m.chat_id = ANY($1) ORDER BY u.display_name",
    )
    .bind(chat_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(cid, id, username, display_name)| {
            (
                cid,
                UserRow {
                    id,
                    username,
                    display_name,
                },
            )
        })
        .collect())
}

/// The newest live message per chat. Backs the `Chat.lastMessage` DataLoader.
pub async fn last_messages_for_chats(pool: &PgPool, chat_ids: &[Uuid]) -> Result<Vec<MessageRow>> {
    Ok(sqlx::query_as::<_, MessageRow>(
        "SELECT DISTINCT ON (chat_id) \
            id, chat_id, sender_id, body, media_key, content_type, created_at \
         FROM messages \
         WHERE chat_id = ANY($1) AND (expires_at IS NULL OR expires_at > now()) \
         ORDER BY chat_id, created_at DESC",
    )
    .bind(chat_ids)
    .fetch_all(pool)
    .await?)
}

pub async fn create_chat(
    pool: &PgPool,
    kind: &str,
    title: Option<&str>,
    created_by: Uuid,
    member_ids: &[Uuid],
) -> Result<Uuid> {
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO chats (kind, title, created_by) VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(kind)
    .bind(title)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    for uid in member_ids {
        sqlx::query(
            "INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2) \
             ON CONFLICT (chat_id, user_id) DO NOTHING",
        )
        .bind(id)
        .bind(uid)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn add_member(pool: &PgPool, chat_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query(
        "INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2) \
         ON CONFLICT (chat_id, user_id) DO NOTHING",
    )
    .bind(chat_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a message and the unread receipts for the chat's other members in one
/// transaction, so a crash can't orphan a message without its receipts. Returns
/// the new message id.
#[allow(clippy::too_many_arguments)]
pub async fn insert_message(
    pool: &PgPool,
    id: Uuid,
    chat_id: Uuid,
    sender_id: Uuid,
    body: &str,
    media_key: Option<&str>,
    content_type: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Uuid> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO messages (id, chat_id, sender_id, body, media_key, content_type, expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(chat_id)
    .bind(sender_id)
    .bind(body)
    .bind(media_key)
    .bind(content_type)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    let recipients: Vec<(Uuid,)> =
        sqlx::query_as("SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2")
            .bind(chat_id)
            .bind(sender_id)
            .fetch_all(&mut *tx)
            .await?;
    let recipient_ids: Vec<Uuid> = recipients.into_iter().map(|r| r.0).collect();
    sqlx::query(
        "INSERT INTO receipts (message_id, user_id) \
         SELECT $1, unnest($2::uuid[]) ON CONFLICT (message_id, user_id) DO NOTHING",
    )
    .bind(id)
    .bind(&recipient_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn message(pool: &PgPool, id: Uuid) -> Result<Option<MessageRow>> {
    Ok(sqlx::query_as::<_, MessageRow>(
        "SELECT id, chat_id, sender_id, body, media_key, content_type, created_at \
         FROM messages WHERE id = $1 AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// A newest-first window of live messages, `before` an exclusive `created_at` cursor.
pub async fn list_messages(
    pool: &PgPool,
    chat_id: Uuid,
    before: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<MessageRow>> {
    Ok(sqlx::query_as::<_, MessageRow>(
        "SELECT id, chat_id, sender_id, body, media_key, content_type, created_at \
         FROM messages \
         WHERE chat_id = $1 \
           AND (expires_at IS NULL OR expires_at > now()) \
           AND ($2::timestamptz IS NULL OR created_at < $2) \
         ORDER BY created_at DESC LIMIT $3",
    )
    .bind(chat_id)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn mark_delivered(pool: &PgPool, message_id: Uuid, user_id: Uuid) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE receipts SET delivered_at = now() \
         WHERE message_id = $1 AND user_id = $2 AND delivered_at IS NULL",
    )
    .bind(message_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Mark the user's receipt read, scoped to a message actually in `chat_id` so a
/// member of one chat can't flip a receipt on a message in another. Returns
/// whether a receipt row was updated.
pub async fn mark_read(
    pool: &PgPool,
    chat_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE receipts SET read_at = COALESCE(read_at, now()), \
            delivered_at = COALESCE(delivered_at, now()) \
         WHERE message_id = $1 AND user_id = $2 \
           AND EXISTS (SELECT 1 FROM messages WHERE id = $1 AND chat_id = $3)",
    )
    .bind(message_id)
    .bind(user_id)
    .bind(chat_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Receipts grouped by message. Backs the `Message.receipts` DataLoader.
pub async fn receipts_for_messages(pool: &PgPool, message_ids: &[Uuid]) -> Result<Vec<ReceiptRow>> {
    Ok(sqlx::query_as::<_, ReceiptRow>(
        "SELECT message_id, user_id, delivered_at, read_at \
         FROM receipts WHERE message_id = ANY($1)",
    )
    .bind(message_ids)
    .fetch_all(pool)
    .await?)
}

/// Per-chat unread counts for a viewer, derived from receipts: a chat's unread is
/// the count of the viewer's receipts still `read_at IS NULL` on live messages.
/// Returns `(chat_id, n)` only for chats with a non-zero count; the loader maps the
/// rest to 0. Backs the `Chat.unread` DataLoader.
pub async fn unread_for_chats(
    pool: &PgPool,
    viewer_id: Uuid,
    chat_ids: &[Uuid],
) -> Result<Vec<(Uuid, i32)>> {
    Ok(sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT m.chat_id AS chat_id, count(*)::int AS n \
         FROM receipts r JOIN messages m ON m.id = r.message_id \
         WHERE r.user_id = $1 AND r.read_at IS NULL \
           AND (m.expires_at IS NULL OR m.expires_at > now()) \
           AND m.chat_id = ANY($2) \
         GROUP BY m.chat_id",
    )
    .bind(viewer_id)
    .bind(chat_ids)
    .fetch_all(pool)
    .await?)
}

pub async fn other_member_ids(pool: &PgPool, chat_id: Uuid, sender: Uuid) -> Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2")
            .bind(chat_id)
            .bind(sender)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn set_disappearing(pool: &PgPool, chat_id: Uuid, seconds: Option<i32>) -> Result<()> {
    sqlx::query("UPDATE chats SET disappearing_seconds = $2 WHERE id = $1")
        .bind(chat_id)
        .bind(seconds)
        .execute(pool)
        .await?;
    Ok(())
}

/// Recall pending disappearances: clear `expires_at` on not-yet-expired messages in a
/// chat. Called when disappearing is turned OFF, so the already-scheduled reap jobs
/// find the row no longer due and no-op instead of deleting it.
pub async fn cancel_pending_reaps(pool: &PgPool, chat_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE messages SET expires_at = NULL \
         WHERE chat_id = $1 AND expires_at IS NOT NULL AND expires_at > now()",
    )
    .bind(chat_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `(media_key, expires_at)` for a message, ignoring the live filter so the reap
/// worker can see an already-expired (or recalled) row. `None` if the row is gone.
pub async fn message_reap_info(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<(Option<String>, Option<DateTime<Utc>>)>> {
    Ok(
        sqlx::query_as("SELECT media_key, expires_at FROM messages WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Hard-delete a message only if it is actually due (`expires_at <= now()`).
/// Idempotent: an already-gone or recalled message deletes nothing.
pub async fn delete_expired_message(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM messages WHERE id = $1 AND expires_at <= now()")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Due disappearing messages (and their media keys) for the reconciliation sweep to
/// self-heal: any reap whose post-commit enqueue was dropped. Bounded.
pub async fn due_messages(pool: &PgPool, limit: i64) -> Result<Vec<(Uuid, Option<String>)>> {
    Ok(
        sqlx::query_as("SELECT id, media_key FROM messages WHERE expires_at <= now() LIMIT $1")
            .bind(limit)
            .fetch_all(pool)
            .await?,
    )
}

/// Live messages with at least one never-delivered receipt, older than a grace window:
/// fanout that was likely never enqueued. The sweep re-enqueues fanout for these
/// (idempotent on `mark_delivered`). Bounded.
pub async fn undelivered_message_ids(pool: &PgPool, limit: i64) -> Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT DISTINCT m.id FROM messages m JOIN receipts r ON r.message_id = m.id \
         WHERE r.delivered_at IS NULL AND m.created_at < now() - interval '30 seconds' \
           AND (m.expires_at IS NULL OR m.expires_at > now()) LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
