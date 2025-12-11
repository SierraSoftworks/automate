mod collectors;
mod config;
mod db;
mod filter;
mod job;
mod parsers;
mod prelude;
mod publishers;
mod services;
mod ui;
mod web;
mod webhooks;
mod workflows;

#[cfg(test)]
mod testing;

use clap::Parser;
use futures_concurrency::future::Race;
use tracing_batteries::prelude::*;

use crate::{prelude::*, workflows::CronJob};

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Args {
    config: Option<String>,
}

#[tokio::main]
async fn main() {
    let telemetry = tracing_batteries::Session::new("automate", env!("CARGO_PKG_VERSION"))
        .with_battery(
            tracing_batteries::OpenTelemetry::new("https://refinery.sierrasoftworks.com")
                .with_header(
                    "x-honeycomb-team",
                    "hcaik_01kc5cbks0wtpdg3pndrxkxyswg7ngh59mvxk00hrb69dyp446jvchds8h",
                )
                .with_stdout(true),
        )
        .with_battery(tracing_batteries::Sentry::new(
            "https://64422db58bbf92837d6484d1b8117d5a@o219072.ingest.us.sentry.io/4506753155137536",
        ))
        .with_battery(tracing_batteries::Medama::new(
            "https://analytics.sierrasoftworks.com",
        ));

    if let Err(err) = run().await {
        eprintln!("{}", err);
        telemetry.record_error(&err);
        telemetry.shutdown();
        std::process::exit(1);
    } else {
        telemetry.shutdown();
    }
}

#[instrument("main.run", err(Display))]
async fn run() -> Result<(), human_errors::Error> {
    let args = Args::parse();

    let config = Config::load(args.config.unwrap_or_else(|| "config.toml".into()))?;

    let db = db::SqliteDatabase::open("database.sqlite").await.unwrap();
    let services = services::ServicesContainer::new(config, db);

    {
        CronJob::setup(&services.config().workflows.calendars, services.clone()).await?;
        CronJob::setup(
            &services.config().workflows.github_notifications,
            services.clone(),
        )
        .await?;
        CronJob::setup(
            &services.config().workflows.github_releases,
            services.clone(),
        )
        .await?;
        CronJob::setup(&services.config().workflows.rss, services.clone()).await?;
        CronJob::setup(&services.config().workflows.xkcd, services.clone()).await?;
        CronJob::setup(&services.config().workflows.youtube, services.clone()).await?;
    }

    (
        crate::web::run_web_server(services.clone()),
        crate::workflows::CronJob.run(services.clone()),

        (
            crate::publishers::TodoistCreateTask.run(services.clone()),
            crate::publishers::TodoistUpsertTask.run(services.clone()),
            crate::publishers::TodoistCompleteTask.run(services.clone()),
        ).race(),

        (
            crate::webhooks::AzureMonitorWebhook.run(services.clone()),
            crate::webhooks::GrafanaWebhook.run(services.clone()),
            crate::webhooks::HoneycombWebhook.run(services.clone()),
            // TODO: SentryAlertsWebhook
            crate::webhooks::TailscaleWebhook.run(services.clone()),
            crate::webhooks::TerraformWebhook.run(services.clone()),
        ).race(),

        (
            crate::workflows::CalendarWorkflow.run(services.clone()),
            crate::workflows::GitHubNotificationsWorkflow.run(services.clone()),
            // TODO: GitHubNotificationsCleanupWorkflow (close out old notifications where the subject has been closed)
            crate::workflows::GitHubReleasesWorkflow.run(services.clone()),
            crate::workflows::RssWorkflow.run(services.clone()),
            crate::workflows::XkcdWorkflow.run(services.clone()),
            crate::workflows::YouTubeWorkflow.run(services.clone()),
        ).race()
    ).race().await
        .map_err_as_user(&[
            "Restart the application and try again after addressing any issues reported in the logs.",
        ])?;

    Ok(())
}
