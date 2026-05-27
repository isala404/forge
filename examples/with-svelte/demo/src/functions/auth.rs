use crate::schema::{AuthResponse, User, UserPublic, UserRole};
use forge::prelude::*;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RegisterInput {
    pub email: String,
    pub name: String,
    pub password: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
}

async fn auth_response(ctx: &MutationContext, user: &User) -> Result<AuthResponse> {
    let role = match user.role {
        UserRole::Admin => "admin",
        UserRole::Member => "user",
        UserRole::Guest => "guest",
    };
    let pair = ctx.issue_token_pair(user.id, &[role]).await?;
    Ok(AuthResponse {
        access_token: pair.access_token,
        refresh_token: pair.refresh_token,
        user: UserPublic::from(user.clone()),
    })
}

fn validate_register_input(input: &RegisterInput) -> Result<(String, String)> {
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
    let (email, name) = validate_register_input(&input)?;

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
        INSERT INTO users (id, email, name, role, password_hash, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING
            id, email, name,
            role as "role: UserRole",
            password_hash,
            created_at, updated_at
        "#,
        id,
        &email,
        &name,
        UserRole::Member as UserRole,
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
        SELECT
            id, email, name,
            role as "role: UserRole",
            password_hash,
            created_at, updated_at
        FROM users WHERE email = $1
        "#,
        &input.email
    )
    .fetch_optional(&mut conn)
    .await?
    .ok_or_else(|| ForgeError::Validation("Invalid email or password".into()))?;

    let hash = user
        .password_hash
        .as_deref()
        .ok_or_else(|| ForgeError::Validation("Invalid email or password".into()))?;

    let valid = {
        use argon2::PasswordVerifier;
        let parsed = password_hash::PasswordHash::new(hash)
            .map_err(|e| ForgeError::internal(e.to_string()))?;
        argon2::Argon2::default()
            .verify_password(input.password.as_bytes(), &parsed)
            .map_err(|_| ForgeError::Validation("Invalid email or password".into()))?;
        true
    };

    if !valid {
        return Err(ForgeError::Validation("Invalid email or password".into()));
    }

    auth_response(ctx, &user).await
}

#[forge::mutation(auth = "none")]
pub async fn refresh_token(ctx: &MutationContext, input: RefreshInput) -> Result<TokenPair> {
    ctx.rotate_refresh_token(&input.refresh_token).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register_input(email: &str, name: &str, password: &str) -> RegisterInput {
        RegisterInput {
            email: email.into(),
            name: name.into(),
            password: password.into(),
        }
    }

    #[test]
    fn test_validate_register_input_trims_values() {
        let (email, name) = validate_register_input(&register_input(
            "  ship@example.com  ",
            "  Release Pilot  ",
            "password123",
        ))
        .unwrap();

        assert_eq!(email, "ship@example.com");
        assert_eq!(name, "Release Pilot");
    }

    #[test]
    fn test_validate_register_input_rejects_blank_email() {
        let err =
            validate_register_input(&register_input("   ", "User", "password123")).unwrap_err();

        assert!(matches!(err, ForgeError::Validation(_)));
        assert!(err.to_string().contains("Email is required"));
    }

    #[test]
    fn test_validate_register_input_rejects_short_password() {
        let err = validate_register_input(&register_input("ship@example.com", "User", "short"))
            .unwrap_err();

        assert!(matches!(err, ForgeError::Validation(_)));
        assert!(
            err.to_string()
                .contains("Password must be at least 8 characters")
        );
    }
}
