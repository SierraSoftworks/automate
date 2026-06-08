//! Small formatting helpers shared across pages.

/// Builds an href for in-app navigation, preserving demo mode across full-page
/// navigations by carrying the `?demo` query forward when it is active.
pub fn nav_href(path: &str) -> String {
    if crate::fixtures::is_demo() {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{path}{separator}demo")
    } else {
        path.to_string()
    }
}

/// Formats a UTC timestamp as an ISO 8601 / RFC 3339 string with a `Z` suffix,
/// for example `2026-06-08T12:48:38Z`.
pub fn format_iso8601(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Formats a duration (in seconds) compactly using the single largest sensible
/// unit, for example `45s`, `5m`, `2h`, or `3d`. The sign is ignored.
pub fn short_duration(secs: i64) -> String {
    let abs = secs.unsigned_abs();
    if abs < 60 {
        format!("{abs}s")
    } else if abs < 3600 {
        format!("{}m", abs / 60)
    } else if abs < 86_400 {
        format!("{}h", abs / 3600)
    } else {
        format!("{}d", abs / 86_400)
    }
}

/// Formats a timestamp relative to now in a compact form, for example
/// `15m ago`, `in 5m`, or `now` when it is within a second of the present.
pub fn short_relative(dt: chrono::DateTime<chrono::Utc>) -> String {
    let secs = dt.signed_duration_since(chrono::Utc::now()).num_seconds();
    if secs.abs() < 1 {
        return "now".to_string();
    }
    let magnitude = short_duration(secs);
    if secs < 0 {
        format!("{magnitude} ago")
    } else {
        format!("in {magnitude}")
    }
}
