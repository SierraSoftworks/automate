use std::fmt::Display;

use chrono::Utc;

use crate::prelude::*;

#[derive(serde::Deserialize, Clone)]
pub struct CronJobConfig<J: Job> {
    #[serde(flatten)]
    pub job: J::JobType,

    pub cron: croner::Cron,
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct CronJobTask {
    pub cron: croner::Cron,
    pub kind: String,
    pub idempotency_key: Option<String>,
    pub task: serde_json::Value,
}

impl Display for CronJobTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl<J: Job> From<&CronJobConfig<J>> for CronJobTask
where
    J::JobType: serde::Serialize + Display,
{
    fn from(config: &CronJobConfig<J>) -> Self {
        CronJobTask {
            cron: config.cron.clone(),
            kind: J::partition().to_string(),
            idempotency_key: Some(format!("{}", config.job)),
            task: serde_json::to_value(&config.job).unwrap(),
        }
    }
}

pub struct CronJob;

impl CronJob {
    #[instrument("cron_job.setup", skip(jobs, services), fields(otel.kind=?OpenTelemetrySpanKind::Producer, job.kind = std::any::type_name::<J::JobType>()))]
    pub async fn setup<J: Job>(
        jobs: &[CronJobConfig<J>],
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error>
    where
        J::JobType: serde::Serialize + Display,
    {
        let queue = services.queue();

        for job in jobs.iter() {
            let job: CronJobTask = job.into();
            let now = Utc::now();
            let next_run = job.cron.find_next_occurrence(&now, false)
                .wrap_err_as_user("We could not determine the next time at which this cron job should be dispatched.", &[
                    "Please ensure the cron schedule is valid.",
                ])?
                ;

            info!("Scheduling cron job '{}' to run at {}", job.kind, next_run);

            queue
                .enqueue(
                    "cron",
                    job.clone(),
                    job.idempotency_key.map(|k| k.into()),
                    Some(next_run - now),
                )
                .await?;
        }

        Ok(())
    }
}

impl Job for CronJob {
    type JobType = CronJobTask;

    fn partition() -> &'static str {
        "cron"
    }

    fn propagate_parent() -> bool {
        false
    }

    #[instrument("workflow.cron.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let now = Utc::now();
        let next_run = job
            .cron
            .find_next_occurrence(&now, false)
            .wrap_err_as_user(
                "We could not determine the next time at which this cron job should be dispatched.",
                &["Please ensure the cron schedule is valid."],
            )?;

        // Enqueue the job to be run at the next scheduled time
        services
            .queue()
            .enqueue(
                "cron",
                job.clone(),
                job.idempotency_key.as_ref().map(|k| k.clone().into()),
                Some(next_run - now),
            )
            .await?;

        // Enqueue the actual task to be run immediately
        services
            .queue()
            .enqueue(job.kind.clone(), job.task.clone(), None, None)
            .await?;

        Ok(())
    }
}
