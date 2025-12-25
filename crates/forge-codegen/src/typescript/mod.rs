/// TypeScript code generator for FORGE.
///
/// Generates TypeScript types, API bindings, and Svelte stores from Rust schema.
pub struct TypeScriptGenerator {
    /// Output directory for generated files.
    output_dir: std::path::PathBuf,
}

impl TypeScriptGenerator {
    /// Create a new TypeScript generator.
    pub fn new(output_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Generate all TypeScript artifacts.
    pub fn generate(&self) -> Result<(), crate::Error> {
        // TODO: Implement in Phase 11
        Ok(())
    }
}

/// Code generation error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),
}
