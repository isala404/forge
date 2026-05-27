use crate::schema::{AuthResponse, User, UserPublic};
use forge::prelude::*;

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterInput {
    pub email: String,
    pub name: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
}

async fn auth_response(ctx: &MutationContext, user: &User) -> Result<AuthResponse> {
    let pair = ctx.issue_token_pair(user.id, &["user"]).await?;
    Ok(AuthResponse {
        access_token: pair.access_token,
        refresh_token: pair.refresh_token,
        user: UserPublic::from(user.clone()),
    })
}

fn validate_register(input: &RegisterInput) -> Result<(String, String)> {
    let email = input.email.trim();
    if email.is_empty() {
        return Err(ForgeError::Validation("Email is required".into()));
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ForgeError::Validation("Name is required".into()));
    }
    if input.password.len() < 8 {
        return Err(ForgeError::Validation(
            "Password must be at least 8 characters".into(),
        ));
    }
    Ok((email.to_string(), name.to_string()))
}

#[forge::mutation(auth = "none")]
pub async fn register(ctx: &MutationContext, input: RegisterInput) -> Result<AuthResponse> {
    let (email, name) = validate_register(&input)?;

    let password_hash = {
        use argon2::PasswordHasher;
        use password_hash::SaltString;
        let salt = SaltString::generate(&mut password_hash::rand_core::OsRng);
        argon2::Argon2::default()
            .hash_password(input.password.as_bytes(), &salt)
            .map_err(|e| ForgeError::internal(e.to_string()))?
            .to_string()
    };

    let id = Uuid::new_v4();
    let now = Utc::now();
    let mut conn = ctx.conn().await?;

    let user = sqlx::query_as!(
        User,
        r#"
        INSERT INTO users (id, email, name, password_hash, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, email, name, password_hash as "password_hash!", created_at, updated_at
        "#,
        id,
        &email,
        &name,
        &password_hash,
        now,
        now
    )
    .fetch_one(&mut conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) if db_err.constraint() == Some("idx_users_email") => {
            ForgeError::Validation("Email already registered".into())
        }
        _ => ForgeError::from(e),
    })?;

    auth_response(ctx, &user).await
}

#[forge::mutation(auth = "none")]
pub async fn login(ctx: &MutationContext, input: LoginInput) -> Result<AuthResponse> {
    let mut conn = ctx.conn().await?;

    let user = sqlx::query_as!(
        User,
        r#"
        SELECT id, email, name, password_hash as "password_hash!", created_at, updated_at
        FROM users WHERE email = $1
        "#,
        &input.email
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| ForgeError::Validation("Invalid email or password".into()))?;

    {
        use argon2::PasswordVerifier;
        let parsed = password_hash::PasswordHash::new(&user.password_hash)
            .map_err(|e| ForgeError::internal(e.to_string()))?;
        argon2::Argon2::default()
            .verify_password(input.password.as_bytes(), &parsed)
            .map_err(|_| ForgeError::Validation("Invalid email or password".into()))?;
    }

    auth_response(ctx, &user).await
}

#[forge::mutation(auth = "none")]
pub async fn refresh_token(ctx: &MutationContext, input: RefreshInput) -> Result<TokenPair> {
    ctx.rotate_refresh_token(&input.refresh_token).await
}

#[forge::query(scope = "global")]
pub async fn me(ctx: &QueryContext) -> Result<UserPublic> {
    let user_id = ctx.user_id()?;
    let user = sqlx::query_as!(
        User,
        r#"
        SELECT id, email, name, password_hash as "password_hash!", created_at, updated_at
        FROM users WHERE id = $1
        "#,
        user_id
    )
    .fetch_optional(ctx.db())
    .await?
    .ok_or_else(|| ForgeError::NotFound("User not found".into()))?;
    Ok(UserPublic::from(user))
}
