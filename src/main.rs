mod collectors;
mod config;
mod db;
mod filter;
mod job;
mod parsers;
mod prelude;
mod publishers;
mod services;
mod workflows;

#[cfg(test)]
mod testing;

use clap::Parser;

use crate::prelude::*;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Args {
    config: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let telemetry = tracing_batteries::Session::new("automate", env!("CARGO_PKG_VERSION"))
        .with_battery(tracing_batteries::OpenTelemetry::new(""))
        .with_battery(tracing_batteries::Medama::new(
            "https://analytics.sierrasoftworks.com",
        ));

    let local_set = &mut tokio::task::LocalSet::new();
    if let Err(err) = local_set.run_until(run()).await {
        eprintln!("{}", err);
        telemetry.record_error(&err);
        telemetry.shutdown();
        std::process::exit(1);
    } else {
        telemetry.shutdown();
    }
}

async fn run() -> Result<(), human_errors::Error> {
    let args = Args::parse();

    let config = Config::load(args.config.unwrap_or_else(|| "config.toml".into()))?;

    let db = db::SqliteDatabase::open("database.sqlite").await.unwrap();
    let services = services::ServicesContainer::new(db, config.connections);

    tokio::try_join!(
        crate::workflows::GitHubReleasesToTodoistWorkflow.run(services.clone()),
        crate::workflows::RssToTodoistWorkflow.run(services.clone()),
        crate::publishers::TodoistCreateTask.run(services.clone()),
        config.workflows.run_all(services)
    )
    .map_err_as_user(&[])?;

    Ok(())
}
