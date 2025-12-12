use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::prelude::*;

mod azure_monitor;
mod grafana;
mod honeycomb;
mod tailscale;
mod terraform;

pub use azure_monitor::{AzureMonitorWebhook, AzureMonitorWebhookConfig};
pub use grafana::{GrafanaWebhook, GrafanaWebhookConfig};
pub use honeycomb::{HoneycombWebhook, HoneycombWebhookConfig};
pub use tailscale::{TailscaleWebhook, TailscaleWebhookConfig};
pub use terraform::{TerraformWebhook, TerraformWebhookConfig};

#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub body: String,
    pub query: String,
    pub headers: HashMap<String, String>,
}

impl std::fmt::Display for WebhookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WebhookEvent")
    }
}

impl WebhookEvent {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, human_errors::Error> {
        serde_json::from_str(&self.body).wrap_err_as_user(
            "Failed to parse webhook event payload as the expected type.",
            &["Make sure the sender of the webhook is sending the expected payload format."],
        )
    }
}
