use std::sync::Arc;

use actix_web::{App, HttpServer, web};
use human_errors::ResultExt;

use crate::integrations::Registry;
use crate::prelude::TenantId;
use crate::services::{AppContext, AppServices};

mod api;
mod helpers;
mod integrations;
mod oauth;
mod principal;
mod telemetry;
mod ui;
mod webhooks;

pub use oauth::{OAuth2Config, OAuth2RefreshToken, refresh_or_notify};
pub use principal::Principal;

/// Concrete over [`AppServices`] rather than generic over [`Services`].
///
/// The integration registry has to name one concrete services type to stay
/// object-safe (as [`crate::job::JobRunnable`] does), and its routes extract that
/// type. A generic parameter here would still compile but would leave the
/// integration routes looking for application data that was never registered, so
/// the constraint is stated in the signature instead of being discovered at
/// runtime.
pub async fn run_web_server(context: AppContext) -> Result<(), human_errors::Error> {
    // Built once, up front, so a duplicate integration id is a start-up failure
    // rather than a route that quietly resolves to whichever registration the
    // linker emitted first.
    let registry = Arc::new(Registry::new(&context.config())?);

    // The handlers that have not yet been made tenant-aware continue to act as
    // the local tenant, which is where a single-installation agent has always
    // kept its records. Authentication reaches for the context instead, because
    // the user registry it maintains belongs to the installation rather than to
    // any one account.
    let services = context.tenant(TenantId::local());

    if let Some((mut addr, port)) = context.config().web.address.split_once(':') {
        if addr.is_empty() {
            addr = "0.0.0.0";
        }

        let port = port.parse::<u16>().wrap_user_err(
            "The port number in the web.address field is not a valid number.",
            &["Ensure that the port is a valid integer between 0 and 65535."],
        )?;

        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(services.clone()))
                .app_data(web::Data::new(context.clone()))
                .app_data(web::Data::from(registry.clone()))
                .wrap(telemetry::TracingLogger::<AppServices>::new())
                .service(api::configure())
                .service(integrations::configure())
                .service(integrations::configure_oauth_callback())
                .route("/webhooks/w/{token}", web::post().to(webhooks::deliver))
                .route(
                    "/webhooks/{source}",
                    web::post().to(webhooks::deliver_source),
                )
                .route("/robots.txt", web::get().to(ui::robots))
                .default_service(web::get().to(ui::serve))
        })
        .bind((addr, port))
        .or_user_err(&[
            "Failed to bind the web server to the specified address and port.",
            "Ensure that the port is not already in use by another process.",
            "Ensure that you have permission to bind to the specified port.",
        ])?;

        server.run().await.or_system_err(&[
            "The web server encountered an error while running.",
            "Check the logs for more information.",
        ])?;
        Ok(())
    } else {
        Err(human_errors::user(
            "You have not provided a valid address for the web server to bind to.",
            &[
                "Ensure that the web.address field in your configuration is set to a valid address and port (e.g. `127.0.0.1:8080`).",
            ],
        ))
    }
}
