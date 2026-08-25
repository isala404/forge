//! Transport-neutral, lossy hints that tell a client to re-read authoritative state.
//!
//! This is a data contract, not a ninth Forge primitive or a durable change feed. Encode an
//! event and deliver it through application-owned SSE, WebSocket, push, or Forge pub/sub.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ForgeError, Result};

pub const INVALIDATION_SCHEMA_VERSION: u32 = 1;
pub const MAX_INVALIDATION_BYTES: usize = 4096;
pub const MAX_INVALIDATION_TAGS: usize = 32;
pub const MAX_INVALIDATION_QUERY_KEYS: usize = 32;
pub const MAX_INVALIDATION_TARGETS: usize = 64;
pub const MAX_INVALIDATION_TAG_BYTES: usize = 128;
pub const MAX_INVALIDATION_REVISION_BYTES: usize = 256;
pub const MAX_QUERY_KEY_PARTS: usize = 8;
pub const MAX_QUERY_KEY_DEPTH: usize = 3;
pub const MAX_QUERY_KEY_NODES: usize = 32;
pub const MAX_QUERY_KEY_CONTAINER_ITEMS: usize = 16;
pub const MAX_QUERY_KEY_STRING_BYTES: usize = 128;
pub const MAX_QUERY_KEY_OBJECT_KEY_BYTES: usize = 64;

/// The canonical JSON Schema for invalidation event version 1.
pub const INVALIDATION_SCHEMA_JSON: &str = include_str!("../contract/invalidation-v1.schema.json");

/// A bounded hint that names coarse application resources or query-key fragments.
///
/// Unknown JSON fields are ignored while decoding version 1. The event contains no state
/// payload: a receiver must refetch or advance its durable application cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvalidationEvent {
    pub schema_version: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub query_keys: Vec<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl InvalidationEvent {
    pub fn new(
        tags: Vec<String>,
        query_keys: Vec<Vec<Value>>,
        revision: Option<String>,
    ) -> Result<Self> {
        let event = Self {
            schema_version: INVALIDATION_SCHEMA_VERSION,
            tags,
            query_keys,
            revision,
        };
        event.validate()?;
        Ok(event)
    }

    /// Decode a bounded version-1 event. Unknown additive fields are discarded.
    pub fn decode(encoded: &[u8]) -> Result<Self> {
        if encoded.len() > MAX_INVALIDATION_BYTES {
            return Err(ForgeError::limit("invalidation event exceeds 4096 bytes"));
        }
        let event: Self = serde_json::from_slice(encoded)
            .map_err(|_| ForgeError::invalid("invalidation event must be valid JSON"))?;
        event.validate()?;
        Ok(event)
    }

    /// Validate and encode the normalized version-1 event.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| ForgeError::invalid("invalidation event cannot be encoded"))?;
        if encoded.len() > MAX_INVALIDATION_BYTES {
            return Err(ForgeError::limit("invalidation event exceeds 4096 bytes"));
        }
        Ok(encoded)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != INVALIDATION_SCHEMA_VERSION {
            return Err(ForgeError::invalid(
                "unsupported invalidation schema version",
            ));
        }
        if self.tags.is_empty() && self.query_keys.is_empty() {
            return Err(ForgeError::invalid(
                "invalidation event requires a tag or query-key fragment",
            ));
        }
        if self.tags.len() > MAX_INVALIDATION_TAGS {
            return Err(ForgeError::limit("invalidation event has too many tags"));
        }
        if self.query_keys.len() > MAX_INVALIDATION_QUERY_KEYS
            || self.tags.len() + self.query_keys.len() > MAX_INVALIDATION_TARGETS
        {
            return Err(ForgeError::limit("invalidation event has too many targets"));
        }

        let mut unique_tags = HashSet::with_capacity(self.tags.len());
        for tag in &self.tags {
            if tag.is_empty() || tag.len() > MAX_INVALIDATION_TAG_BYTES {
                return Err(ForgeError::invalid(
                    "invalidation tags must be 1..=128 UTF-8 bytes",
                ));
            }
            if !unique_tags.insert(tag) {
                return Err(ForgeError::invalid("invalidation tags must be unique"));
            }
        }

        for query_key in &self.query_keys {
            if query_key.is_empty() || query_key.len() > MAX_QUERY_KEY_PARTS {
                return Err(ForgeError::invalid(
                    "query-key fragments must contain 1..=8 parts",
                ));
            }
            let mut nodes = 0;
            for part in query_key {
                validate_query_key_value(part, 1, &mut nodes)?;
            }
        }

        if self
            .revision
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > MAX_INVALIDATION_REVISION_BYTES)
        {
            return Err(ForgeError::invalid(
                "invalidation revision must be 1..=256 UTF-8 bytes",
            ));
        }

        let encoded = serde_json::to_vec(self)
            .map_err(|_| ForgeError::invalid("invalidation event cannot be encoded"))?;
        if encoded.len() > MAX_INVALIDATION_BYTES {
            return Err(ForgeError::limit("invalidation event exceeds 4096 bytes"));
        }
        Ok(())
    }
}

fn validate_query_key_value(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if *nodes > MAX_QUERY_KEY_NODES {
        return Err(ForgeError::limit("query-key fragment has too many nodes"));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) if value.len() <= MAX_QUERY_KEY_STRING_BYTES => Ok(()),
        Value::String(_) => Err(ForgeError::limit("query-key string exceeds 128 bytes")),
        Value::Array(values) => {
            validate_container(depth, values.len())?;
            for value in values {
                validate_query_key_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            validate_container(depth, values.len())?;
            for (key, value) in values {
                if key.len() > MAX_QUERY_KEY_OBJECT_KEY_BYTES {
                    return Err(ForgeError::limit("query-key object key exceeds 64 bytes"));
                }
                validate_query_key_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
    }
}

fn validate_container(depth: usize, items: usize) -> Result<()> {
    if depth >= MAX_QUERY_KEY_DEPTH {
        return Err(ForgeError::limit("query-key nesting exceeds 3 levels"));
    }
    if items > MAX_QUERY_KEY_CONTAINER_ITEMS {
        return Err(ForgeError::limit("query-key container has too many items"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_discards_unknown_additive_fields() {
        let event = InvalidationEvent::decode(
            br#"{"schema_version":1,"tags":["links"],"query_keys":[["links",{"owner":"u1"}]],"revision":"42","future":{"ignored":true}}"#,
        )
        .unwrap();
        assert_eq!(event.tags, ["links"]);
        assert!(
            !String::from_utf8(event.encode().unwrap())
                .unwrap()
                .contains("future")
        );
    }

    #[test]
    fn rejects_state_payloads_by_size_and_unbounded_shapes() {
        let oversized = vec![b'x'; MAX_INVALIDATION_BYTES + 1];
        assert_eq!(
            InvalidationEvent::decode(&oversized).unwrap_err().code(),
            "LIMIT"
        );
        let nested = InvalidationEvent::new(vec![], vec![vec![json!([[[["too-deep"]]]])]], None)
            .unwrap_err();
        assert_eq!(nested.code(), "LIMIT");
    }

    #[test]
    fn rejects_empty_or_duplicate_targets() {
        assert_eq!(
            InvalidationEvent::new(vec![], vec![], None)
                .unwrap_err()
                .code(),
            "INVALID"
        );
        assert_eq!(
            InvalidationEvent::new(vec!["links".into(), "links".into()], vec![], None)
                .unwrap_err()
                .code(),
            "INVALID"
        );
    }
}
