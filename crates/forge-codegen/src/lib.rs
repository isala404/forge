//! FORGE binding generation system.
//!
//! Two-phase pipeline with `forge.schema.json` as the contract:
//!
//! 1. **Phase 1** — Parser reads Rust source, produces `forge.schema.json`.
//! 2. **Phase 2** — Emitters read the schema, produce per-language client code.

mod binding;
pub mod dioxus;
mod emit;
pub mod parser;
pub mod schema_json;
pub mod typescript;

pub use dioxus::DioxusGenerator;
pub use parser::{ParseOutcome, find_duplicate_handlers, parse_project, validate_registry};
pub use schema_json::{emit as emit_schema, emit_string as emit_schema_json};
pub use typescript::{Error, GenerateOptions, RUNES_SVELTE_TS, TypeScriptGenerator};
