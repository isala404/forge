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
        // Nonstandard but ubiquitous macros (Quartz/Vixie cron); agents emit them constantly.
        let src = expand_macro(expr).unwrap_or(expr);
        let fields: Vec<&str> = src.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(ForgeError::invalid(format!(
                "cron expression needs exactly 5 fields, got {}",
                fields.len()
            )));
        }
        let get = |i: usize| fields.get(i).copied().unwrap_or("");
        let (minute, _) = parse_field(get(0), 0, 59, &[])?;
        let (hour, _) = parse_field(get(1), 0, 23, &[])?;
        let (dom, dom_restricted) = parse_field(get(2), 1, 31, &[])?;
        let (month, _) = parse_field(get(3), 1, 12, MONTHS)?;
        let (dow, dow_restricted) = parse_field(get(4), 0, 7, DAYS)?;
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

    /// The most recent matching minute at or before `at` (UTC), or `None` if none falls
    /// within ~4 years back. Finds the latest *missed* tick on recovery: for a fast cron
    /// many ticks behind, this is essentially `at` truncated to the minute, so the grace
    /// check sees a tick only seconds late rather than the oldest one.
    pub(crate) fn prev_or_at(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut t = at.with_second(0)?.with_nanosecond(0)?;
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(&t) {
                return Some(t);
            }
            t -= chrono::Duration::minutes(1);
        }
        None
    }
}

/// Three-letter month names (JAN=1 … DEC=12) and day names (SUN=0 … SAT=6).
const MONTHS: &[(&str, usize)] = &[
    ("JAN", 1),
    ("FEB", 2),
    ("MAR", 3),
    ("APR", 4),
    ("MAY", 5),
    ("JUN", 6),
    ("JUL", 7),
    ("AUG", 8),
    ("SEP", 9),
    ("OCT", 10),
    ("NOV", 11),
    ("DEC", 12),
];
const DAYS: &[(&str, usize)] = &[
    ("SUN", 0),
    ("MON", 1),
    ("TUE", 2),
    ("WED", 3),
    ("THU", 4),
    ("FRI", 5),
    ("SAT", 6),
];

/// Expand a `@`-macro to its 5-field equivalent, or `None` if not a macro.
fn expand_macro(expr: &str) -> Option<&'static str> {
    match expr.trim() {
        "@yearly" | "@annually" => Some("0 0 1 1 *"),
        "@monthly" => Some("0 0 1 * *"),
        "@weekly" => Some("0 0 * * 0"),
        "@daily" | "@midnight" => Some("0 0 * * *"),
        "@hourly" => Some("0 * * * *"),
        _ => None,
    }
}

fn parse_field(
    spec: &str,
    min: usize,
    max: usize,
    names: &[(&str, usize)],
) -> Result<(Vec<bool>, bool)> {
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
            (
                parse_num(a, min, max, names)?,
                parse_num(b, min, max, names)?,
            )
        } else {
            let v = parse_num(range, min, max, names)?;
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

fn parse_num(s: &str, min: usize, max: usize, names: &[(&str, usize)]) -> Result<usize> {
    let n = if let Some(&(_, v)) = names.iter().find(|(name, _)| name.eq_ignore_ascii_case(s)) {
        v
    } else {
        s.parse::<usize>()
            .map_err(|_| ForgeError::invalid(format!("invalid cron number: {s:?}")))?
    };
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
    fn daily_macro_equals_its_expansion() {
        assert_eq!(
            Cron::parse("@daily")
                .unwrap()
                .next_after(at("2026-06-06T10:00:00Z")),
            Cron::parse("0 0 * * *")
                .unwrap()
                .next_after(at("2026-06-06T10:00:00Z"))
        );
        assert!(Cron::parse("@hourly").is_ok());
        assert!(Cron::parse("@weekly").is_ok());
        assert!(Cron::parse("@yearly").is_ok());
    }

    #[test]
    fn named_months_and_days_parse() {
        // MON-FRI is the same as 1-5; JAN is month 1.
        let named = Cron::parse("0 9 * jan MON-FRI").unwrap();
        let numeric = Cron::parse("0 9 * 1 1-5").unwrap();
        assert_eq!(
            named.next_after(at("2026-06-06T12:00:00Z")),
            numeric.next_after(at("2026-06-06T12:00:00Z"))
        );
        // A day name is not a valid month name, so it fails to parse in the month field.
        assert!(Cron::parse("0 9 * MON *").is_err());
    }

    #[test]
    fn prev_or_at_finds_the_most_recent_tick() {
        // Every-minute cron: prev tick is `at` truncated to the minute.
        let every = Cron::parse("* * * * *").unwrap();
        assert_eq!(
            every.prev_or_at(at("2026-06-06T10:30:45Z")),
            Some(at("2026-06-06T10:30:00Z"))
        );
        // A daily 09:00 cron, asked at 14:00, points back to 09:00 the same day.
        let daily = Cron::parse("0 9 * * *").unwrap();
        assert_eq!(
            daily.prev_or_at(at("2026-06-06T14:00:00Z")),
            Some(at("2026-06-06T09:00:00Z"))
        );
        // An exact match is returned as-is (at-or-before, inclusive).
        assert_eq!(
            daily.prev_or_at(at("2026-06-06T09:00:00Z")),
            Some(at("2026-06-06T09:00:00Z"))
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
