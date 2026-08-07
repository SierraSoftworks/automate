use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::prelude::*;

mod azure_monitor;
mod github;
mod grafana;
mod grey;
mod honeycomb;
mod routing;
mod sentry;
mod tailscale;
mod terraform;
mod todoist;

pub use github::{GitHubAttentionEvent, GitHubAttentionKind, GitHubPullRequestEvent};
pub use routing::{Delivered, WebhookSource, WebhookSourceRegistration, route, source};

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

/// A delivery, together with the workflow it was addressed to.
///
/// Every webhook workflow is handed one of these. The event is what the sender
/// posted; the workflow names the record that says how to treat it, which is
/// read when the job runs rather than copied in here, so that an edit made since
/// the delivery arrived takes effect on it.
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookDelivery {
    pub workflow: automate_api::WorkflowId,
    pub event: WebhookEvent,
}

impl std::fmt::Display for WebhookDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook/{}", self.workflow)
    }
}

impl WebhookDelivery {
    /// The stored configuration this delivery should be run against.
    ///
    /// `None` means the workflow has been deleted or paused since the delivery
    /// arrived, which is not a failure — there is simply nothing to do, and
    /// retrying would not change that.
    pub async fn config<C>(
        &self,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Option<C>, human_errors::Error>
    where
        C: serde::de::DeserializeOwned,
    {
        let store = crate::workflow_store::WorkflowStore::new(services);

        let Some(record) = store.find(self.workflow).await? else {
            info!(workflow.id = %self.workflow, "Discarding a delivery for a workflow which no longer exists.");
            return Ok(None);
        };

        if !record.enabled {
            debug!(workflow.id = %record.id, "Discarding a delivery for a paused workflow.");
            return Ok(None);
        }

        let config = serde_json::from_value(record.config).wrap_user_err(
            "This workflow is not configured correctly, so a delivery could not be handled.",
            &["Open the workflow and check that every field it asks for is filled in."],
        )?;

        Ok(Some(config))
    }
}

impl WebhookEvent {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, human_errors::Error> {
        serde_json::from_str(&self.body).wrap_user_err(
            "Failed to parse webhook event payload as the expected type.",
            &["Make sure the sender of the webhook is sending the expected payload format."],
        )
    }

    /// A header, matched without regard to case as HTTP requires (RFC 7230).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
