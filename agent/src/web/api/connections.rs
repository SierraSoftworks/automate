//! Managing the services a user has linked.
//!
//! Everything here acts on the account the request is for, so an administrator
//! acting as somebody else manages that person's connections rather than their
//! own — which is the point of being able to act as them at all.
//!
//! Credentials only ever travel inwards. A connection can be created with one
//! and its credential can be replaced, but nothing here will read one back out:
//! the responses carry [`ConnectionSummary`], which has nowhere to put one.

use actix_web::{HttpResponse, http::StatusCode, web};
use automate_api::{ConnectionId, ConnectionKind};

use super::json_error;
use super::scope::Scoped;
use crate::connections::ConnectionSecret;
use crate::db::{AuditCategory, AuditEntry, AuditOutcome, AuditStore};
use crate::prelude::*;

/// The body of a request to link a new service.
#[derive(serde::Deserialize)]
pub struct CreateConnection {
    /// The service to link, e.g. `todoist`.
    pub provider: String,

    /// What to call this connection. Defaults to the provider's name, which is
    /// enough until somebody links a second account on the same service.
    #[serde(default)]
    pub name: Option<String>,

    /// The token obtained from the provider.
    pub key: String,
}

/// The changes that can be made to an existing connection.
#[derive(serde::Deserialize)]
pub struct UpdateConnection {
    /// A new display name.
    #[serde(default)]
    pub name: Option<String>,

    /// A replacement credential, for when a token is rotated at the provider.
    #[serde(default)]
    pub key: Option<String>,
}

/// `GET /api/v1/connections` — the services this account has linked.
pub async fn list(services: Scoped) -> HttpResponse {
    match services.connections().list().await {
        Ok(connections) => {
            let summaries: Vec<_> = connections.iter().map(|c| c.to_summary()).collect();
            HttpResponse::Ok().json(summaries)
        }
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `GET /api/v1/connections/{connection}` — one linked service.
pub async fn get(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match services.connections().get(id).await {
        Ok(Some(connection)) => HttpResponse::Ok().json(connection.to_summary()),
        Ok(None) => not_found(id),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `POST /api/v1/connections` — links a service using a token the user supplies.
///
/// Services authorised through OAuth are linked by the setup wizard instead,
/// which is what obtains the credential; there is nothing for the user to paste.
pub async fn create(services: Scoped, body: web::Json<CreateConnection>) -> HttpResponse {
    let body = body.into_inner();

    let key = body.key.trim().to_string();
    if key.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Provide the token this service issued you.",
        );
    }

    if body.provider.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Specify which service to connect.");
    }

    let name = body
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| body.provider.clone());

    let store = services.connections();

    match store
        .create(
            body.provider.clone(),
            name,
            None,
            ConnectionSecret::ApiKey { key },
        )
        .await
    {
        Ok(connection) => {
            record(
                &services,
                "created",
                connection.id,
                format!("Connected {}.", connection.provider),
            )
            .await;

            HttpResponse::Created().json(connection.to_summary())
        }
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `PATCH /api/v1/connections/{connection}` — renames a connection or replaces
/// its credential.
pub async fn update(
    services: Scoped,
    id: web::Path<String>,
    body: web::Json<UpdateConnection>,
) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let body = body.into_inner();
    let store = services.connections();

    let Some(existing) = (match store.get(id).await {
        Ok(existing) => existing,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }) else {
        return not_found(id);
    };

    if let Some(key) = body.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        // Replacing the credential of a connection the provider issued means
        // overwriting a grant we would otherwise renew ourselves, which would
        // silently stop the renewal working.
        if existing.kind != ConnectionKind::ApiKey {
            return json_error(
                StatusCode::BAD_REQUEST,
                "This connection's credential is issued by the provider, so it cannot be replaced by hand. Reconnect the service instead.",
            );
        }

        if let Err(err) = store
            .update_secret(
                id,
                ConnectionSecret::ApiKey {
                    key: key.to_string(),
                },
            )
            .await
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description());
        }

        record(
            &services,
            "credential-replaced",
            id,
            "The stored credential was replaced.",
        )
        .await;
    }

    if let Some(name) = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        && let Err(err) = store.rename(id, name).await
    {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description());
    }

    match store.get(id).await {
        Ok(Some(connection)) => HttpResponse::Ok().json(connection.to_summary()),
        Ok(None) => not_found(id),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `DELETE /api/v1/connections/{connection}` — unlinks a service.
pub async fn delete(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match services.connections().delete(id).await {
        Ok(true) => {
            record(&services, "removed", id, "The connection was removed.").await;
            HttpResponse::NoContent().finish()
        }
        Ok(false) => not_found(id),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// Parses a connection identifier from the path, explaining what went wrong.
///
/// Identifiers are words, so a mistyped one is a likely enough mistake to be
/// worth a message that says which word was not recognised.
fn parse_id(raw: &str) -> Result<ConnectionId, HttpResponse> {
    raw.parse()
        .map_err(|err| json_error(StatusCode::BAD_REQUEST, err))
}

fn not_found(id: ConnectionId) -> HttpResponse {
    json_error(
        StatusCode::NOT_FOUND,
        format!("There is no connection named '{id}'."),
    )
}

/// Records a change to a connection in the account's audit log.
async fn record(services: &Scoped, action: &'static str, id: ConnectionId, message: impl ToString) {
    let entry = AuditEntry::new(AuditCategory::Connection, action, AuditOutcome::Success)
        .subject(id)
        .message(message);

    if let Err(err) = services.audit().record(entry).await {
        // Losing the record should not fail an operation that has already
        // happened; the connection is linked either way.
        warn!(error = %err, "Failed to record a connection change in the audit log.");
    }
}
