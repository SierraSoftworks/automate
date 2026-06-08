//! Baked-in sample data for offline UI previews.
//!
//! Appending `?demo` to the URL (for example when running `trunk serve` without
//! a backend) makes the app render this fixture data instead of calling the
//! API, so the interface can be developed and reviewed without a running agent.

use automate_api::{AdminUser, KeyValueEntry, QueueMessage, QueueStatus};
use chrono::Duration;
use serde_json::json;

/// Returns true when the current URL requests demo mode (`?demo`).
///
/// Demo mode is a development convenience, so it is only available in debug
/// builds. Release builds always return `false`, ensuring the production UI
/// never bypasses the real API.
#[cfg(debug_assertions)]
pub fn is_demo() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|search| search.contains("demo"))
        .unwrap_or(false)
}

/// Demo mode is unavailable in release builds.
#[cfg(not(debug_assertions))]
pub fn is_demo() -> bool {
    false
}

/// A sample signed-in user for demo mode.
pub fn admin_user() -> AdminUser {
    AdminUser {
        name: "Demo User".to_string(),
        email: Some("demo@example.com".to_string()),
    }
}

/// Sample key-value entries spanning a couple of partitions. One payload
/// deliberately contains HTML to demonstrate that payloads are rendered as
/// escaped text.
pub fn kv_entries() -> Vec<KeyValueEntry> {
    vec![
        KeyValueEntry::new(
            "github_notifications",
            "PR-1042",
            json!({
                "title": "Refactor the web UI into separate crates",
                "url": "https://github.com/SierraSoftworks/automate/pull/1042",
                "unread": true
            }),
        ),
        KeyValueEntry::new(
            "github_notifications",
            "ISSUE-1043",
            json!({
                "title": "<script>alert('xss')</script> rendered safely",
                "url": "https://github.com/SierraSoftworks/automate/issues/1043",
                "unread": false
            }),
        ),
        KeyValueEntry::new(
            "rss_state",
            "https://example.com/feed.xml",
            json!({
                "last_seen": "2024-05-01T12:00:00Z",
                "etag": "\"a1b2c3\""
            }),
        ),
    ]
}

/// Sample queued messages covering the three message states.
pub fn queue_messages() -> Vec<QueueMessage> {
    let now = chrono::Utc::now();
    vec![
        // Pending: enqueued a while ago and available now (no hidden span, so the
        // timeline collapses to the pending state node with a notification dot).
        QueueMessage {
            partition: "todoist_create".to_string(),
            key: "task-001".to_string(),
            payload: json!({ "content": "Review the deployment runbook", "project": "Software" }),
            status: QueueStatus::Pending,
            scheduled_at: now - Duration::minutes(15),
            hidden_until: None,
            traceparent: None,
        },
        // Delayed: a short hidden span with the "now" marker roughly a third of
        // the way along.
        QueueMessage {
            partition: "github_notifications".to_string(),
            key: "notif-7781".to_string(),
            payload: json!({ "action": "archive", "thread": 7781 }),
            status: QueueStatus::Delayed,
            scheduled_at: now - Duration::seconds(90),
            hidden_until: Some(now + Duration::minutes(4)),
            traceparent: Some(
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
            ),
        },
        // Delayed further out: the marker sits near the start of a long span.
        QueueMessage {
            partition: "github_notifications".to_string(),
            key: "notif-9920".to_string(),
            payload: json!({ "action": "snooze", "thread": 9920 }),
            status: QueueStatus::Delayed,
            scheduled_at: now - Duration::minutes(5),
            hidden_until: Some(now + Duration::hours(2)),
            traceparent: None,
        },
        // Reserved: actively processing, so the timeline shows the spinning retry
        // glyph and no "now" marker.
        QueueMessage {
            partition: "spotify_add_to_playlist".to_string(),
            key: "track-55".to_string(),
            payload: json!({ "track": "spotify:track:55", "playlist": "Liked 2024" }),
            status: QueueStatus::Reserved,
            scheduled_at: now - Duration::seconds(20),
            hidden_until: Some(now + Duration::seconds(40)),
            traceparent: None,
        },
    ]
}
