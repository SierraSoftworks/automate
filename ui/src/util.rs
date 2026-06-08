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

/// Formats a UTC timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn format_abs(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Formats a timestamp relative to now, for example `in 5 minutes` or
/// `2 hours ago`.
pub fn relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let secs = dt.signed_duration_since(chrono::Utc::now()).num_seconds();
    let abs = secs.unsigned_abs();

    let (value, unit) = if abs < 60 {
        (abs, "second")
    } else if abs < 3600 {
        (abs / 60, "minute")
    } else if abs < 86400 {
        (abs / 3600, "hour")
    } else {
        (abs / 86400, "day")
    };

    let plural = if value == 1 { "" } else { "s" };
    if secs < 0 {
        format!("{value} {unit}{plural} ago")
    } else {
        format!("in {value} {unit}{plural}")
    }
}
