//! Test support: per-test isolated databases. Enabled by `feature = "pg-tests"`.
//!
//! Each [`TestDatabase`] creates a uniquely-named database (off `TEST_DATABASE_URL`,
//! never `DATABASE_URL`, so a test run can never touch a real database) and drops
//! it on `Drop`, even on panic, via a dedicated thread + runtime. The contract
//! test suites in `tests/` build a fresh [`crate::Forge`] per test on top of one.

// CREATE/DROP DATABASE take a dynamic database name and run against a database
// that does not exist yet, so the compile-time query macros cannot apply here.
#![allow(clippy::disallowed_methods)]

use crate::Forge;
use crate::error::{ForgeError, Result};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// A throwaway database for one test.
pub struct TestDatabase {
    /// Maintenance database URL, used to CREATE/DROP the throwaway one.
    admin_url: String,
    name: String,
    url: String,
}

impl TestDatabase {
    /// Create a fresh, uniquely-named database. Requires `TEST_DATABASE_URL`.
    pub async fn new() -> Result<Self> {
        let admin_url = std::env::var("TEST_DATABASE_URL").map_err(|_| {
            ForgeError::config(
                "TEST_DATABASE_URL is not set: DB-backed tests need a Postgres to create test databases against",
            )
        })?;
        // 12 hex chars of the UUID is plenty of uniqueness and keeps the db name short.
        let suffix = Uuid::new_v4().simple().to_string();
        let short = suffix.get(..12).unwrap_or(&suffix);
        let name = format!("forge_test_{short}");
        let url = with_database(&admin_url, &name);

        let mut conn = PgConnection::connect(&admin_url).await.map_err(|e| {
            ForgeError::config(format!("could not connect to TEST_DATABASE_URL: {e}"))
        })?;
        // db name is generated and safe; CREATE DATABASE cannot be parameterized.
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&mut conn)
            .await
            .map_err(|e| ForgeError::config(format!("could not create test database: {e}")))?;
        conn.close().await.ok();

        Ok(Self {
            admin_url,
            name,
            url,
        })
    }

    /// The connection string for this throwaway database.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// A `forge.toml` whose system database points at this throwaway database, with `extra`
    /// TOML appended. `extra` keys before any `[section]` header extend `[postgres]` (e.g.
    /// `"max_connections = 2\n"`); start a new section to set anything else (`"[forge]\n…"`).
    pub fn config_toml(&self, extra: &str) -> String {
        format!("[postgres]\nurl = \"{}\"\n{extra}", self.url)
    }

    /// Build a `Forge` against this database. `init` migrates the throwaway DB's schema.
    pub async fn forge(&self) -> Result<Forge> {
        self.forge_with("").await
    }

    /// Build a `Forge` against this database with `extra` TOML appended to the config (see
    /// [`config_toml`](Self::config_toml) for how `extra` composes).
    pub async fn forge_with(&self, extra: &str) -> Result<Forge> {
        Forge::init_from_str(&self.config_toml(extra)).await
    }

    /// Run a raw SQL statement against this database. Test setup only (e.g. seeding
    /// rows or auxiliary tables a test needs before `Forge::init`).
    pub async fn execute_raw(&self, sql: &str) -> Result<()> {
        let mut conn = PgConnection::connect(&self.url)
            .await
            .map_err(|e| ForgeError::config(format!("could not connect to test database: {e}")))?;
        let res = sqlx::query(sql)
            .execute(&mut conn)
            .await
            .map_err(ForgeError::from_sqlx)
            .map(|_| ());
        conn.close().await.ok();
        res
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let name = self.name.clone();
        // Drop on its own thread + runtime so it works regardless of the calling
        // context (including a panicking async test). FORCE terminates any
        // lingering connections (e.g. an Arc-held pool not yet closed).
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                if let Ok(mut conn) = PgConnection::connect(&admin_url).await {
                    let _ =
                        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
                            .execute(&mut conn)
                            .await;
                    conn.close().await.ok();
                }
            });
        })
        .join();
    }
}

/// Replace the database component of a Postgres URL, preserving any query string.
fn with_database(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let trimmed = base.trim_end_matches('/');
    let prefix = match trimmed.rfind('/') {
        Some(i) => trimmed.get(..i).unwrap_or(trimmed),
        None => trimmed,
    };
    let mut out = format!("{prefix}/{db}");
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::with_database;

    #[test]
    fn with_database_swaps_path_and_keeps_query() {
        assert_eq!(
            with_database("postgres://u:p@h:5432/forge_dev", "forge_test_abc"),
            "postgres://u:p@h:5432/forge_test_abc"
        );
        assert_eq!(
            with_database("postgres://u:p@h:5432/forge_dev?sslmode=disable", "t1"),
            "postgres://u:p@h:5432/t1?sslmode=disable"
        );
    }
}
