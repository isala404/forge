use chrono::{DateTime, Utc};
use forge_core::schema::TableDef;

use super::diff::{DatabaseTable, SchemaDiff};

/// Generates SQL migrations from schema changes.
pub struct MigrationGenerator {
    /// Output directory for migrations.
    output_dir: std::path::PathBuf,
}

impl MigrationGenerator {
    /// Create a new migration generator.
    pub fn new(output_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Generate a migration from schema diff.
    pub fn generate(
        &self,
        rust_tables: &[TableDef],
        db_tables: &[DatabaseTable],
    ) -> Result<Option<Migration>, GeneratorError> {
        let diff = SchemaDiff::from_comparison(rust_tables, db_tables);

        if diff.is_empty() {
            return Ok(None);
        }

        let now = Utc::now();
        let version = now.format("%Y%m%d_%H%M%S").to_string();
        let name = self.generate_name(&diff);

        let sql = diff.to_sql().join("\n\n");

        let migration = Migration {
            version: version.clone(),
            name: name.clone(),
            sql,
            created_at: now,
            path: self.output_dir.join(format!("{}_{}.sql", version, name)),
        };

        Ok(Some(migration))
    }

    /// Generate a human-readable name for the migration.
    fn generate_name(&self, diff: &SchemaDiff) -> String {
        if diff.entries.is_empty() {
            return "empty".to_string();
        }

        // Use first entry to generate name
        let first = &diff.entries[0];
        match first.action {
            super::diff::DiffAction::CreateTable => {
                format!("create_{}", first.table_name)
            }
            super::diff::DiffAction::AddColumn => {
                format!("add_column_to_{}", first.table_name)
            }
            super::diff::DiffAction::DropColumn => {
                format!("remove_column_from_{}", first.table_name)
            }
            super::diff::DiffAction::DropTable => {
                format!("drop_{}", first.table_name)
            }
            _ => "schema_update".to_string(),
        }
    }

    /// Write migration to disk.
    pub fn write_migration(&self, migration: &Migration) -> Result<(), GeneratorError> {
        std::fs::create_dir_all(&self.output_dir).map_err(|e| GeneratorError::Io(e.to_string()))?;

        let content = format!(
            "-- Migration: {}\n-- Generated at: {}\n\n{}\n",
            migration.name,
            migration.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            migration.sql
        );

        std::fs::write(&migration.path, content).map_err(|e| GeneratorError::Io(e.to_string()))?;

        Ok(())
    }
}

/// A generated migration.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Version identifier (timestamp-based).
    pub version: String,
    /// Human-readable name.
    pub name: String,
    /// SQL content.
    pub sql: String,
    /// When the migration was created.
    pub created_at: DateTime<Utc>,
    /// Path to the migration file.
    pub path: std::path::PathBuf,
}

/// Migration generator errors.
#[derive(Debug, thiserror::Error)]
pub enum GeneratorError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::schema::FieldAttribute;
    use forge_core::schema::RustType;
    use forge_core::schema::{FieldDef, TableDef};

    #[test]
    fn test_generate_migration() {
        let generator = MigrationGenerator::new("/tmp/migrations");

        let mut table = TableDef::new("users", "User");
        let mut id_field = FieldDef::new("id", RustType::Uuid);
        id_field.attributes.push(FieldAttribute::Id);
        table.fields.push(id_field);

        let migration = generator.generate(&[table], &[]).unwrap();

        assert!(migration.is_some());
        let m = migration.unwrap();
        assert!(m.name.contains("users"));
        assert!(m.sql.contains("CREATE TABLE"));
    }
}
