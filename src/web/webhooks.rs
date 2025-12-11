use std::collections::HashMap;

use actix_web::{Responder, web};
use tracing::instrument;
use tracing_batteries::prelude::error;

use crate::{db::Queue, prelude::Services, webhooks::WebhookEvent};

#[instrument("webhooks.handle", skip(req, kind, body, services), fields(webhook.kind = %kind))]
pub async fn handle<S: Services>(req: actix_web::HttpRequest, kind: web::Path<String>, body: web::Payload, services: web::Data<S>) -> impl Responder {
    let body = match body.to_bytes().await {
        Ok(bytes) => {
            String::from_utf8_lossy(&bytes).to_string()
        }
        Err(err) => {
            error!("Failed to read webhook body: {}", err);
            return actix_web::HttpResponse::BadRequest().finish()
        }
    };

    let mut event = WebhookEvent {
        body,
        query: req.query_string().to_string(),
        headers: HashMap::new(),
    };
    
    req.headers().iter().for_each(|(key, value)| {
        if let Ok(value_str) = value.to_str() {
            event.headers.insert(key.to_string(), value_str.to_string());
        }
    });
    
    if let Err(err) = services.get_ref().queue().enqueue(format!("webhooks/{kind}"), event, None, None).await {
        error!("Failed to enqueue webhook payload: {}", err);
        return actix_web::HttpResponse::InternalServerError().finish();
    }

    actix_web::HttpResponse::NoContent().finish()
}