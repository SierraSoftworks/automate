//! Request extractors that decide which records a handler may reach.
//!
//! A handler does not choose its own tenant. It declares, by which extractor it
//! takes, whether it operates on the account the request is acting for or on the
//! installation as a whole:
//!
//! - [`Scoped`] yields services restricted to that account. This is what almost
//!   every handler wants, and it is impossible to widen.
//! - [`Administrative`] yields the root context, and only resolves for a request
//!   made by an administrator.
//!
//! Because the choice lives in the signature, reviewing which endpoints can see
//! across tenants is a matter of searching for one type rather than reading
//! every handler body.

use std::future::{Ready, ready};
use std::ops::Deref;

use actix_web::http::StatusCode;
use actix_web::{FromRequest, HttpMessage, HttpRequest, dev::Payload, web};

use automate_api::TenantId;

use crate::connections::ConnectionStore;
use crate::services::{AppContext, AppServices};
use crate::web::Principal;

use super::json_error;

/// Services restricted to the account a request is acting for.
///
/// While an administrator is impersonating somebody, this is the impersonated
/// account — which is the point: the administrator sees exactly what that user
/// would see.
pub struct Scoped {
    services: AppServices,

    /// Carried alongside the services because the scoped handle deliberately
    /// does not reveal which account it belongs to, and the stores built from it
    /// need the name to reconstruct the context their credentials are sealed
    /// against.
    tenant: TenantId,

    /// Kept so that handlers can reach the records that belong to nobody, such
    /// as the index mapping a webhook URL to the account that owns it.
    context: AppContext,
}

impl Scoped {
    /// This account's workflows, with the webhook address book attached so that
    /// creating or deleting one keeps its URL in step.
    pub fn workflows(&self) -> crate::workflow_store::WorkflowStore<AppServices> {
        crate::workflow_store::WorkflowStore::new(self.services.clone())
            .with_index(self.context.tenant(TenantId::system()))
    }

    /// The account this request is acting for.
    ///
    /// Needed by the OAuth wizard, which has to bind an in-flight authorisation
    /// to the account that started it.
    #[allow(dead_code)]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// This account's linked service credentials.
    pub fn connections(&self) -> ConnectionStore<AppServices> {
        ConnectionStore::new(self.services.clone(), self.tenant.clone())
    }
}

impl Deref for Scoped {
    type Target = AppServices;

    fn deref(&self) -> &Self::Target {
        &self.services
    }
}

impl FromRequest for Scoped {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(resolve(req).map(|(context, principal)| {
            let tenant = principal.effective().clone();

            Scoped {
                services: context.tenant(tenant.clone()),
                tenant,
                context,
            }
        }))
    }
}

/// The installation-wide context, available only to administrators.
///
/// The guard is here rather than in a route wrapper so that an endpoint cannot
/// be mounted somewhere that forgets it.
pub struct Administrative(AppContext);

impl Deref for Administrative {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequest for Administrative {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(resolve(req).and_then(|(context, principal)| {
            if !principal.is_admin() {
                return Err(forbidden("Only administrators may access this resource."));
            }

            Ok(Administrative(context))
        }))
    }
}

/// Pulls the pieces both extractors need off the request.
fn resolve(req: &HttpRequest) -> Result<(AppContext, Principal), actix_web::Error> {
    let Some(context) = req.app_data::<web::Data<AppContext>>() else {
        return Err(actix_web::error::InternalError::from_response(
            "missing application context",
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service context unavailable.",
            ),
        )
        .into());
    };

    // Absent only if an endpoint were mounted outside the authentication
    // middleware, which would be a routing mistake rather than a client error.
    let Some(principal) = req.extensions().get::<Principal>().cloned() else {
        return Err(actix_web::error::InternalError::from_response(
            "unauthenticated request reached a scoped handler",
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "This request was not authenticated.",
            ),
        )
        .into());
    };

    Ok((context.get_ref().clone(), principal))
}

fn forbidden(message: &'static str) -> actix_web::Error {
    actix_web::error::InternalError::from_response(
        message,
        json_error(StatusCode::FORBIDDEN, message),
    )
    .into()
}
