use std::{collections::HashMap, fmt::Display, time::Duration};

use crate::{db::Queue, job::Job, services::Services};

mod github_releases_to_todoist;
mod honeycomb_alerts_to_todoist;
mod rss_to_todoist;
mod tailscale_alerts_to_todoist;
mod xkcd_to_todoist;
mod youtube_to_todoist;

use chrono::Utc;
use human_errors::ResultExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracing_batteries::prelude::*;

pub use github_releases_to_todoist::{GitHubReleasesConfig, GitHubReleasesToTodoistWorkflow};
pub use honeycomb_alerts_to_todoist::HoneycombAlertsToTodoistWorkflow;
pub use rss_to_todoist::{RssConfig, RssToTodoistWorkflow};
pub use tailscale_alerts_to_todoist::TailscaleAlertsToTodoistWorkflow;
pub use xkcd_to_todoist::{XkcdConfig, XkcdToTodoistWorkflow};
pub use youtube_to_todoist::{YouTubeConfig, YouTubeToTodoistWorkflow};

#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub body: String,
    pub query: String,
    pub headers: HashMap<String, String>,
}

impl WebhookEvent {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, human_errors::Error> {
        serde_json::from_str(&self.body)
            .wrap_err_as_user(
                "Failed to parse webhook event payload as the expected type.",
                &[
                    "Make sure the sender of the webhook is sending the expected payload format.",
                ]
            )
    }
}

pub trait CronWorkflow:
    Display + Clone + Serialize + DeserializeOwned + Send + Sync + 'static
{
    fn schedule(&self) -> croner::Cron;
}

pub async fn schedule<S, J, W>(
    workflows: &[W],
    _job: J,
    services: &S,
) -> Result<(), human_errors::Error>
where
    S: Services + Clone + Send + Sync + 'static,
    J: Job<S, JobType = W>,
    W: CronWorkflow,
{
    let mut join_handles: Vec<tokio::task::JoinHandle<Result<(), human_errors::Error>>> =
        Vec::with_capacity(workflows.len());

    for workflow in workflows {
        let cron = workflow.schedule();
        let services = services.clone();
        let workflow = workflow.clone();

        join_handles.push(tokio::spawn(async move {
            while let Ok(next) = cron.find_next_occurrence(&Utc::now(), true) {
                let now = Utc::now();
                let duration_until_next = next.signed_duration_since(now);
                let sleep_duration = duration_until_next
                    .to_std()
                    .unwrap_or_else(|_| Duration::from_secs(0));

                info!(
                    "Next scheduled run for workflow '{}' at {} (in {})",
                    &workflow, next, duration_until_next
                );

                tokio::time::sleep(sleep_duration).await;

                services
                    .queue()
                    .enqueue(J::partition(), workflow.clone(), None)
                    .await?;
            }

            Ok(())
        }));
    }

    for handle in join_handles {
        handle.await.map_err_as_system(&[
            "Please report this failure to the development team on GitHub.",
        ])??;
    }

    Ok(())
}
