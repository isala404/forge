use std::sync::Arc;

use crate::function::AuthContext;

/// Extension point for role resolution.
///
/// The default implementation returns the flat `roles` JWT claim.
/// Register a custom resolver via `ForgeBuilder::with_role_resolver` for
/// hierarchy expansion, group lookups, or remote permission services.
///
/// Called once per `require_role` check. Keep implementations cheap — the
/// result is not cached between calls.
pub trait RoleResolver: Send + Sync + 'static {
    fn resolve(&self, auth: &AuthContext) -> Vec<String>;
}

/// Default resolver — returns the `roles` JWT claim as-is.
pub struct DefaultRoleResolver;

impl RoleResolver for DefaultRoleResolver {
    fn resolve(&self, auth: &AuthContext) -> Vec<String> {
        auth.roles().to_vec()
    }
}

/// Shared resolver handle used throughout the runtime.
pub type SharedRoleResolver = Arc<dyn RoleResolver>;

/// Create a shared handle to the default resolver.
pub fn default_role_resolver() -> SharedRoleResolver {
    Arc::new(DefaultRoleResolver)
}
