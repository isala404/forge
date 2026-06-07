//! Value types shared across primitives.

/// Opaque, backend-owned pagination token: callers pass back exactly what a `scan`/`list` returned and never construct or inspect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub(crate) String);

impl Cursor {
    pub(crate) fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
