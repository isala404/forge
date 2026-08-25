//! Stateless interoperability helpers for formats owned by other ecosystems.
//!
//! These helpers do not add a Forge primitive. CloudEvents owns its event contract, and
//! applications remain responsible for transport, authorization, retention, and delivery.

use std::collections::{BTreeMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Map, Value};

use crate::config_store::{MAX_BULK_KEYS, MAX_KEY_BYTES, MAX_VALUE_BYTES};
use crate::{ForgeError, Result};

pub const CLOUD_EVENT_SPEC_VERSION: &str = "1.0";
pub const MAX_CLOUD_EVENT_BYTES: usize = 1024 * 1024;
pub const MAX_CLOUD_EVENT_EXTENSIONS: usize = 64;
pub const MAX_ENV_ALIASES_PER_KEY: usize = 16;

const RESERVED_CLOUD_EVENT_ATTRIBUTES: [&str; 12] = [
    "specversion",
    "id",
    "source",
    "type",
    "datacontenttype",
    "dataschema",
    "subject",
    "time",
    "data",
    "data_base64",
    "dataref",
    "dataref_base64",
];

/// A CloudEvents 1.0 event with an optional binary payload.
///
/// Structured JSON encoding always uses `data_base64` for `data`, preserving arbitrary
/// bytes without guessing from the declared media type. Decoding also accepts the standard
/// JSON `data` member and normalizes it to bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudEvent {
    pub id: String,
    pub source: String,
    pub event_type: String,
    pub subject: Option<String>,
    pub time: Option<String>,
    pub data_content_type: Option<String>,
    pub data_schema: Option<String>,
    pub data: Option<Vec<u8>>,
    pub extensions: BTreeMap<String, Value>,
}

impl CloudEvent {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            event_type: event_type.into(),
            subject: None,
            time: None,
            data_content_type: None,
            data_schema: None,
            data: None,
            extensions: BTreeMap::new(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_cloud_event(self)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        decode_cloud_event(encoded)
    }

    pub fn validate(&self) -> Result<()> {
        validate_required_attribute("id", &self.id)?;
        validate_required_attribute("source", &self.source)?;
        validate_required_attribute("type", &self.event_type)?;
        validate_optional_attribute("subject", self.subject.as_deref())?;
        validate_optional_attribute("datacontenttype", self.data_content_type.as_deref())?;
        validate_optional_attribute("dataschema", self.data_schema.as_deref())?;
        if let Some(time) = self.time.as_deref() {
            validate_required_attribute("time", time)?;
            chrono::DateTime::parse_from_rfc3339(time)
                .map_err(|_| ForgeError::invalid("CloudEvents time must be RFC 3339"))?;
        }
        if self.extensions.len() > MAX_CLOUD_EVENT_EXTENSIONS {
            return Err(ForgeError::limit(
                "CloudEvent has too many extension attributes",
            ));
        }
        for (name, value) in &self.extensions {
            validate_extension(name, value)?;
        }
        Ok(())
    }
}

/// Encode the CloudEvents 1.0 structured JSON format (`application/cloudevents+json`).
pub fn encode_cloud_event(event: &CloudEvent) -> Result<Vec<u8>> {
    event.validate()?;
    let mut object = Map::new();
    object.insert(
        "specversion".into(),
        Value::String(CLOUD_EVENT_SPEC_VERSION.into()),
    );
    object.insert("id".into(), Value::String(event.id.clone()));
    object.insert("source".into(), Value::String(event.source.clone()));
    object.insert("type".into(), Value::String(event.event_type.clone()));
    insert_optional(&mut object, "subject", event.subject.as_ref());
    insert_optional(&mut object, "time", event.time.as_ref());
    insert_optional(
        &mut object,
        "datacontenttype",
        event.data_content_type.as_ref(),
    );
    insert_optional(&mut object, "dataschema", event.data_schema.as_ref());
    for (name, value) in &event.extensions {
        object.insert(name.clone(), value.clone());
    }
    if let Some(data) = &event.data {
        object.insert("data_base64".into(), Value::String(BASE64.encode(data)));
    }
    let encoded = serde_json::to_vec(&Value::Object(object))
        .map_err(|_| ForgeError::invalid("CloudEvent cannot be encoded"))?;
    check_cloud_event_size(encoded.len())?;
    Ok(encoded)
}

