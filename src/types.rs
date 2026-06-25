//! Value types shared across primitives.

/// Opaque, backend-owned pagination token: callers pass back exactly what a `scan`/`list` returned and never construct or inspect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub(crate) String);

impl Cursor {
    /// The cursor as an opaque token string. It is still opaque — treat it as a
    /// black box and only pass it back via [`Cursor::from_token`]. Exposed so a
    /// language binding can carry a cursor across the FFI boundary as a string.
    pub fn token(&self) -> &str {
        &self.0
    }

    /// Rebuild a cursor from a token previously returned by [`Cursor::token`]. The
    /// only valid input is a token a prior `scan`/`list` produced; passing anything
    /// else yields an unspecified (but safe) next page.
    pub fn from_token(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}
