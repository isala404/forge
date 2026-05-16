use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::util::{parse_duration, parse_size};

/// A duration value parsed from a human-readable string like `"30s"`, `"5m"`, `"1h"`.
///
/// Validates at deserialization time so invalid values fail early. Implements
/// `Deref<Target = Duration>` for ergonomic access in consumer code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationStr(Duration);

impl DurationStr {
    /// Create a new `DurationStr` from a `Duration`.
    pub fn new(d: Duration) -> Self {
        Self(d)
    }

    /// Get the inner `Duration`.
    pub fn into_inner(self) -> Duration {
        self.0
    }

    /// Get the duration as whole seconds.
    pub fn as_secs(&self) -> u64 {
        self.0.as_secs()
    }

    /// Get the duration as whole milliseconds (saturates at `u64::MAX`).
    pub fn as_millis(&self) -> u64 {
        u64::try_from(self.0.as_millis()).unwrap_or(u64::MAX)
    }
}

impl std::ops::Deref for DurationStr {
    type Target = Duration;
    fn deref(&self) -> &Duration {
        &self.0
    }
}

impl From<Duration> for DurationStr {
    fn from(d: Duration) -> Self {
        Self(d)
    }
}

impl From<DurationStr> for Duration {
    fn from(d: DurationStr) -> Self {
        d.0
    }
}

impl Default for DurationStr {
    fn default() -> Self {
        Self(Duration::ZERO)
    }
}

impl fmt::Display for DurationStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.as_secs();
        let millis = self.0.as_millis() as u64;
        if millis > 0 && millis < 1000 {
            write!(f, "{millis}ms")
        } else if secs > 0 && secs.is_multiple_of(86400) {
            write!(f, "{}d", secs / 86400)
        } else if secs > 0 && secs.is_multiple_of(3600) {
            write!(f, "{}h", secs / 3600)
        } else if secs > 0 && secs.is_multiple_of(60) {
            write!(f, "{}m", secs / 60)
        } else {
            write!(f, "{secs}s")
        }
    }
}

impl Serialize for DurationStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DurationStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        parse_duration(&s)
            .map(DurationStr)
            .ok_or_else(|| de::Error::custom(format!("invalid duration: '{s}'")))
    }
}

/// A byte-size value parsed from a human-readable string like `"20mb"`, `"1gb"`.
///
/// Validates at deserialization time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeStr(usize);

impl SizeStr {
    /// Create a new `SizeStr`.
    pub fn new(bytes: usize) -> Self {
        Self(bytes)
    }

    /// Get the size in bytes.
    pub fn as_bytes(&self) -> usize {
        self.0
    }
}

impl std::ops::Deref for SizeStr {
    type Target = usize;
    fn deref(&self) -> &usize {
        &self.0
    }
}

impl From<usize> for SizeStr {
    fn from(n: usize) -> Self {
        Self(n)
    }
}

impl fmt::Display for SizeStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 >= 1024 * 1024 * 1024 && self.0.is_multiple_of(1024 * 1024 * 1024) {
            write!(f, "{}gb", self.0 / (1024 * 1024 * 1024))
        } else if self.0 >= 1024 * 1024 && self.0.is_multiple_of(1024 * 1024) {
            write!(f, "{}mb", self.0 / (1024 * 1024))
        } else if self.0 >= 1024 && self.0.is_multiple_of(1024) {
            write!(f, "{}kb", self.0 / 1024)
        } else {
            write!(f, "{}b", self.0)
        }
    }
}

impl Serialize for SizeStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SizeStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        parse_size(&s)
            .map(SizeStr)
            .ok_or_else(|| de::Error::custom(format!("invalid size: '{s}'")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_str_roundtrip() {
        let cases = vec![
            ("100ms", Duration::from_millis(100), "100ms"),
            ("30s", Duration::from_secs(30), "30s"),
            ("5m", Duration::from_secs(300), "5m"),
            ("1h", Duration::from_secs(3600), "1h"),
            ("7d", Duration::from_secs(604800), "7d"),
        ];
        for (input, expected_dur, expected_str) in cases {
            let d: DurationStr = serde_json::from_str(&format!("\"{input}\"")).unwrap();
            assert_eq!(*d, expected_dur, "parsing {input}");
            assert_eq!(d.to_string(), expected_str, "display {input}");
        }
    }

    #[test]
    fn test_duration_str_invalid() {
        let result: Result<DurationStr, _> = serde_json::from_str("\"invalid\"");
        assert!(result.is_err());
    }

    #[test]
    fn test_size_str_roundtrip() {
        let cases = vec![
            ("1kb", 1024, "1kb"),
            ("20mb", 20 * 1024 * 1024, "20mb"),
            ("1gb", 1024 * 1024 * 1024, "1gb"),
            ("512b", 512, "512b"),
        ];
        for (input, expected_bytes, expected_str) in cases {
            let s: SizeStr = serde_json::from_str(&format!("\"{input}\"")).unwrap();
            assert_eq!(s.as_bytes(), expected_bytes, "parsing {input}");
            assert_eq!(s.to_string(), expected_str, "display {input}");
        }
    }
}
