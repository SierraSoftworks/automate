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

use crate::prelude::*;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Args {
    #[arg(short, long, help = "Path to the configuration file")]
    config: Option<String>,

    #[arg(
        short,
        long,
        help = "Path to an environment file to load (if it exists).",
        default_value = ".env"
    )]
    env: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Load environment variables from .env file if it exists
    // These will override process-level environment variables
    if let Err(err) = Config::load_env_file(&args.env) {
        eprintln!("{}", human_errors::pretty(&err));
        std::process::exit(2);
    }

    let telemetry = tracing_batteries::Session::new("automate", env!("CARGO_PKG_VERSION"))
        .with_battery(tracing_batteries::OpenTelemetry::new("").with_stdout(true))
        .with_battery(tracing_batteries::Sentry::new(
            "https://64422db58bbf92837d6484d1b8117d5a@o219072.ingest.us.sentry.io/4506753155137536",
        ))
        .with_battery(tracing_batteries::Umami::new(
            "https://analytics.sierrasoftworks.com",
            "1dc61b17-a026-478d-aee9-70ef2878fd03",
        ).with_initial_page("/.app/"));

    if let Err(err) = run(args).await {
        eprintln!("{}", human_errors::pretty(&err));
        telemetry.record_error(&err);
        telemetry.shutdown();
        std::process::exit(1);
    } else {
        telemetry.shutdown();
    }
}

#[instrument("main.run", skip(args), err(Display))]
async fn run(args: Args) -> Result<(), human_errors::Error> {
    let config = Config::load(args.config.unwrap_or_else(|| "config.toml".into()))?;

    let db = db::SqliteDatabase::open("database.sqlite").await.unwrap();
    let services = services::ServicesContainer::new(config, db);

    (
        crate::web::run_web_server(services.clone()),
        crate::job::JobHost::run(services.clone()),
    )
        .race()
        .await
        .or_user_err(&[
            "Restart the application and try again after addressing any issues reported in the logs.",
        ])?;

    Ok(())
}
