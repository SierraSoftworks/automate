use std::{fmt::Display, time::Duration};

use crate::{db::Queue, job::Job, services::Services};

mod github_releases_to_todoist;
mod rss_to_todoist;
mod xkcd;
mod youtube;

use chrono::Utc;
pub use github_releases_to_todoist::{GitHubReleasesConfig, GitHubReleasesToTodoistWorkflow};
use human_errors::ResultExt;
pub use rss_to_todoist::{RssConfig, RssToTodoistWorkflow};
use serde::{Serialize, de::DeserializeOwned};
use tracing_batteries::prelude::*;
pub use xkcd::{XkcdConfig, XkcdToTodoistWorkflow};
pub use youtube::{YouTubeConfig, YouTubeToTodoistWorkflow};

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
                    "Next scheduled run for workflow '{}' at {} (in {:?})",
                    &workflow, next, sleep_duration
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
