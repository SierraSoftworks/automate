use std::borrow::Cow;

use chrono::{TimeDelta, Utc};

use crate::prelude::*;

pub trait Job {
    type JobType: serde::Serialize + serde::de::DeserializeOwned + Send + 'static;

    #[instrument("job.dispatch", skip(job, idempotency_key, services), err(Display))]
    async fn dispatch(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        services: &impl Services,
    ) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition());

        queue.enqueue(job, idempotency_key, None).await?;

        Ok(())
    }

    #[instrument("job.dispatch_delayed", skip(job, idempotency_key, delay, services), err(Display))]
    async fn dispatch_delayed(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        delay: TimeDelta,
        services: &impl Services,
    ) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition());

        queue.enqueue(job, idempotency_key, Some(delay)).await?;

        Ok(())
    }

    fn partition() -> &'static str;

    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(5)
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error>;

    #[instrument("job.run", skip(self, services), err(Display))]
    async fn run(&self, services: impl Services + Clone + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition().to_string());

        let root_span = tracing::Span::current();

        loop {
            match queue.dequeue(self.timeout()).await {
                Ok(Some(item)) => {
                    let delay = Utc::now() - item.scheduled_at;
                    let span = info_span!(
                        parent: None,
                        "job.run",
                        job.name = queue.name(),
                        job.delay = delay.num_milliseconds()
                    );
                    span.follows_from(&root_span);

                    let traceparent = item.traceparent.as_deref().unwrap_or("none");

                    if let Some(_) = &item.traceparent {
                        if let Err(err) = get_text_map_propagator(|p| {
                            span.set_parent(p.extract(&item))
                        }) {
                            warn!(error = %err, "Failed to propagate trace context for job '{}' (traceparent: {traceparent}): {err}", queue.name());
                        }
                    }

                    debug!("Processing job '{}' (traceparent: {traceparent}).", queue.name());
                    if let Err(err) = self
                        .handle(&item.payload, services.clone())
                        .instrument(span)
                        .await
                    {
                        error!(error = %err, "An error occurred while processing job '{}' (traceparent: {traceparent}): {err}", queue.name());
                    } else {
                        info!("Job '{}' completed successfully (traceparent: {traceparent}).", queue.name());
                        queue.complete(item).await.unwrap();
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

    fn job_hash(&self, job: &Self::JobType) -> Result<String, human_errors::Error> {
        let serialized = serde_json::to_string(job).wrap_err_as_system(
            "Failed to serialize job for hashing.",
            &[
                "Please report this issue to the dev team on GitHub.",
            ],
        )?;

        Ok(sha256::digest(serialized))
    }
}
