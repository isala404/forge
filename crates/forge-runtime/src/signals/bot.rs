//! Bot detection via User-Agent pattern matching.
//!
//! Classifies requests as bot or human based on known crawler patterns.
//! Bot events are stored with `is_bot = true` for dashboard filtering.
//!
//! Uses an Aho-Corasick automaton built once at startup for O(n) matching
//! in the length of the input string, rather than O(n*m) for m patterns.

use std::sync::LazyLock;

use aho_corasick::AhoCorasick;

/// Known bot User-Agent substrings (case-insensitive matching).
const BOT_PATTERNS: &[&str] = &[
    // Search engines
    "googlebot",
    "bingbot",
    "yandexbot",
    "baiduspider",
    "duckduckbot",
    "slurp",
    "sogou",
    "exabot",
    "ia_archiver",
    // Social media
    "facebookexternalhit",
    "twitterbot",
    "linkedinbot",
    "whatsapp",
    "telegrambot",
    "discordbot",
    "slackbot",
    // Monitoring / SEO / tools
    "uptimerobot",
    "pingdom",
    "site24x7",
    "statuscake",
    "semrushbot",
    "ahrefsbot",
    "mj12bot",
    "dotbot",
    "rogerbot",
    "screaming frog",
    // Headless browsers
    "headlesschrome",
    "phantomjs",
    "puppeteer",
    "playwright",
    // Generic
    "bot/",
    "bot;",
    "crawler",
    "spider",
    "scraper",
    "http-client",
    "python-requests",
    "python-urllib",
    "go-http-client",
    "java/",
    "wget",
    "curl/",
    "libwww",
    "apache-httpclient",
    // Patterns below match the format these libraries actually emit in a UA
    // (token + "/version"). Bare substrings like "axios" or "okhttp" flagged
    // legitimate mobile apps and react-native clients that ship those names
    // embedded inside larger UAs.
    "okhttp/",
    "node-fetch/",
    "axios/",
    "postmanruntime/",
];

/// Pre-compiled Aho-Corasick automaton for bot detection.
/// Built once on first access; all patterns are lowercase so we match
/// against a lowercased UA string.
static BOT_AUTOMATON: LazyLock<AhoCorasick> =
    LazyLock::new(|| AhoCorasick::new(BOT_PATTERNS).expect("bot patterns are valid"));

/// Check if a User-Agent string belongs to a known bot.
pub fn is_bot(user_agent: Option<&str>) -> bool {
    let ua = match user_agent {
        Some(ua) if !ua.is_empty() => ua,
        _ => return false,
    };
    is_bot_lower(&ua.to_ascii_lowercase())
}

/// Check if a pre-lowercased User-Agent string belongs to a known bot.
pub fn is_bot_lower(ua_lower: &str) -> bool {
    BOT_AUTOMATON.is_match(ua_lower)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detects_googlebot() {
        assert!(is_bot(Some(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
        )));
    }

    #[tokio::test]
    async fn detects_headless_chrome() {
        assert!(is_bot(Some("Mozilla/5.0 HeadlessChrome/90.0.4430.212")));
    }

    #[tokio::test]
    async fn detects_curl() {
        assert!(is_bot(Some("curl/7.68.0")));
    }

    #[tokio::test]
    async fn allows_real_browser() {
        assert!(!is_bot(Some(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        )));
    }

    #[tokio::test]
    async fn allows_mobile_browser() {
        assert!(!is_bot(Some(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
        )));
    }

    #[tokio::test]
    async fn missing_ua_is_not_bot() {
        assert!(!is_bot(None));
        assert!(!is_bot(Some("")));
    }
}
