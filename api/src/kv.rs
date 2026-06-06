use serde::{Deserialize, Serialize};

/// A single entry in the key-value store, identified by its partition and key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValueEntry {
    pub partition: String,
    pub key: String,
    pub payload: serde_json::Value,
}

impl KeyValueEntry {
    pub fn new(
        partition: impl Into<String>,
        key: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            partition: partition.into(),
            key: key.into(),
            payload,
        }
    }
}
