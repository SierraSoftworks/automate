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

/// A plain reading of the common cron shorthands and simple expressions.
///
/// Deliberately partial. A cron expression can say things that take a paragraph
/// to explain, and a half-right paraphrase of one is worse than none — so
/// anything beyond the shapes recognised here is left to speak for itself rather
/// than described approximately.
pub fn describe_cron(expression: &str) -> Option<String> {
    let expression = expression.trim();

    match expression.to_ascii_lowercase().as_str() {
        "" => return None,
        "@hourly" => return Some("every hour".into()),
        "@daily" | "@midnight" => return Some("every day at midnight".into()),
        "@weekly" => return Some("every week on Sunday".into()),
        "@monthly" => return Some("on the first of every month".into()),
        "@yearly" | "@annually" => return Some("once a year, on 1 January".into()),
        _ => {}
    }

    let parts: Vec<&str> = expression.split_whitespace().collect();
    let [minute, hour, day, month, weekday] = parts.as_slice() else {
        return None;
    };

    if (*day, *month, *weekday) != ("*", "*", "*") {
        return None;
    }

    // "0 */6 * * *" — every N hours on the hour.
    if let Some(interval) = hour.strip_prefix("*/")
        && *minute == "0"
    {
        return interval
            .parse::<u32>()
            .ok()
            .map(|hours| format!("every {hours} hours"));
    }

    // "*/15 * * * *" — every N minutes.
    if let Some(interval) = minute.strip_prefix("*/")
        && *hour == "*"
    {
        return interval
            .parse::<u32>()
            .ok()
            .map(|minutes| format!("every {minutes} minutes"));
    }

    // "30 9 * * *" — once a day at a given time.
    if let (Ok(minute), Ok(hour)) = (minute.parse::<u32>(), hour.parse::<u32>()) {
        return Some(format!("every day at {hour:02}:{minute:02}"));
    }

    None
}
