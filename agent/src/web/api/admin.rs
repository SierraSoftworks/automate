//! Installation-wide endpoints.
//!
//! Everything here takes the [`Administrative`] extractor, which refuses a
//! request from anyone who is not an administrator. Putting the guard in the
//! handler's signature rather than in a route wrapper means it travels with the
//! endpoint and cannot be lost by remounting it somewhere else.
//!
//! Note what is deliberately absent: there are no cross-tenant views of the
//! key-value store or the queue. An administrator who needs to see somebody's
//! records acts as them with `X-Impersonate-User` instead, which shows exactly
//! what that user sees and leaves a record of having looked.

use actix_web::{HttpResponse, http::StatusCode, web};

use super::json_error;
use super::scope::Administrative;
use crate::db::AuditQuery;
use crate::prelude::*;
use crate::users::UserRegistry;

/// How many audit entries a single request will return.
const DEFAULT_AUDIT_LIMIT: usize = 100;
const MAX_AUDIT_LIMIT: usize = 1000;

/// Query parameters for reading the audit log.
#[derive(serde::Deserialize)]
pub struct AuditParams {
    /// Return entries older than this id, for paging backwards through history.
    #[serde(default)]
    pub before: Option<i64>,

    /// Restrict to entries about one workflow, connection or account.
    #[serde(default)]
    pub subject: Option<String>,

    #[serde(default)]
    pub limit: Option<usize>,
}

/// Changes an administrator can make to an account.
#[derive(serde::Deserialize)]
pub struct UpdateUser {
    /// Suspend or restore the account.
    #[serde(default)]
    pub disabled: Option<bool>,
}

/// `GET /api/v1/admin/users` — every account that has signed in.
pub async fn list_users(context: Administrative) -> HttpResponse {
    let registry = UserRegistry::new(context.tenant(TenantId::system()));

    match registry.list().await {
        Ok(users) => HttpResponse::Ok().json(users),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `PATCH /api/v1/admin/users/{username}` — suspends or restores an account.
pub async fn update_user(
    context: Administrative,
    username: web::Path<String>,
    body: web::Json<UpdateUser>,
) -> HttpResponse {
    let username = match TenantId::new(username.into_inner()) {
        Ok(username) => username,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, err),
    };

    let registry = UserRegistry::new(context.tenant(TenantId::system()));

    let Some(disabled) = body.into_inner().disabled else {
        // Nothing to do, but reporting it is friendlier than a silent success
        // that leaves the caller thinking a typo'd field took effect.
        return json_error(
            StatusCode::BAD_REQUEST,
            "Specify 'disabled' to suspend or restore this account.",
        );
    };

    match registry.set_disabled(&username, disabled).await {
        Ok(Some(user)) => {
            warn!(
                user.account = %username,
                user.disabled = disabled,
                "An administrator changed an account's status."
            );
            HttpResponse::Ok().json(user)
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            format!("There is no account named '{username}'."),
        ),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `GET /api/v1/admin/audit` — the audit log across every account.
pub async fn audit(context: Administrative, params: web::Query<AuditParams>) -> HttpResponse {
    let params = params.into_inner();

    let mut query = AuditQuery {
        // Clamped so that a caller cannot ask for the entire history in one
        // response and hold the database open while it is serialised.
        limit: params
            .limit
            .unwrap_or(DEFAULT_AUDIT_LIMIT)
            .min(MAX_AUDIT_LIMIT),
        subject: params.subject,
        ..Default::default()
    };

    if let Some(before) = params.before {
        query.before = Some(before);
    }

    match context.database().audit_all(query).await {
        Ok(entries) => HttpResponse::Ok().json(entries),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}
