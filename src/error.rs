//! The public error type: [`ForgeError`] and [`Result`].
//!
//! The taxonomy is small and every variant says what the caller should do (retry, fix
//! config, treat as a caller bug); retryability is part of the contract. `Display` never
//! renders secrets, payloads, keys, or raw backend text. The underlying cause (which may
//! name constraints or schemas) stays reachable via [`std::error::Error::source`] for
//! logging but is never shown.

use thiserror::Error;

/// Forge's public error type. See `docs/contracts/*` for the per-primitive
/// mapping of failures onto these variants.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ForgeError {
    /// Misconfiguration: bad connection string, missing migration, malformed option. By
    /// principle 3 this can only occur during `forge::Forge::init`. Not retryable.
    #[error("configuration error: {0}")]
    Config(String),

    /// Transient backend outage (pool checkout timeout, dropped connection,
    /// Postgres `08xxx`/`57014`/`57P03`). Retryable.
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    /// The requested entity does not exist.
    #[error("not found")]
    NotFound,

    /// A precondition was not met: CAS mismatch, lease/fence lost, duplicate `dedup_id`,
    /// `SET NX` miss surfaced as an error path. Not retryable as-is; re-read state and decide.
    #[error("precondition failed: {0}")]
    Precondition(String),

    /// A size, length, or quota limit was exceeded. Not retryable.
    #[error("limit exceeded: {0}")]
    Limit(String),

    /// Caller-side bug: invalid argument, malformed key, out-of-range option.
    /// Not retryable.
    #[error("invalid input: {0}")]
    Invalid(String),

    /// A backend/SDK error that is none of the above. `Display` shows only `context`; the raw
    /// cause stays on `source()`, never rendered, so it is safe to surface. `retryable` carries
    /// the contract's per-error flag.
    #[error("backend error: {context}")]
    Backend {
        context: String,
        retryable: bool,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl ForgeError {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self::Unavailable(msg.into())
    }

    pub fn precondition(msg: impl Into<String>) -> Self {
        Self::Precondition(msg.into())
    }

    pub fn limit(msg: impl Into<String>) -> Self {
        Self::Limit(msg.into())
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }

    /// Construct a non-retryable [`ForgeError::Backend`] with no source.
    pub fn backend(context: impl Into<String>) -> Self {
        Self::Backend {
            context: context.into(),
            retryable: false,
            source: None,
        }
    }

    /// Construct a [`ForgeError::Backend`] carrying a source cause. The source is
    /// preserved for logging but never rendered by `Display`.
    pub fn backend_with(
        context: impl Into<String>,
        retryable: bool,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            context: context.into(),
            retryable,
            source: Some(Box::new(source)),
        }
    }

    /// Whether retrying the operation might succeed. Part of the contract.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Unavailable(_) => true,
            Self::Backend { retryable, .. } => *retryable,
            _ => false,
        }
    }

    /// Classify a [`sqlx::Error`] into the taxonomy without leaking its text.
    pub fn from_sqlx(err: sqlx::Error) -> Self {
        if is_transient_sqlx_error(&err) {
            // Raw error omitted: it can name constraints or schemas, and the caller needs
            // only the retryable signal.
            Self::Unavailable("database temporarily unavailable".to_string())
        } else {
            Self::Backend {
                context: "database error".to_string(),
                retryable: false,
                source: Some(Box::new(err)),
            }
        }
    }
}

/// `?` on a `sqlx::Error` produces a classified, secret-safe [`ForgeError`].
impl From<sqlx::Error> for ForgeError {
    fn from(err: sqlx::Error) -> Self {
        Self::from_sqlx(err)
    }
}

/// Transient sqlx failures that are safe to retry: pool checkout timeouts, dropped or closed
/// connections, IO errors on the database socket. Logical errors (constraint violations, type
/// mismatches, missing rows) do not retry.
fn is_transient_sqlx_error(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Io(_) => true,
        sqlx::Error::Database(db_err) => db_err
            .code()
            // Transient SQLSTATEs: connection_exception (08xxx),
            // statement_timeout (57014), cannot_connect_now (57P03).
            .map(|c| c.starts_with("08") || c == "57014" || c == "57P03")
            .unwrap_or(false),
        _ => false,
    }
}

