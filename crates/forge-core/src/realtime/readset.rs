use std::collections::{HashMap, HashSet};

use uuid::Uuid;

/// Tracking mode for read sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingMode {
    /// Track only tables (coarse-grained).
    Table,
    /// Track individual rows (fine-grained).
    Row,
    /// Adaptive mode - automatically choose based on query characteristics.
    Adaptive,
}

impl Default for TrackingMode {
    fn default() -> Self {
        Self::Adaptive
    }
}

impl TrackingMode {
    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "table" => Some(Self::Table),
            "row" => Some(Self::Row),
            "adaptive" => Some(Self::Adaptive),
            _ => None,
        }
    }

    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Row => "row",
            Self::Adaptive => "adaptive",
        }
    }
}

/// Read set tracking tables and rows read during query execution.
#[derive(Debug, Clone, Default)]
pub struct ReadSet {
    /// Tables accessed.
    pub tables: HashSet<String>,
    /// Specific rows read per table.
    pub rows: HashMap<String, HashSet<Uuid>>,
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

    /// Create a read set with row-level tracking.
    pub fn row_level() -> Self {
        Self {
            mode: TrackingMode::Row,
            ..Default::default()
        }
    }

    /// Add a table to the read set.
    pub fn add_table(&mut self, table: impl Into<String>) {
        self.tables.insert(table.into());
    }

    /// Add a row to the read set.
    pub fn add_row(&mut self, table: impl Into<String>, row_id: Uuid) {
        let table = table.into();
        self.tables.insert(table.clone());
        self.rows.entry(table).or_default().insert(row_id);
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
        self.tables.contains(table)
    }

    /// Check if this read set includes a specific row.
    pub fn includes_row(&self, table: &str, row_id: Uuid) -> bool {
        if !self.tables.contains(table) {
            return false;
        }

        // If tracking at table level, any row in the table is included
        if self.mode == TrackingMode::Table {
            return true;
        }

        // If tracking at row level, check specific rows
        if let Some(rows) = self.rows.get(table) {
            rows.contains(&row_id)
        } else {
            // No specific rows tracked means all rows in the table
            true
        }
    }

    /// Estimate memory usage in bytes.
    pub fn memory_bytes(&self) -> usize {
        let table_bytes = self.tables.iter().map(|s| s.len() + 24).sum::<usize>();
        let row_bytes = self
            .rows
            .values()
            .map(|set| set.len() * 16 + 24)
            .sum::<usize>();
        let filter_bytes = self
            .filter_columns
            .values()
            .map(|set| set.iter().map(|s| s.len() + 24).sum::<usize>())
            .sum::<usize>();

        table_bytes + row_bytes + filter_bytes + 64 // overhead
    }

    /// Get total row count tracked.
    pub fn row_count(&self) -> usize {
        self.rows.values().map(|set| set.len()).sum()
    }

    /// Merge another read set into this one.
    pub fn merge(&mut self, other: &ReadSet) {
        self.tables.extend(other.tables.iter().cloned());

        for (table, rows) in &other.rows {
            self.rows
                .entry(table.clone())
                .or_default()
                .extend(rows.iter().cloned());
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
pub enum ChangeOperation {
    /// Row inserted.
    Insert,
    /// Row updated.
    Update,
    /// Row deleted.
    Delete,
}

impl ChangeOperation {
    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "INSERT" | "I" => Some(Self::Insert),
            "UPDATE" | "U" => Some(Self::Update),
            "DELETE" | "D" => Some(Self::Delete),
            _ => None,
        }
    }

    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
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

    /// Check if this change should invalidate a read set.
    pub fn invalidates(&self, read_set: &ReadSet) -> bool {
        // Check if the table is in the read set
        if !read_set.includes_table(&self.table) {
            return false;
        }

        // For row-level tracking, check if the specific row was read
        if read_set.mode == TrackingMode::Row {
            if let Some(row_id) = self.row_id {
                match self.operation {
                    // Updates and deletes only invalidate if the specific row was read
                    ChangeOperation::Update | ChangeOperation::Delete => {
                        return read_set.includes_row(&self.table, row_id);
                    }
                    // Inserts always potentially invalidate (new row might match filter)
                    ChangeOperation::Insert => {}
                }
            }
        }

        // Conservative: invalidate if unsure
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracking_mode_conversion() {
        assert_eq!(TrackingMode::from_str("table"), Some(TrackingMode::Table));
        assert_eq!(TrackingMode::from_str("row"), Some(TrackingMode::Row));
        assert_eq!(
            TrackingMode::from_str("adaptive"),
            Some(TrackingMode::Adaptive)
        );
        assert_eq!(TrackingMode::from_str("invalid"), None);
    }

    #[test]
    fn test_read_set_add_table() {
        let mut read_set = ReadSet::new();
        read_set.add_table("projects");

        assert!(read_set.includes_table("projects"));
        assert!(!read_set.includes_table("users"));
    }

    #[test]
    fn test_read_set_add_row() {
        let mut read_set = ReadSet::row_level();
        let row_id = Uuid::new_v4();
        read_set.add_row("projects", row_id);

        assert!(read_set.includes_table("projects"));
        assert!(read_set.includes_row("projects", row_id));
        assert!(!read_set.includes_row("projects", Uuid::new_v4()));
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
    fn test_change_invalidates_row_level() {
        let mut read_set = ReadSet::row_level();
        let tracked_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        read_set.add_row("projects", tracked_id);

        // Update to tracked row should invalidate
        let change = Change::new("projects", ChangeOperation::Update).with_row_id(tracked_id);
        assert!(change.invalidates(&read_set));

        // Update to other row should not invalidate
        let change = Change::new("projects", ChangeOperation::Update).with_row_id(other_id);
        assert!(!change.invalidates(&read_set));

        // Insert always potentially invalidates
        let change = Change::new("projects", ChangeOperation::Insert).with_row_id(other_id);
        assert!(change.invalidates(&read_set));
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
