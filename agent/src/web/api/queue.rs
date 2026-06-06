//! Queue administration endpoints.

use actix_web::{HttpResponse, web};

use super::json_error;
use crate::db::Queue;
use crate::prelude::*;

/// Query parameters identifying a single queued message to delete.
#[derive(serde::Deserialize)]
pub struct DeleteQuery {
    pub key: String,
}

/// Request body for re-enqueuing (triggering) a queued message immediately.
#[derive(serde::Deserialize)]
pub struct TriggerRequest {
    pub key: String,
    pub payload: serde_json::Value,
}

/// `GET /api/v1/queue` — returns the queued messages across all partitions,
/// sorted by their scheduled time.
pub async fn list<S: Services>(services: web::Data<S>) -> HttpResponse {
    let partitions = match services.queue().partitions().await {
        Ok(partitions) => partitions,
        Err(err) => {
            return json_error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            );
        }
    };

    let now = chrono::Utc::now();
    let mut messages: Vec<automate_api::QueueMessage> = Vec::new();

    for partition in partitions {
        let peeked = match services
            .queue()
            .peek::<_, serde_json::Value>(partition.clone(), 100)
            .await
        {
            Ok(msgs) => msgs,
            Err(err) => {
                return json_error(
                    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                    err.to_string(),
                );
            }
        };

        messages.extend(peeked.into_iter().map(|msg| {
            let status = if msg.reserved_by.is_some() {
                automate_api::QueueStatus::Reserved
            } else if msg.hidden_until > now {
                automate_api::QueueStatus::Delayed
            } else {
                automate_api::QueueStatus::Pending
            };

            let hidden_until = matches!(
                status,
                automate_api::QueueStatus::Reserved | automate_api::QueueStatus::Delayed
            )
            .then_some(msg.hidden_until);

            automate_api::QueueMessage {
                partition: partition.clone(),
                key: msg.key,
                payload: msg.payload,
                status,
                scheduled_at: msg.scheduled_at,
                hidden_until,
                traceparent: msg.traceparent,
            }
        }));
    }

    messages.sort_by_key(|msg| msg.scheduled_at);

    HttpResponse::Ok().json(messages)
}

/// `POST /api/v1/queue/{partition}/trigger` — re-enqueues a message so it
/// becomes immediately available for processing.
pub async fn trigger<S: Services>(
    services: web::Data<S>,
    partition: web::Path<String>,
    body: web::Json<TriggerRequest>,
) -> HttpResponse {
    let partition = partition.into_inner();
    let body = body.into_inner();

    if let Err(err) = services
        .queue()
        .enqueue(partition, body.payload, Some(body.key.into()), None)
        .await
    {
        return json_error(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        );
    }

    HttpResponse::NoContent().finish()
}

/// `DELETE /api/v1/queue/{partition}?key=...` — removes a queued message.
pub async fn delete<S: Services>(
    services: web::Data<S>,
    partition: web::Path<String>,
    query: web::Query<DeleteQuery>,
) -> HttpResponse {
    let partition = partition.into_inner();
    let key = query.into_inner().key;

    if let Err(err) = services.queue().purge(partition, key).await {
        return json_error(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        );
    }

    HttpResponse::NoContent().finish()
}
