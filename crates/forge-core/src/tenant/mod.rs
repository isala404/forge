use std::str::FromStr;

use uuid::Uuid;

use crate::ForgeError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TenantIsolationMode {
    #[default]
    None,
    Strict,
    ReadShared,
}

impl TenantIsolationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Strict => "strict",
            Self::ReadShared => "read_shared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTenantIsolationModeError(pub String);

impl std::fmt::Display for ParseTenantIsolationModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tenant isolation mode: '{}'", self.0)
    }
}

impl std::error::Error for ParseTenantIsolationModeError {}

impl FromStr for TenantIsolationMode {
    type Err = ParseTenantIsolationModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "strict" => Ok(Self::Strict),
            "read_shared" => Ok(Self::ReadShared),
            _ => Err(ParseTenantIsolationModeError(s.to_string())),
        }
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TenantContext {
    pub tenant_id: Option<Uuid>,
    pub isolation_mode: TenantIsolationMode,
}

impl TenantContext {
    pub fn none() -> Self {
        Self {
            tenant_id: None,
            isolation_mode: TenantIsolationMode::None,
        }
    }

    pub fn new(tenant_id: Uuid, isolation_mode: TenantIsolationMode) -> Self {
        Self {
            tenant_id: Some(tenant_id),
            isolation_mode,
        }
    }

    pub fn strict(tenant_id: Uuid) -> Self {
        Self::new(tenant_id, TenantIsolationMode::Strict)
    }

    pub fn has_tenant(&self) -> bool {
        self.tenant_id.is_some()
    }

    pub fn require_tenant(&self) -> crate::Result<Uuid> {
        self.tenant_id
            .ok_or_else(|| ForgeError::Unauthorized("Tenant context required".into()))
    }

    pub fn requires_filtering(&self) -> bool {
        self.tenant_id.is_some() && self.isolation_mode != TenantIsolationMode::None
    }

    /// Returns (SQL clause, param value) for tenant-scoped WHERE filtering.
    pub fn sql_filter(&self, column: &str, param_index: u32) -> Option<(String, Uuid)> {
        // Validate column name to prevent SQL injection via dynamic column names
        if column.is_empty()
            || !column
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return None;
        }
        self.tenant_id
            .map(|id| (format!("\"{}\" = ${}", column, param_index), id))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_tenant_context_none() {
        let ctx = TenantContext::none();
        assert!(!ctx.has_tenant());
        assert!(!ctx.requires_filtering());
    }

    #[test]
    fn test_tenant_context_strict() {
        let tenant_id = Uuid::new_v4();
        let ctx = TenantContext::strict(tenant_id);
        assert!(ctx.has_tenant());
        assert!(ctx.requires_filtering());
        assert_eq!(ctx.tenant_id, Some(tenant_id));
    }

    #[test]
    fn test_tenant_sql_filter() {
        let tenant_id = Uuid::new_v4();
        let ctx = TenantContext::strict(tenant_id);
        let filter = ctx.sql_filter("tenant_id", 1);
        assert!(filter.is_some());
        let (clause, id) = filter.unwrap();
        assert_eq!(clause, "\"tenant_id\" = $1");
        assert_eq!(id, tenant_id);
    }

    #[test]
    fn test_require_tenant() {
        let ctx = TenantContext::none();
        assert!(ctx.require_tenant().is_err());

        let tenant_id = Uuid::new_v4();
        let ctx = TenantContext::strict(tenant_id);
        assert!(ctx.require_tenant().is_ok());
    }

    #[test]
    fn sql_filter_rejects_injection_attempts() {
        let ctx = TenantContext::strict(Uuid::new_v4());
        // Anything outside [A-Za-z0-9_] must be refused so the column name can
        // never carry SQL. Empty is rejected too.
        for bad in [
            "",
            "tenant_id; DROP TABLE users",
            "tenant_id OR 1=1",
            "tenant_id--",
            "tenant\"_id",
            "tenant id",
            "tenant.id",
            "tenant_id)",
            "té",
        ] {
            assert!(
                ctx.sql_filter(bad, 1).is_none(),
                "column {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn sql_filter_accepts_valid_identifiers_and_quotes_them() {
        let tenant_id = Uuid::new_v4();
        let ctx = TenantContext::strict(tenant_id);
        let (clause, id) = ctx
            .sql_filter("org_id_2", 5)
            .expect("alphanumeric+underscore column is valid");
        assert_eq!(clause, "\"org_id_2\" = $5");
        assert_eq!(id, tenant_id);
    }

    #[test]
    fn sql_filter_returns_none_without_tenant_even_for_valid_column() {
        // No tenant id => nothing to scope by, regardless of column validity.
        let ctx = TenantContext::none();
        assert!(ctx.sql_filter("tenant_id", 1).is_none());
    }
}