/// Decode one CloudEvents 1.0 structured JSON event and discard no extension attributes.
pub fn decode_cloud_event(encoded: &[u8]) -> Result<CloudEvent> {
    check_cloud_event_size(encoded.len())?;
    let value: Value = serde_json::from_slice(encoded)
        .map_err(|_| ForgeError::invalid("CloudEvent must be valid JSON"))?;
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| ForgeError::invalid("CloudEvent must be a JSON object"))?;
    let specversion = take_required_string(&mut object, "specversion")?;
    if specversion != CLOUD_EVENT_SPEC_VERSION {
        return Err(ForgeError::invalid("unsupported CloudEvents specversion"));
    }
    let id = take_required_string(&mut object, "id")?;
    let source = take_required_string(&mut object, "source")?;
    let event_type = take_required_string(&mut object, "type")?;
    let subject = take_optional_string(&mut object, "subject")?;
    let time = take_optional_string(&mut object, "time")?;
    let mut data_content_type = take_optional_string(&mut object, "datacontenttype")?;
    let data_schema = take_optional_string(&mut object, "dataschema")?;
    let encoded_data = object.remove("data_base64");
    let json_data = object.remove("data");
    if encoded_data.is_some() && json_data.is_some() {
        return Err(ForgeError::invalid(
            "CloudEvent data and data_base64 are mutually exclusive",
        ));
    }
    let data = match (encoded_data, json_data) {
        (Some(_), Some(_)) => {
            return Err(ForgeError::invalid(
                "CloudEvent data and data_base64 are mutually exclusive",
            ));
        }
        (Some(Value::String(value)), None) => Some(
            BASE64
                .decode(value)
                .map_err(|_| ForgeError::invalid("CloudEvent data_base64 is invalid"))?,
        ),
        (Some(_), None) => {
            return Err(ForgeError::invalid(
                "CloudEvent data_base64 must be a string",
            ));
        }
        (None, Some(value)) if is_json_content_type(data_content_type.as_deref()) => {
            if data_content_type.is_none() {
                data_content_type = Some("application/json".into());
            }
            Some(
                serde_json::to_vec(&value)
                    .map_err(|_| ForgeError::invalid("CloudEvent data cannot be encoded"))?,
            )
        }
        (None, Some(Value::String(value))) => Some(value.into_bytes()),
        (None, Some(_)) => {
            return Err(ForgeError::invalid(
                "non-JSON CloudEvent data must be a string",
            ));
        }
        (None, None) => None,
    };
    let event = CloudEvent {
        id,
        source,
        event_type,
        subject,
        time,
        data_content_type,
        data_schema,
        data,
        extensions: object.into_iter().collect(),
    };
    event.validate()?;
    Ok(event)
}

/// One logical Forge config key and the common environment names accepted for it.
///
/// Names are ordered aliases. Import rejects conflicting values rather than silently
/// choosing one; export writes only the first, canonical name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvConfigMapping {
    pub key: String,
    pub names: Vec<String>,
}

