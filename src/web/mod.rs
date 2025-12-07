use actix_web::{web, App, HttpServer};

use crate::prelude::Services;

mod ui;
mod webhooks;

pub async fn run_web_server<S: Services + Clone + Send + Sync + 'static>(services: S) -> Result<(), human_errors::Error> {
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(services.clone()))
            .route("/", web::get().to(ui::index))
            .route("/webhooks/{kind:.*}", web::post().to(webhooks::handle::<S>))
            .default_service(web::to(ui::not_found))
    })
    .bind(("127.0.0.1", 8080))?;

    server.run().await?;

    Ok(())
}