//! The signed-in user's identity.

use actix_web::{HttpMessage, HttpRequest, HttpResponse};

use crate::web::Principal;

/// `GET /api/v1/me` — returns the signed-in user's display identity.
///
/// Responds with `204 No Content` when no identity provider is configured: there
/// is nobody to identify, and inventing a name would suggest an account exists
/// where none does.
///
/// While an administrator is acting as somebody else this describes the account
/// being acted upon, with `impersonated_by` naming the administrator, so the UI
/// can show whose records are on screen and on whose authority.
pub async fn me(req: HttpRequest) -> HttpResponse {
    let user = req
        .extensions()
        .get::<Principal>()
        .and_then(Principal::to_admin_user);

    match user {
        Some(user) => HttpResponse::Ok().json(user),
        None => HttpResponse::NoContent().finish(),
    }
}
