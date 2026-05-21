//! Cursor-based pagination types for the wire format.
//!
//! These types establish the pagination contract before GA so that adding
//! query helpers later is purely additive.

use serde::{Deserialize, Serialize};

/// Opaque cursor for keyset pagination.
///
/// Clients receive and return this as a string. The internal format
/// is an implementation detail (base64-encoded JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor(String);

impl Cursor {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A page of results with cursor-based navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page_info: PageInfo,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}

/// Pagination metadata returned alongside a page of results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PageInfo {
    pub has_next_page: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_cursor: Option<Cursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
}

impl PageInfo {
    pub fn last_page() -> Self {
        Self {
            has_next_page: false,
            end_cursor: None,
            total_count: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        let cursor = Cursor::new("eyJpZCI6NDJ9");
        let json = serde_json::to_string(&cursor).unwrap();
        let back: Cursor = serde_json::from_str(&json).unwrap();
        assert_eq!(cursor, back);
    }

    #[test]
    fn page_serialization_shape() {
        let page = Page::new(
            vec!["alice", "bob"],
            PageInfo {
                has_next_page: true,
                end_cursor: Some(Cursor::new("abc")),
                total_count: Some(10),
            },
        );
        let json: serde_json::Value = serde_json::to_value(&page).unwrap();
        assert_eq!(json["items"], serde_json::json!(["alice", "bob"]));
        assert_eq!(json["page_info"]["has_next_page"], true);
        assert_eq!(json["page_info"]["end_cursor"], "abc");
        assert_eq!(json["page_info"]["total_count"], 10);
    }

    #[test]
    fn page_info_skips_none_fields() {
        let info = PageInfo::last_page();
        let json: serde_json::Value = serde_json::to_value(&info).unwrap();
        assert!(!json.as_object().unwrap().contains_key("end_cursor"));
        assert!(!json.as_object().unwrap().contains_key("total_count"));
    }
}
