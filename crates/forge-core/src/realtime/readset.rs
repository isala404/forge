use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use uuid::Uuid;

/// Tracking mode for read sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TrackingMode {
    /// No tracking (disabled).
    None,
    /// Track only tables (coarse-grained).
    #[default]
    Table,
}

impl TrackingMode {
    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Table => "table",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTrackingModeError(pub String);

impl std::fmt::Display for ParseTrackingModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tracking mode: {}", self.0)
    }
}

impl std::error::Error for ParseTrackingModeError {}

impl FromStr for TrackingMode {
    type Err = ParseTrackingModeError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "table" => Ok(Self::Table),
            _ => Err(ParseTrackingModeError(s.to_string())),
        }
    }
}

/// Read set tracking tables read during query execution.
#[derive(Debug, Clone, Default)]
pub struct ReadSet {
    /// Tables accessed (stack-allocated for common case of 1-4 tables).
    pub tables: Vec<String>,
    /// Columns used in filters.
    pub filter_columns: HashMap<String, HashSet<String>>,
    /// Tracking mode used.
    pub mode: TrackingMode,
}

impl ReadSet {
    /// Create a new empty read set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a read set with table-level tracking.
    pub fn table_level() -> Self {
        Self {
            mode: TrackingMode::Table,
            ..Default::default()
        }
    }

    /// Add a table to the read set.
    pub fn add_table(&mut self, table: impl Into<String>) {
        let table = table.into();
        if !self.tables.contains(&table) {
            self.tables.push(table);
        }
    }

    /// Add a filter column.
    pub fn add_filter_column(&mut self, table: impl Into<String>, column: impl Into<String>) {
        self.filter_columns
            .entry(table.into())
            .or_default()
            .insert(column.into());
    }

    /// Check if this read set includes a specific table.
    pub fn includes_table(&self, table: &str) -> bool {
        self.tables.iter().any(|t| t == table)
    }

    /// Estimate memory usage in bytes.
    pub fn memory_bytes(&self) -> usize {
        let table_bytes = self.tables.iter().map(|s| s.len() + 24).sum::<usize>();
        let col_bytes = self
            .filter_columns
            .values()
            .map(|set| set.iter().map(|s| s.len() + 24).sum::<usize>())
            .sum::<usize>();

        table_bytes + col_bytes + 64
    }

    /// Merge another read set into this one.
    pub fn merge(&mut self, other: &ReadSet) {
        for table in &other.tables {
            if !self.tables.contains(table) {
                self.tables.push(table.clone());
            }
        }

        for (table, columns) in &other.filter_columns {
            self.filter_columns
                .entry(table.clone())
                .or_default()
                .extend(columns.iter().cloned());
        }
    }
}

/// Change operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeOperation {
    /// Row inserted.
    Insert,
    /// Row updated.
    Update,
    /// Row deleted.
    Delete,
}

impl ChangeOperation {
    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseChangeOperationError(pub String);

impl std::fmt::Display for ParseChangeOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid change operation: {}", self.0)
    }
}

impl std::error::Error for ParseChangeOperationError {}

impl FromStr for ChangeOperation {
    type Err = ParseChangeOperationError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "INSERT" | "I" => Ok(Self::Insert),
            "UPDATE" | "U" => Ok(Self::Update),
            "DELETE" | "D" => Ok(Self::Delete),
            _ => Err(ParseChangeOperationError(s.to_string())),
        }
    }
}

/// A database change event.
#[derive(Debug, Clone)]
pub struct Change {
    /// Table that changed.
    pub table: String,
    /// Type of operation.
    pub operation: ChangeOperation,
    /// Row ID that changed.
    pub row_id: Option<Uuid>,
    /// Columns that changed (for updates).
    pub changed_columns: Vec<String>,
}

impl Change {
    /// Create a new change event.
    pub fn new(table: impl Into<String>, operation: ChangeOperation) -> Self {
        Self {
            table: table.into(),
            operation,
            row_id: None,
            changed_columns: Vec::new(),
        }
    }

    /// Set the row ID.
    pub fn with_row_id(mut self, row_id: Uuid) -> Self {
        self.row_id = Some(row_id);
        self
    }

    /// Set the changed columns.
    pub fn with_columns(mut self, columns: Vec<String>) -> Self {
        self.changed_columns = columns;
        self
    }

    /// Check if this change should invalidate a read set, optionally filtering
    /// by compile-time selected columns from the query.
    pub fn invalidates(&self, read_set: &ReadSet) -> bool {
        read_set.includes_table(&self.table)
    }

    /// Column-aware invalidation: returns false if the changed columns don't
    /// overlap with the query's selected columns.
    pub fn invalidates_columns(&self, selected_columns: &[&str]) -> bool {
        // If we don't know changed columns or selected columns, be conservative
        if self.changed_columns.is_empty() || selected_columns.is_empty() {
            return true;
        }

        // Only for updates, since inserts/deletes affect row presence
        if self.operation != ChangeOperation::Update {
            return true;
        }

        self.changed_columns
            .iter()
            .any(|c| selected_columns.contains(&c.as_str()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_tracking_mode_conversion() {
        assert_eq!("table".parse::<TrackingMode>(), Ok(TrackingMode::Table));
        assert!("invalid".parse::<TrackingMode>().is_err());
    }

    #[test]
    fn test_read_set_add_table() {
        let mut read_set = ReadSet::new();
        read_set.add_table("projects");

        assert!(read_set.includes_table("projects"));
        assert!(!read_set.includes_table("users"));
    }

    #[test]
    fn test_change_invalidates_table_level() {
        let mut read_set = ReadSet::table_level();
        read_set.add_table("projects");

        let change = Change::new("projects", ChangeOperation::Insert);
        assert!(change.invalidates(&read_set));

        let change = Change::new("users", ChangeOperation::Insert);
        assert!(!change.invalidates(&read_set));
    }

    #[test]
    fn test_column_invalidation() {
        let change = Change::new("users", ChangeOperation::Update)
            .with_columns(vec!["name".to_string(), "email".to_string()]);

        // Overlapping columns should invalidate
        assert!(change.invalidates_columns(&["name", "age"]));

        // Non-overlapping columns should not
        assert!(!change.invalidates_columns(&["age", "phone"]));

        // Empty selected columns means unknown, invalidate conservatively
        assert!(change.invalidates_columns(&[]));
    }

    #[test]
    fn test_column_invalidation_non_update() {
        // Inserts and deletes always invalidate regardless of columns
        let change =
            Change::new("users", ChangeOperation::Insert).with_columns(vec!["name".to_string()]);
        assert!(change.invalidates_columns(&["age"]));
    }

    #[test]
    fn test_read_set_merge() {
        let mut read_set1 = ReadSet::new();
        read_set1.add_table("projects");

        let mut read_set2 = ReadSet::new();
        read_set2.add_table("users");

        read_set1.merge(&read_set2);

        assert!(read_set1.includes_table("projects"));
        assert!(read_set1.includes_table("users"));
    }
}
