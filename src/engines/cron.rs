use chrono::Utc;
use human_errors::ResultExt;
use tracing_batteries::prelude::*;

use crate::services::{Services};


pub async fn cron<S, F>(name: impl ToString, cron: croner::Cron, services: S, callback: F) -> Result<(), human_errors::Error>
where
    S: Services + Clone,
    F: AsyncFn(S) -> Result<(), human_errors::Error> + Send + Sync + 'static,
{
    let name = name.to_string();

    while let Ok(next) = cron.find_next_occurrence(&Utc::now(), true) {
        let now = Utc::now();
        let duration = next.signed_duration_since(now);
        debug!("Waiting {duration} until next cron job for '{name}' at {next} ({cron})...");
        tokio::time::sleep(duration.to_std().wrap_err_as_user(
            format!("Your cron expression '{}' results in the job needing to wait for too long.", &cron),
            &["Consider using a different cron expression that results in shorter wait times."],
        )?).await;

        debug!("Running cron job '{name}' scheduled for {next}...");
        let span = info_span!("cron.run", cron.job = name.as_str(), cron.spec = %cron);
        if let Err(err) = callback(services.clone()).instrument(span).await {
            error!(error = %err, "An error occurred while running cron job '{name}': {err}");
        } else {
            info!("Workflow '{name}' completed (next run scheduled for {next}).");
        }
    }

    Ok(())
}