use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::prelude::*;

mod honeycomb;
mod tailscale;

pub use honeycomb::HoneycombWebhook;
pub use tailscale::TailscaleWebhook;

#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub body: String,
    pub query: String,
    pub headers: HashMap<String, String>,
}

impl WebhookEvent {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, human_errors::Error> {
        serde_json::from_str(&self.body)
            .wrap_err_as_user(
                "Failed to parse webhook event payload as the expected type.",
                &[
                    "Make sure the sender of the webhook is sending the expected payload format.",
                ]
            )
    }
}