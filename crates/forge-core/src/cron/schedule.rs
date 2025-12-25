use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::str::FromStr;

/// A parsed cron schedule.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    /// The cron expression string.
    expression: String,
    /// Parsed schedule (cached).
    schedule: Option<Schedule>,
}

impl Default for CronSchedule {
    fn default() -> Self {
        Self {
            expression: "0 * * * * *".to_string(),
            schedule: Schedule::from_str("0 * * * * *").ok(),
        }
    }
}

impl CronSchedule {
    /// Create a new cron schedule from an expression.
    pub fn new(expression: &str) -> Result<Self, CronParseError> {
        // Normalize expression (add seconds if missing)
        let normalized = normalize_cron_expression(expression);

        let schedule = Schedule::from_str(&normalized)
            .map_err(|e| CronParseError::InvalidExpression(e.to_string()))?;

        Ok(Self {
            expression: normalized,
            schedule: Some(schedule),
        })
    }

    /// Get the cron expression string.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Get the next scheduled time after the given time.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.schedule.as_ref()?.upcoming(Utc).next()
    }

    /// Get the next scheduled time after the given time in a specific timezone.
    pub fn next_after_in_tz(&self, after: DateTime<Utc>, timezone: &str) -> Option<DateTime<Utc>> {
        let tz: Tz = timezone.parse().ok()?;
        let local_time = after.with_timezone(&tz);

        // Get upcoming times in the target timezone
        self.schedule
            .as_ref()?
            .after(&local_time)
            .next()
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// Get all scheduled times between two times.
    pub fn between(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        let Some(ref schedule) = self.schedule else {
            return vec![];
        };

        schedule.after(&start).take_while(|dt| *dt < end).collect()
    }

    /// Get all scheduled times between two times in a specific timezone.
    pub fn between_in_tz(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        timezone: &str,
    ) -> Vec<DateTime<Utc>> {
        let Ok(tz) = timezone.parse::<Tz>() else {
            return vec![];
        };

        let Some(ref schedule) = self.schedule else {
            return vec![];
        };

        let local_start = start.with_timezone(&tz);
        let local_end = end.with_timezone(&tz);

        schedule
            .after(&local_start)
            .take_while(|dt| *dt < local_end)
            .map(|dt| dt.with_timezone(&Utc))
            .collect()
    }
}

/// Normalize a cron expression to include seconds.
fn normalize_cron_expression(expr: &str) -> String {
    let parts: Vec<&str> = expr.split_whitespace().collect();

    match parts.len() {
        5 => format!("0 {}", expr), // Add "0" for seconds
        6 => expr.to_string(),      // Already has seconds
        _ => expr.to_string(),      // Let the parser handle the error
    }
}

/// Cron parsing error.
#[derive(Debug, Clone)]
pub enum CronParseError {
    /// Invalid cron expression.
    InvalidExpression(String),
}

impl std::fmt::Display for CronParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidExpression(e) => write!(f, "Invalid cron expression: {}", e),
        }
    }
}

impl std::error::Error for CronParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_five_part_cron() {
        let schedule = CronSchedule::new("*/5 * * * *").unwrap();
        assert_eq!(schedule.expression(), "0 */5 * * * *");
    }

    #[test]
    fn test_parse_six_part_cron() {
        let schedule = CronSchedule::new("30 */5 * * * *").unwrap();
        assert_eq!(schedule.expression(), "30 */5 * * * *");
    }

    #[test]
    fn test_next_after() {
        let schedule = CronSchedule::new("0 0 * * * *").unwrap(); // Every hour
        let now = Utc::now();
        let next = schedule.next_after(now);
        assert!(next.is_some());
        assert!(next.unwrap() > now);
    }

    #[test]
    fn test_invalid_cron() {
        let result = CronSchedule::new("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_between() {
        let schedule = CronSchedule::new("0 * * * *").unwrap(); // Every minute
        let start = Utc::now();
        let end = start + chrono::Duration::hours(1);
        let times = schedule.between(start, end);
        assert!(!times.is_empty());
    }
}
