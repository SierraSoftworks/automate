use actix_web::{web, App, HttpServer};
use human_errors::ResultExt;

use crate::{filter::Filterable, prelude::Services};

mod ui;
mod webhooks;

pub async fn run_web_server<S: Services + Clone + Send + Sync + 'static>(services: S) -> Result<(), human_errors::Error> {
    if let Some((mut addr, port)) = services.config().web.address.split_once(':') {
        if addr.is_empty() {
            addr = "0.0.0.0";
        }

        let port = port.parse::<u16>().wrap_err_as_user(
            "The port number in the web.address field is not a valid number.",
            &[
                "Ensure that the port is a valid integer between 0 and 65535.",
            ],
        )?;

        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(services.clone()))
                .route("/", web::get().to(ui::index))
                .route("/webhooks/{kind:.*}", web::post().to(webhooks::handle::<S>))
                .service(web::resource("/admin")
                    .guard(actix_web::guard::fn_guard(|ctx| {
                        ctx.app_data()
                            .map_or(false, |services: &web::Data<S>| {
                                services
                                    .config()
                                    .web
                                    .admin_acl
                                    .matches(&RequestContextFilter { req: ctx })
                                    .unwrap_or(false)
                            })
                    }))
                    .to(ui::admin_index::<S>))
                .default_service(web::to(ui::not_found))
        })
        .bind((addr, port))?;

        server.run().await?;
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

struct RequestContextFilter<'a> {
    req: &'a actix_web::guard::GuardContext<'a>,
}

impl<'a> Filterable for RequestContextFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "method" => self.req.head().method.as_str().into(),
            "path" => self.req.head().uri.path().into(),
            "client_ip" => self.req.head().peer_addr.map(|addr| addr.ip().to_string()).into(),
            key if key.starts_with("headers.") => {
                let header_name = &key["headers.".len()..];
                let header_value = self.req.head().headers().get(header_name);
                match header_value {
                    Some(value) => match value.to_str() {
                        Ok(s) => s.into(),
                        Err(_) => crate::filter::FilterValue::Null,
                    },
                    None => crate::filter::FilterValue::Null,
                }
            }
            _ => crate::filter::FilterValue::Null,
        }
    }
}