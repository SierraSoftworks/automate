use chrono::{TimeDelta, Utc};

use crate::prelude::*;

pub trait Job<S: Services + Clone> {
    type JobType: serde::Serialize + serde::de::DeserializeOwned + Send + 'static;

    async fn dispatch(
        job: Self::JobType,
        delay: Option<TimeDelta>,
        services: &S,
    ) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition());

        queue.enqueue(job, delay).await?;

        Ok(())
    }

    fn partition() -> &'static str;

    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(5)
    }

    async fn handle(&self, job: &Self::JobType, services: S) -> Result<(), human_errors::Error>;

    async fn run(&self, services: S) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition().to_string());

        loop {
            match queue.dequeue(self.timeout()).await {
                Ok(Some(item)) => {
                    let delay = Utc::now() - item.scheduled_at;
                    let span = info_span!(
                        "job.run",
                        job.name = queue.name(),
                        job.delay = delay.num_milliseconds()
                    );

                    debug!("Processing job '{}'.", queue.name());
                    if let Err(err) = self
                        .handle(&item.payload, services.clone())
                        .instrument(span)
                        .await
                    {
                        error!(error = %err, "An error occurred while processing job '{}': {}", queue.name(), err);
                    } else {
                        queue.complete(item).await.unwrap();
                        info!("Job '{}' completed successfully.", queue.name());
                    }
                }
                Ok(None) => {
                    debug!("No jobs available in queue '{}', waiting...", queue.name());
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Err(err) => {
                    error!(error = %err, "An error occurred while fetching job from queue '{}': {}", queue.name(), err);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }
}
