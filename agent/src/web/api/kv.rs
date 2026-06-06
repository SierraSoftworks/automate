//! Key-value store administration endpoints.

use actix_web::{HttpResponse, web};

use super::json_error;
use crate::db::KeyValueStore;
use crate::prelude::*;

/// Query parameters identifying a single key-value entry to delete.
#[derive(serde::Deserialize)]
pub struct DeleteQuery {
    pub key: String,
}

/// `GET /api/v1/kv` — returns every entry across all partitions.
pub async fn list<S: Services>(services: web::Data<S>) -> HttpResponse {
    let entries = match services.kv().scan::<serde_json::Value>().await {
        Ok(entries) => entries,
        Err(err) => {
            return json_error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            );
        }
    };

    let mut entries: Vec<automate_api::KeyValueEntry> = entries
        .into_iter()
        .map(|(partition, key, payload)| automate_api::KeyValueEntry::new(partition, key, payload))
        .collect();
    entries.sort_by(|a, b| a.partition.cmp(&b.partition).then(a.key.cmp(&b.key)));

    HttpResponse::Ok().json(entries)
}

/// `DELETE /api/v1/kv/{partition}?key=...` — removes a single entry.
pub async fn delete<S: Services>(
    services: web::Data<S>,
    partition: web::Path<String>,
    query: web::Query<DeleteQuery>,
) -> HttpResponse {
    let partition = partition.into_inner();
    let key = query.into_inner().key;

    if let Err(err) = services.kv().remove(partition, key).await {
        return json_error(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        );
    }

    HttpResponse::NoContent().finish()
}
