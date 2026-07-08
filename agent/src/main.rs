mod collectors;
mod config;
mod db;
mod filter;
mod job;
mod jobs;
mod parsers;
mod prelude;
mod publishers;
mod services;
mod web;
mod webhooks;

#[cfg(test)]
mod testing;

use std::sync::Arc;

use clap::Parser;
use futures_concurrency::future::Race;
use tracing_batteries::{Session, prelude::*};

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

    let session = Arc::new(Session::new("automate", env!("CARGO_PKG_VERSION"))
        .with_battery(tracing_batteries::OpenTelemetry::new("").with_stdout(true))
        .with_battery(tracing_batteries::Sentry::new(
            "https://64422db58bbf92837d6484d1b8117d5a@o219072.ingest.us.sentry.io/4506753155137536",
        ))
        .with_battery(tracing_batteries::Analytics::new(
            "https://analytics.sierrasoftworks.com",
        )));

    let result = run(args, session.clone()).await;

    if let Err(err) = &result {
        eprintln!("{}", human_errors::pretty(err));

        if err.is(human_errors::Kind::System) {
            session.record_error(err);

        }
    }

    // The web server and job host have stopped, but some of their in-flight
    // work may still be dropping the `Arc<Session>` clones it was holding.
    // Reclaim sole ownership so we can consume the session and flush every
    // telemetry battery before the process exits.
    shutdown_session(session).await;

    if result.is_err() {
        std::process::exit(1);
    }
}

/// Reclaims sole ownership of the telemetry [`Session`] and shuts it down,
/// flushing all batteries.
///
/// [`Session::shutdown`] consumes the session, so we cannot call it while any
/// clone is still alive. After the web server and job host stop, their tasks
/// release their clones asynchronously, so we briefly wait for the strong count
/// to drop to one before consuming the session. In the rare case a clone is
/// still outstanding after the grace period we skip the shutdown rather than
/// block indefinitely.
async fn shutdown_session(session: Arc<Session>) {
    let mut session = session;

    for _ in 0..40 {
        match Arc::try_unwrap(session) {
            Ok(owned) => {
                owned.shutdown();
                return;
            }
            Err(shared) => {
                session = shared;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }

    eprintln!(
        "Warning: could not reclaim sole ownership of the telemetry session during shutdown; some telemetry may not have been flushed."
    );
}

#[instrument("main.run", skip(args, session), err(Display))]
async fn run(args: Args, session: Arc<Session>) -> Result<(), human_errors::Error> {
    let config = Config::load(args.config.unwrap_or_else(|| "config.toml".into()))?;

    let db = db::SqliteDatabase::open("database.sqlite").await.unwrap();
    let services = services::ServicesContainer::new(config, db, session.clone());

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
