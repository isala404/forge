//! A minimal 5-field cron parser (UTC, 1-minute resolution): `minute hour
//! day-of-month month day-of-week`. Supports `*`, `*/n`, `a`, `a-b`, and `a-b/n`,
//! comma-separated. Day-of-week is `0..=7` with both `0` and `7` meaning Sunday.
//! Pure (no DB), so it is unit-tested directly.

use crate::error::{ForgeError, Result};
use chrono::{DateTime, Datelike, Timelike, Utc};

/// A parsed cron expression. The per-field bitmaps make matching a few array reads.
#[derive(Debug, Clone)]
pub(crate) struct Cron {
    minute: Vec<bool>, // 0..=59
    hour: Vec<bool>,   // 0..=23
    dom: Vec<bool>,    // 1..=31 (index 0 unused)
    month: Vec<bool>,  // 1..=12 (index 0 unused)
    dow: Vec<bool>,    // 0..=7  (0 and 7 = Sunday)
    /// Vixie rule: when BOTH day-of-month and day-of-week are restricted, a day
    /// matches if EITHER does; otherwise the unrestricted one is ignored.
    dom_restricted: bool,
    dow_restricted: bool,
}

impl Cron {
    /// Parse a standard 5-field cron expression. Invalid syntax/range => `Invalid`.
    pub(crate) fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(ForgeError::invalid(format!(
                "cron expression needs exactly 5 fields, got {}",
                fields.len()
            )));
        }
        let get = |i: usize| fields.get(i).copied().unwrap_or("");
        let (minute, _) = parse_field(get(0), 0, 59)?;
        let (hour, _) = parse_field(get(1), 0, 23)?;
        let (dom, dom_restricted) = parse_field(get(2), 1, 31)?;
        let (month, _) = parse_field(get(3), 1, 12)?;
        let (dow, dow_restricted) = parse_field(get(4), 0, 7)?;
        Ok(Self {
            minute,
            hour,
            dom,
            month,
            dow,
            dom_restricted,
            dow_restricted,
        })
    }

    fn matches(&self, dt: &DateTime<Utc>) -> bool {
        let bit = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
        let wd = dt.weekday().num_days_from_sunday() as usize; // 0 = Sunday
        let dom_ok = bit(&self.dom, dt.day() as usize);
        let dow_ok = bit(&self.dow, wd) || (wd == 0 && bit(&self.dow, 7));
        let day_ok = match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_ok || dow_ok,
            (true, false) => dom_ok,
            (false, true) => dow_ok,
            (false, false) => true,
        };
        bit(&self.minute, dt.minute() as usize)
            && bit(&self.hour, dt.hour() as usize)
            && bit(&self.month, dt.month() as usize)
            && day_ok
    }

    /// The first matching minute strictly after `after` (UTC), or `None` if none
    /// falls within ~4 years (an unsatisfiable expression).
    pub(crate) fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut t = (after + chrono::Duration::minutes(1))
            .with_second(0)?
            .with_nanosecond(0)?;
        // 4-year cap covers the rarest case (e.g. Feb 29) without looping forever.
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(&t) {
                return Some(t);
            }
            t += chrono::Duration::minutes(1);
        }
        None
    }
}

/// Parse one field into a `min..=max` bitmap. Returns the bitmap and whether the
/// field was restricted (anything other than `*`).
fn parse_field(spec: &str, min: usize, max: usize) -> Result<(Vec<bool>, bool)> {
    if spec.is_empty() {
        return Err(ForgeError::invalid("empty cron field"));
    }
    let mut set = vec![false; max + 1];
    let restricted = spec != "*";
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step = s
                    .parse::<usize>()
                    .map_err(|_| ForgeError::invalid(format!("invalid cron step: {s:?}")))?;
                if step == 0 {
                    return Err(ForgeError::invalid("cron step must be > 0"));
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (parse_num(a, min, max)?, parse_num(b, min, max)?)
        } else {
            let v = parse_num(range, min, max)?;
            (v, v)
        };
        if lo > hi {
            return Err(ForgeError::invalid(format!(
                "cron range start > end: {range:?}"
            )));
        }
        let mut v = lo;
        while v <= hi {
            if let Some(slot) = set.get_mut(v) {
                *slot = true;
            }
            v += step;
        }
    }
    Ok((set, restricted))
}

fn parse_num(s: &str, min: usize, max: usize) -> Result<usize> {
    let n = s
        .parse::<usize>()
        .map_err(|_| ForgeError::invalid(format!("invalid cron number: {s:?}")))?;
    if n < min || n > max {
        return Err(ForgeError::invalid(format!(
            "cron value {n} out of range {min}..={max}"
        )));
    }
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn star_matches_every_minute() {
        let c = Cron::parse("* * * * *").unwrap();
        let now = at("2026-06-06T10:30:00Z");
        assert_eq!(c.next_after(now), Some(at("2026-06-06T10:31:00Z")));
    }

    #[test]
    fn step_minutes() {
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert!(c.minute[0] && c.minute[15] && c.minute[30] && c.minute[45]);
        assert!(!c.minute[7]);
        assert_eq!(
            c.next_after(at("2026-06-06T10:31:00Z")),
            Some(at("2026-06-06T10:45:00Z"))
        );
    }

    #[test]
    fn daily_at_specific_time() {
        let c = Cron::parse("0 9 * * *").unwrap();
        assert_eq!(
            c.next_after(at("2026-06-06T10:00:00Z")),
            Some(at("2026-06-07T09:00:00Z"))
        );
    }

    #[test]
    fn weekday_range() {
        // 09:00 on Mon-Fri. 2026-06-06 is a Saturday => next is Monday the 8th.
        let c = Cron::parse("0 9 * * 1-5").unwrap();
        assert_eq!(
            c.next_after(at("2026-06-06T12:00:00Z")),
            Some(at("2026-06-08T09:00:00Z"))
        );
    }

    #[test]
    fn invalid_expressions_error() {
        assert!(Cron::parse("* * * *").is_err()); // 4 fields
        assert!(Cron::parse("60 * * * *").is_err()); // minute out of range
        assert!(Cron::parse("*/0 * * * *").is_err()); // zero step
        assert!(Cron::parse("5-1 * * * *").is_err()); // reversed range
    }
}