impl EnvConfigMapping {
    pub fn new(key: impl Into<String>, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            key: key.into(),
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

/// Import a caller-supplied environment snapshot into logical config keys.
pub fn import_env_config(
    environment: &BTreeMap<String, String>,
    mappings: &[EnvConfigMapping],
) -> Result<BTreeMap<String, String>> {
    validate_env_mappings(mappings)?;
    let mut imported = BTreeMap::new();
    for mapping in mappings {
        let values: Vec<_> = mapping
            .names
            .iter()
            .filter_map(|name| environment.get(name).map(|value| (name, value)))
            .collect();
        let Some((_, first)) = values.first() else {
            continue;
        };
        if values.iter().any(|(_, value)| *value != *first) {
            return Err(ForgeError::invalid(format!(
                "environment aliases for {} conflict",
                mapping.key
            )));
        }
        check_config_value(first)?;
        imported.insert(mapping.key.clone(), (*first).clone());
    }
    Ok(imported)
}

/// Export logical config values using each mapping's first, canonical environment name.
pub fn export_env_config(
    config: &BTreeMap<String, String>,
    mappings: &[EnvConfigMapping],
) -> Result<BTreeMap<String, String>> {
    validate_env_mappings(mappings)?;
    let mut exported = BTreeMap::new();
    for mapping in mappings {
        if let Some(value) = config.get(&mapping.key) {
            check_config_value(value)?;
            let name = mapping
                .names
                .first()
                .ok_or_else(|| ForgeError::invalid("environment mapping requires an alias"))?;
            exported.insert(name.clone(), value.clone());
        }
    }
    Ok(exported)
}

fn validate_env_mappings(mappings: &[EnvConfigMapping]) -> Result<()> {
    if mappings.len() > MAX_BULK_KEYS {
        return Err(ForgeError::limit("environment mapping exceeds 256 keys"));
    }
    let mut keys = HashSet::new();
    let mut names = HashSet::new();
    for mapping in mappings {
        if mapping.key.is_empty() || mapping.key.len() > MAX_KEY_BYTES {
            return Err(ForgeError::invalid(
                "environment mapping keys must be 1..=256 UTF-8 bytes",
            ));
        }
        if !keys.insert(mapping.key.as_str()) {
            return Err(ForgeError::invalid(
                "environment mapping keys must be unique",
            ));
        }
        if mapping.names.is_empty() || mapping.names.len() > MAX_ENV_ALIASES_PER_KEY {
            return Err(ForgeError::invalid(
                "environment mapping requires 1..=16 aliases per key",
            ));
        }
        for name in &mapping.names {
            if !valid_environment_name(name) {
                return Err(ForgeError::invalid("invalid environment variable name"));
            }
            if !names.insert(name.as_str()) {
                return Err(ForgeError::invalid(
                    "environment aliases must map to exactly one config key",
                ));
            }
        }
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn check_config_value(value: &str) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(ForgeError::limit("environment config value exceeds 64 KiB"));
    }
    Ok(())
}

fn insert_optional(object: &mut Map<String, Value>, name: &str, value: Option<&String>) {
    if let Some(value) = value {
        object.insert(name.into(), Value::String(value.clone()));
    }
}

fn take_required_string(object: &mut Map<String, Value>, name: &str) -> Result<String> {
    match object.remove(name) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(ForgeError::invalid(format!(
            "CloudEvent {name} must be a string"
        ))),
    }
}

fn take_optional_string(object: &mut Map<String, Value>, name: &str) -> Result<Option<String>> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(ForgeError::invalid(format!(
            "CloudEvent {name} must be a string"
        ))),
    }
}

fn validate_required_attribute(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(is_disallowed_attribute_character) {
        return Err(ForgeError::invalid(format!(
            "CloudEvent {name} is empty or contains control characters"
        )));
    }
    Ok(())
}

fn validate_optional_attribute(name: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_required_attribute(name, value)?;
    }
    Ok(())
}

fn is_disallowed_attribute_character(value: char) -> bool {
    matches!(value as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

fn validate_extension(name: &str, value: &Value) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || RESERVED_CLOUD_EVENT_ATTRIBUTES.contains(&name)
    {
        return Err(ForgeError::invalid(
            "CloudEvents extension names must be lowercase alphanumeric and non-reserved",
        ));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number)
            if number
                .as_i64()
                .is_some_and(|value| i32::try_from(value).is_ok()) =>
        {
            Ok(())
        }
        _ => Err(ForgeError::invalid(
            "CloudEvents extension values must be null, boolean, 32-bit integer, or string",
        )),
    }
}

fn is_json_content_type(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return true;
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let Some((_, subtype)) = media_type.split_once('/') else {
        return false;
    };
    subtype == "json" || subtype.ends_with("+json")
}

