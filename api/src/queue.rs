use serde::{Deserialize, Serialize};

/// The lifecycle status of a queued message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueStatus {
    /// The message is available to be processed now.
    Pending,
    /// The message has been reserved by a consumer and is being processed.
    Reserved,
    /// The message is scheduled to become available at a later time.
    Delayed,
}

impl QueueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueueStatus::Pending => "pending",
            QueueStatus::Reserved => "reserved",
            QueueStatus::Delayed => "delayed",
        }
    }
}

/// A queued job message as presented to the administrative UI.
///
/// Timestamps are carried as raw UTC instants; any human-friendly or relative
/// formatting is the responsibility of the consuming client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueMessage {
    pub partition: String,
    pub key: String,
    pub payload: serde_json::Value,
    pub status: QueueStatus,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    /// The instant at which a reserved or delayed message becomes available
    /// again. Absent for messages that are immediately available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
}
