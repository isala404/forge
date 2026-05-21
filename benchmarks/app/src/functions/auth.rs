use forge::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::User;

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterInput {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user_id: Uuid,
}

fn sign_jwt(user_id: Uuid, secret: &str) -> Result<String> {
    let claims = serde_json::json!({
        "sub": user_id,
        "iat": chrono::Utc::now().timestamp(),
        "exp": (chrono::Utc::now() + chrono::Duration::days(7)).timestamp(),
    });

    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ForgeError::internal_with("JWT signing failed", e))
}

#[forge::mutation(auth = "none")]
pub async fn register(ctx: &MutationContext, input: RegisterInput) -> Result<AuthResponse> {
    let mut conn = ctx.conn().await?;

    let user = sqlx::query_as!(
        User,
        "INSERT INTO users (name) VALUES ($1) RETURNING id, name, created_at",
        &input.name
    )
    .fetch_one(&mut conn)
    .await
    .map_err(|e| {
        if e.to_string().contains("users_name_key") {
            ForgeError::Validation("Name already taken".into())
        } else {
            ForgeError::from(e)
        }
    })?;

    let secret = ctx.env_require("JWT_SECRET")?;
    let token = sign_jwt(user.id, &secret)?;

    Ok(AuthResponse {
        token,
        user_id: user.id,
    })
}