fn check_cloud_event_size(size: usize) -> Result<()> {
    if size > MAX_CLOUD_EVENT_BYTES {
        return Err(ForgeError::limit("CloudEvent exceeds 1 MiB"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binary_event_round_trips_and_preserves_extensions() {
        let mut event = CloudEvent::new("job-42", "/tools/render", "com.example.rendered");
        event.data_content_type = Some("application/octet-stream".into());
        event.data = Some(vec![0, 1, 2, 255]);
        event.extensions.insert("traceid".into(), json!("abc"));
        let encoded = event.encode().unwrap();
        assert!(String::from_utf8_lossy(&encoded).contains("data_base64"));
        assert_eq!(CloudEvent::decode(&encoded).unwrap(), event);
    }

    #[test]
    fn json_data_and_implicit_content_type_are_normalized() {
        let event = CloudEvent::decode(
            br#"{"specversion":"1.0","id":"1","source":"/test","type":"example","data":{"ok":true}}"#,
        )
        .unwrap();
        assert_eq!(event.data_content_type.as_deref(), Some("application/json"));
        assert_eq!(event.data.as_deref(), Some(br#"{"ok":true}"#.as_slice()));
    }

    #[test]
    fn invalid_envelopes_fail_loudly() {
        assert_eq!(
            CloudEvent::decode(
                br#"{"specversion":"1.0","id":"1","source":"/","type":"x","data":{},"data_base64":"eA=="}"#,
            )
            .unwrap_err()
            .code(),
            "INVALID"
        );
        let mut event = CloudEvent::new("1", "/", "x");
        event.extensions.insert("Bad-Key".into(), json!(true));
        assert_eq!(event.encode().unwrap_err().code(), "INVALID");
    }

    #[test]
    fn environment_aliases_import_and_export_without_ambiguity() {
        let mappings = [
            EnvConfigMapping::new("database.url", ["DATABASE_URL", "POSTGRES_URL"]),
            EnvConfigMapping::new("blob.bucket", ["S3_BUCKET"]),
        ];
        let environment = BTreeMap::from([
            ("POSTGRES_URL".into(), "postgres://db/app".into()),
            ("S3_BUCKET".into(), "artifacts".into()),
        ]);
        let imported = import_env_config(&environment, &mappings).unwrap();
        assert_eq!(imported["database.url"], "postgres://db/app");
        let exported = export_env_config(&imported, &mappings).unwrap();
        assert_eq!(exported["DATABASE_URL"], "postgres://db/app");
        assert!(!exported.contains_key("POSTGRES_URL"));
    }

    #[test]
    fn environment_alias_conflicts_are_rejected() {
        let mappings = [EnvConfigMapping::new(
            "database.url",
            ["DATABASE_URL", "POSTGRES_URL"],
        )];
        let environment = BTreeMap::from([
            ("DATABASE_URL".into(), "one".into()),
            ("POSTGRES_URL".into(), "two".into()),
        ]);
        assert_eq!(
            import_env_config(&environment, &mappings)
                .unwrap_err()
                .code(),
            "INVALID"
        );
    }

    #[test]
    fn canonical_interop_vectors_match_rust_helpers() {
        let vectors: Value =
            serde_json::from_str(include_str!("../contract/interop-vectors.json")).unwrap();
        let input = serde_json::to_vec(&vectors["cloud_event"]["input"]).unwrap();
        let event = decode_cloud_event(&input).unwrap();
        assert_eq!(event.data, Some(vec![0, 1, 2, 255]));
        assert_eq!(
            event.extensions,
            serde_json::from_value(vectors["cloud_event"]["extensions"].clone()).unwrap()
        );
        assert_eq!(
            decode_cloud_event(&encode_cloud_event(&event).unwrap()).unwrap(),
            event
        );

        let mappings: Vec<EnvConfigMapping> = vectors["environment"]["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mapping| {
                EnvConfigMapping::new(
                    mapping["key"].as_str().unwrap(),
                    mapping["names"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|name| name.as_str().unwrap()),
                )
            })
            .collect();
        let source: BTreeMap<String, String> =
            serde_json::from_value(vectors["environment"]["source"].clone()).unwrap();
        let expected_imported: BTreeMap<String, String> =
            serde_json::from_value(vectors["environment"]["imported"].clone()).unwrap();
        let expected_exported: BTreeMap<String, String> =
            serde_json::from_value(vectors["environment"]["exported"].clone()).unwrap();
        let imported = import_env_config(&source, &mappings).unwrap();
        assert_eq!(imported, expected_imported);
        assert_eq!(
            export_env_config(&imported, &mappings).unwrap(),
            expected_exported
        );
    }
}
