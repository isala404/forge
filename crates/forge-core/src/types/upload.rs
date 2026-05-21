//! File upload type for handling multipart form data.

use std::fmt;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// A file upload from a multipart form.
///
/// Cannot be stored directly in the database — attempting to convert to a SQL
/// type will panic. Save the bytes to object storage and store only the URL.
#[derive(Clone, Serialize, Deserialize)]
pub struct Upload {
    name: String,
    content_type: String,
    #[serde(with = "bytes_serde")]
    bytes: Bytes,
}

impl Upload {
    pub fn new(name: impl Into<String>, content_type: impl Into<String>, bytes: Bytes) -> Self {
        Self {
            name: name.into(),
            content_type: content_type.into(),
            bytes,
        }
    }

    pub fn from_bytes(name: impl Into<String>, bytes: Bytes) -> Self {
        Self {
            name: name.into(),
            content_type: "application/octet-stream".to_string(),
            bytes,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upload")
            .field("name", &self.name)
            .field("content_type", &self.content_type)
            .field("size", &self.bytes.len())
            .finish()
    }
}

impl fmt::Display for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}, {} bytes)",
            self.name,
            self.content_type,
            self.len()
        )
    }
}

/// Serde helper for Bytes using base64 encoding.
mod bytes_serde {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = STANDARD.encode(bytes);
        encoded.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(&encoded)
            .map(Bytes::from)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let upload = Upload::new("test.txt", "text/plain", Bytes::from("hello"));
        assert_eq!(upload.name(), "test.txt");
        assert_eq!(upload.content_type(), "text/plain");
        assert_eq!(upload.len(), 5);
    }

    #[test]
    fn test_from_bytes() {
        let upload = Upload::from_bytes("data.bin", Bytes::from(vec![1, 2, 3]));
        assert_eq!(upload.content_type(), "application/octet-stream");
        assert_eq!(upload.len(), 3);
    }

    #[test]
    fn test_content_type() {
        let png = Upload::new("img.png", "image/png", Bytes::new());
        let pdf = Upload::new("doc.pdf", "application/pdf", Bytes::new());

        assert!(png.content_type().starts_with("image/"));
        assert_eq!(pdf.content_type(), "application/pdf");
    }

    #[test]
    fn test_serialization() {
        let upload = Upload::new("test.txt", "text/plain", Bytes::from("hello world"));
        let json = serde_json::to_string(&upload).unwrap();
        let parsed: Upload = serde_json::from_str(&json).unwrap();

        assert_eq!(upload.name(), parsed.name());
        assert_eq!(upload.content_type(), parsed.content_type());
        assert_eq!(upload.bytes(), parsed.bytes());
    }

    #[test]
    fn test_display() {
        let upload = Upload::new("file.txt", "text/plain", Bytes::from("abc"));
        let display = format!("{}", upload);
        assert!(display.contains("file.txt"));
        assert!(display.contains("text/plain"));
        assert!(display.contains("3 bytes"));
    }
}
