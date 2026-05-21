//! Local time type (time without date).

use std::fmt;

use chrono::{NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

/// A time of day without date or timezone information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalTime(NaiveTime);

impl LocalTime {
    pub fn from_hms(hour: u32, min: u32, sec: u32) -> Option<Self> {
        NaiveTime::from_hms_opt(hour, min, sec).map(Self)
    }

    pub fn from_hms_milli(hour: u32, min: u32, sec: u32, milli: u32) -> Option<Self> {
        NaiveTime::from_hms_milli_opt(hour, min, sec, milli).map(Self)
    }

    pub fn now() -> Self {
        Self(chrono::Local::now().time())
    }

    pub fn now_utc() -> Self {
        Self(chrono::Utc::now().time())
    }

    /// Midnight (00:00:00).
    pub fn midnight() -> Self {
        Self(NaiveTime::from_hms_opt(0, 0, 0).expect("midnight is always valid"))
    }

    pub fn into_inner(self) -> NaiveTime {
        self.0
    }

    pub fn to_iso8601(&self) -> String {
        self.0.format("%H:%M:%S").to_string()
    }

    pub fn parse_iso8601(s: &str) -> Result<Self, chrono::ParseError> {
        NaiveTime::parse_from_str(s, "%H:%M:%S").map(Self)
    }

    /// Parses HH:MM or HH:MM:SS.
    pub fn parse(s: &str) -> Result<Self, chrono::ParseError> {
        NaiveTime::parse_from_str(s, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
            .map(Self)
    }

    pub fn seconds_since_midnight(&self) -> u32 {
        self.0.num_seconds_from_midnight()
    }
}

impl Default for LocalTime {
    fn default() -> Self {
        Self::midnight()
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_iso8601())
    }
}

impl From<NaiveTime> for LocalTime {
    fn from(time: NaiveTime) -> Self {
        Self(time)
    }
}

impl From<LocalTime> for NaiveTime {
    fn from(time: LocalTime) -> Self {
        time.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn test_from_hms() {
        let time = LocalTime::from_hms(14, 30, 45).unwrap();
        let inner = time.into_inner();
        assert_eq!(inner.hour(), 14);
        assert_eq!(inner.minute(), 30);
        assert_eq!(inner.second(), 45);
    }

    #[test]
    fn test_invalid_time() {
        assert!(LocalTime::from_hms(25, 0, 0).is_none());
        assert!(LocalTime::from_hms(12, 60, 0).is_none());
        assert!(LocalTime::from_hms(12, 0, 60).is_none());
    }

    #[test]
    fn test_serialization() {
        let time = LocalTime::from_hms(14, 30, 0).unwrap();
        let json = serde_json::to_string(&time).unwrap();
        assert_eq!(json, "\"14:30:00\"");
        let parsed: LocalTime = serde_json::from_str(&json).unwrap();
        assert_eq!(time, parsed);
    }

    #[test]
    fn test_iso8601_roundtrip() {
        let time = LocalTime::from_hms(9, 15, 30).unwrap();
        let iso = time.to_iso8601();
        assert_eq!(iso, "09:15:30");
        let parsed = LocalTime::parse_iso8601(&iso).unwrap();
        assert_eq!(time, parsed);
    }

    #[test]
    fn test_parse_flexible() {
        let time1 = LocalTime::parse("14:30").unwrap();
        let time2 = LocalTime::parse("14:30:00").unwrap();
        assert_eq!(time1, time2);
    }

    #[test]
    fn test_midnight() {
        assert_eq!(LocalTime::midnight().into_inner().hour(), 0);
    }

    #[test]
    fn test_seconds_since_midnight() {
        let time = LocalTime::from_hms(1, 0, 0).unwrap();
        assert_eq!(time.seconds_since_midnight(), 3600);
    }
}
