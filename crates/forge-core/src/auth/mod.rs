//! Authentication and JWT handling.
//!
//! Supports both symmetric (HMAC) and asymmetric (RSA via JWKS) JWT validation.
//! Tokens are validated on every request; claims are available in the function context.

mod claims;
pub mod role_resolver;
pub mod tokens;

pub use claims::{Claims, ClaimsBuilder};
pub use role_resolver::{
    DefaultRoleResolver, RoleResolver, SharedRoleResolver, default_role_resolver,
};
pub use tokens::TokenPair;