pub type Result<T> = std::result::Result<T, ForgeError>;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn display_is_generic_and_secret_free() {
        assert_eq!(
            ForgeError::config("bad dsn").to_string(),
            "configuration error: bad dsn"
        );
        assert_eq!(ForgeError::NotFound.to_string(), "not found");
        assert_eq!(
            ForgeError::precondition("lease lost").to_string(),
            "precondition failed: lease lost"
        );
        assert_eq!(
            ForgeError::limit("value > 1 MiB").to_string(),
            "limit exceeded: value > 1 MiB"
        );
        assert_eq!(
            ForgeError::invalid("key contains ':'").to_string(),
            "invalid input: key contains ':'"
        );
    }

    #[test]
    fn backend_display_hides_source() {
        let secret = std::io::Error::other("table forge_secret_tokens constraint uq_x");
        let err = ForgeError::backend_with("database error", false, secret);
        assert_eq!(err.to_string(), "backend error: database error");
        assert!(err.source().is_some());
        assert!(
            err.source()
                .expect("source present")
                .to_string()
                .contains("forge_secret_tokens")
        );
    }

    #[test]
    fn retryability_matches_contract() {
        assert!(ForgeError::unavailable("x").is_retryable());
        assert!(!ForgeError::NotFound.is_retryable());
        assert!(!ForgeError::invalid("x").is_retryable());
        assert!(!ForgeError::limit("x").is_retryable());
        assert!(!ForgeError::precondition("x").is_retryable());
        assert!(ForgeError::backend_with("x", true, std::io::Error::other("e")).is_retryable());
        assert!(!ForgeError::backend("x").is_retryable());
    }

    /// Guards against drift between the `ForgeError` enum and the code table in
    /// `docs/contracts/errors.md`: a new variant won't compile (the match is exhaustive), a
    /// renamed or removed doc row fails the set check, and a changed `is_retryable()` the doc
    /// didn't follow fails the per-row check.
    #[test]
    fn errors_md_table_matches_the_enum() {
        // Exhaustive: adding a variant forces a new arm here (and, via the assertions,
        // a new errors.md row).
        fn code(e: &ForgeError) -> &'static str {
            match e {
                ForgeError::Config(_) => "Config",
                ForgeError::Unavailable(_) => "Unavailable",
                ForgeError::NotFound => "NotFound",
                ForgeError::Precondition(_) => "Precondition",
                ForgeError::Limit(_) => "Limit",
                ForgeError::Invalid(_) => "Invalid",
                ForgeError::Backend { .. } => "Backend",
            }
        }
        let samples = [
            ForgeError::config("c"),
            ForgeError::unavailable("u"),
            ForgeError::NotFound,
            ForgeError::precondition("p"),
            ForgeError::limit("l"),
            ForgeError::invalid("i"),
            ForgeError::backend("b"),
        ];

        let doc = include_str!("../docs/contracts/errors.md");
        // The first backtick-quoted token of each table data row is its canonical code.
        let doc_rows: Vec<(String, String)> = doc
            .lines()
            .filter(|l| l.trim_start().starts_with("| `"))
            .filter_map(|l| {
                let cells: Vec<&str> = l.split('|').map(str::trim).collect();
                // cells: ["", code, rust, node, python, retryable, meaning, ""]
                let code = cells.get(1)?.trim_matches('`').to_string();
                let retryable = cells.get(5)?.replace('*', "").to_lowercase();
                Some((code, retryable))
            })
            .collect();

        let doc_codes: std::collections::BTreeSet<&str> =
            doc_rows.iter().map(|(c, _)| c.as_str()).collect();
        let enum_codes: std::collections::BTreeSet<&str> = samples.iter().map(code).collect();
        assert_eq!(
            doc_codes, enum_codes,
            "errors.md code set must equal the ForgeError variant set"
        );

        for e in &samples {
            let c = code(e);
            let (_, doc_retryable) = doc_rows
                .iter()
                .find(|(rc, _)| rc == c)
                .unwrap_or_else(|| panic!("errors.md has no row for {c}"));
            match doc_retryable.as_str() {
                // Backend carries a per-error flag, so the doc says "per-error".
                "per-error" => assert_eq!(c, "Backend"),
                "yes" => assert!(e.is_retryable(), "{c}: doc says retryable, enum says not"),
                "no" => assert!(
                    !e.is_retryable(),
                    "{c}: doc says not retryable, enum says it is"
                ),
                other => panic!("{c}: unrecognized retryable cell {other:?}"),
            }
        }
    }

    #[test]
    fn error_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ForgeError>();
        assert_sync::<ForgeError>();
    }

    #[test]
    fn transient_sqlx_classifies_to_unavailable() {
        let e = ForgeError::from_sqlx(sqlx::Error::PoolTimedOut);
        assert!(matches!(e, ForgeError::Unavailable(_)));
        assert!(e.is_retryable());
    }

    #[test]
    fn non_transient_sqlx_classifies_to_backend() {
        let e = ForgeError::from_sqlx(sqlx::Error::RowNotFound);
        assert!(matches!(
            e,
            ForgeError::Backend {
                retryable: false,
                ..
            }
        ));
        assert!(!e.is_retryable());
    }

    #[test]
    fn transient_sqlx_pool_and_worker_errors_retry() {
        assert!(is_transient_sqlx_error(&sqlx::Error::PoolTimedOut));
        assert!(is_transient_sqlx_error(&sqlx::Error::PoolClosed));
        assert!(is_transient_sqlx_error(&sqlx::Error::WorkerCrashed));
    }

    #[test]
    fn transient_sqlx_io_error_retries() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        assert!(is_transient_sqlx_error(&sqlx::Error::Io(io)));
    }

    #[test]
    fn transient_sqlx_connection_family_and_timeouts_retry() {
        assert!(is_transient_sqlx_error(&db(FakeDbError::with_code(
            "08006"
        ))));
        assert!(is_transient_sqlx_error(&db(FakeDbError::with_code(
            "08003"
        ))));
        assert!(is_transient_sqlx_error(&db(FakeDbError::with_code(
            "57014"
        ))));
        assert!(is_transient_sqlx_error(&db(FakeDbError::with_code(
            "57P03"
        ))));
    }

    #[test]
    fn non_transient_sqlx_logical_errors_do_not_retry() {
        assert!(!is_transient_sqlx_error(&db(FakeDbError::with_code(
            "23505"
        )))); // unique_violation
        assert!(!is_transient_sqlx_error(&db(FakeDbError::with_code(
            "23503"
        )))); // fk_violation
        assert!(!is_transient_sqlx_error(&db(FakeDbError::with_code(
            "57000"
        )))); // operator_intervention
        assert!(!is_transient_sqlx_error(&db(FakeDbError::no_code())));
        assert!(!is_transient_sqlx_error(&sqlx::Error::RowNotFound));
    }

    fn db(err: FakeDbError) -> sqlx::Error {
        sqlx::Error::Database(Box::new(err))
    }

    /// Minimal `sqlx::error::DatabaseError` carrying a fixed SQLSTATE so the
    /// `Database` arm of the classifier can be driven without a live Postgres.
    #[derive(Debug)]
    struct FakeDbError {
        code: Option<String>,
    }

    impl FakeDbError {
        fn with_code(code: &str) -> Self {
            Self {
                code: Some(code.to_string()),
            }
        }
        fn no_code() -> Self {
            Self { code: None }
        }
    }

    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "fake db error ({:?})", self.code)
        }
    }

    impl std::error::Error for FakeDbError {}

    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "fake db error"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            self.code.as_deref().map(std::borrow::Cow::Borrowed)
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }
}
