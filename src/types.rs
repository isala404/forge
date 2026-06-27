//! Value types shared across primitives.

/// Opaque pagination token: pass back what a `scan`/`list` returned; never construct or inspect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub(crate) String);

impl Cursor {
    /// The opaque token string. Exposed so a language binding can carry a cursor across
    /// FFI as a string; only pass it back via [`Cursor::from_token`].
    pub fn token(&self) -> &str {
        &self.0
    }

    /// Rebuild a cursor from a token returned by [`Cursor::token`]. Only a token a prior
    /// `scan`/`list` produced is valid; anything else yields an unspecified but safe next
    /// page.
    pub fn from_token(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}
