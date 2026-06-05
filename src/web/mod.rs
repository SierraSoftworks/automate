use actix_web::{App, HttpServer, middleware::from_fn, web};
use human_errors::ResultExt;

use crate::prelude::Services;

mod admin;
mod csrf;
mod oauth;
mod oidc;
mod request;
mod telemetry;
mod ui;
mod webhooks;

pub use oauth::{OAuth2Config, OAuth2RefreshToken};

pub async fn run_web_server<S: Services + Clone + Send + Sync + 'static>(
    services: S,
) -> Result<(), human_errors::Error> {
    if let Some((mut addr, port)) = services.config().web.address.split_once(':') {
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
                .wrap(telemetry::TracingLogger)
                .route("/", web::get().to(ui::index))
                .service(oauth::configure::<S>())
                .route("/webhooks/{kind:.*}", web::post().to(webhooks::handle::<S>))
                .service(
                    web::scope("/admin")
                        .wrap(from_fn(oidc::admin_auth::<S>))
                        .route("/.oidc/callback", web::get().to(oidc::oidc_callback::<S>))
                        .route("", web::get().to(admin::admin_index::<S>))
                        .route("/", web::get().to(admin::admin_index::<S>))
                        .route("/db", web::get().to(admin::admin_db_overview::<S>))
                        .route("/db/delete", web::post().to(admin::admin_db_delete::<S>))
                        .route("/queue", web::get().to(admin::admin_queue::<S>))
                        .route(
                            "/queue/trigger",
                            web::post().to(admin::admin_queue_trigger::<S>),
                        )
                        .route(
                            "/queue/delete",
                            web::post().to(admin::admin_queue_delete::<S>),
                        ),
                )
                .default_service(web::to(ui::not_found))
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
