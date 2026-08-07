//! The JSON REST API consumed by the single-page admin UI.
//!
//! All endpoints live under `/api/v1`. The `auth` sub-scope is public so that an
//! unauthenticated browser can run the OIDC popup login (fetch metadata, exchange
//! a code, refresh a session); every other endpoint is gated by [`api_auth`],
//! which authenticates the `Authorization: Bearer` ID token (when OIDC is
//! configured) and evaluates the admin ACL. Because the credential is a bearer
//! header — never an automatically-attached cookie — there is no CSRF surface and
//! no double-submit token to verify.

use actix_web::{
    HttpResponse,
    body::BoxBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header::AUTHORIZATION,
    middleware::{Next, from_fn},
    web,
};

use crate::prelude::*;
use crate::services::AppContext;
use crate::users::UserRegistry;
use crate::web::Principal;
use crate::web::helpers::oidc::{
    AdminRequestFilter, admin_user_from_claims, filterable_claims, username_from_claims,
    validate_token,
};
use crate::web::helpers::request::client_ip;

mod admin;
mod audit;
mod auth;
mod connections;
mod kv;
mod queue;
pub mod scope;
#[cfg(test)]
mod tenancy_tests;
mod user;
mod workflows;

pub use scope::Scoped;

/// The header by which an administrator asks to act as another user.
pub const IMPERSONATE_HEADER: &str = "x-impersonate-user";

/// Extracts a bearer token from the `Authorization` header, accepting either
/// capitalisation of the `Bearer` scheme.
pub(crate) fn bearer_token(headers: &actix_web::http::header::HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(|token| token.trim().to_string())
}

/// Builds a JSON error response with the given status code and message.
pub fn json_error(status: actix_web::http::StatusCode, message: impl ToString) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

/// Registers the `/api/v1` routes. The `auth` endpoints are public; everything
/// else is wrapped in the [`api_auth`] middleware.
pub fn configure() -> actix_web::Scope<
    impl actix_web::dev::ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse<BoxBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    type S = crate::services::AppServices;

    web::scope("/api/v1")
        .service(
            web::scope("/auth")
                .route("/metadata", web::get().to(auth::metadata::<S>))
                .route("/token", web::post().to(auth::auth_token::<S>))
                .route("/refresh", web::post().to(auth::auth_refresh::<S>)),
        )
        .service(
            web::scope("")
                .wrap(from_fn(api_auth::<S>))
                .route("/me", web::get().to(user::me))
                // Everything below operates on the account the request is acting
                // for, which is the impersonated one when an administrator is
                // acting as somebody else. The `Scoped` extractor in each
                // handler's signature is what enforces that.
                .route("/audit", web::get().to(audit::list))
                .route("/kv", web::get().to(kv::list))
                .route("/kv/{partition}", web::delete().to(kv::delete))
                .route("/queue", web::get().to(queue::list))
                .route("/queue/{partition}/trigger", web::post().to(queue::trigger))
                .route("/queue/{partition}", web::delete().to(queue::delete))
                .route("/connections", web::get().to(connections::list))
                .route("/connections", web::post().to(connections::create))
                .route("/connections/{connection}", web::get().to(connections::get))
                .route(
                    "/connections/{connection}",
                    web::patch().to(connections::update),
                )
                .route(
                    "/connections/{connection}",
                    web::delete().to(connections::delete),
                )
                .route(
                    "/connections/{connection}/options/{source}",
                    web::get().to(connections::options),
                )
                .route("/workflow-types", web::get().to(workflows::types))
                .route("/workflows", web::get().to(workflows::list))
                .route("/workflows", web::post().to(workflows::create))
                // Ahead of the `{workflow}` routes, so that these names are not
                // read as identifiers.
                .route("/workflows/export", web::get().to(workflows::export))
                .route("/workflows/import", web::post().to(workflows::import))
                .route("/workflows/{workflow}", web::get().to(workflows::get))
                .route("/workflows/{workflow}", web::put().to(workflows::update))
                .route("/workflows/{workflow}", web::delete().to(workflows::delete))
                .route("/workflows/{workflow}/runs", web::get().to(workflows::runs))
                .route(
                    "/workflows/{workflow}/rotate-webhook",
                    web::post().to(workflows::rotate_webhook),
                )
                .route(
                    "/workflows/{workflow}/trigger",
                    web::post().to(workflows::trigger),
                )
                .route(
                    "/workflows/{workflow}/reset",
                    web::post().to(workflows::reset),
                )
                // Installation-wide endpoints. These take the `Administrative`
                // extractor, which refuses a request from anyone who is not an
                // administrator, so the guard cannot be lost by remounting them.
                .route("/admin/users", web::get().to(admin::list_users))
                .route(
                    "/admin/users/{username}",
                    web::patch().to(admin::update_user),
                )
                .route("/admin/audit", web::get().to(admin::audit))
                // The setup wizard is launched from the admin SPA: list the
                // configured integrations, mint a popup authorization URL, and
                // manage the resulting connections. All admin-gated by
                // `api_auth`.
                .route(
                    "/integrations",
                    web::get().to(crate::web::integrations::list),
                )
                .route(
                    "/integrations/{integration}/connections",
                    web::get().to(crate::web::integrations::list_connections),
                )
                .route(
                    "/integrations/{integration}/connections/{connection}",
                    web::delete().to(crate::web::integrations::disconnect),
                )
                .route(
                    "/integrations/{integration}/setup/start",
                    web::post().to(crate::web::integrations::start),
                ),
        )
}

/// Authentication and authorisation middleware for the protected API endpoints.
///
/// Resolves the [`Principal`] a request is handled under and attaches it to the
/// request extensions. That involves three separate decisions:
///
/// 1. **Who is this?** When an identity provider is configured, a valid
///    `Authorization: Bearer` ID token is required and the account name is taken
///    from its claims. Otherwise there is nobody to identify, and the request
///    acts as the installation's local account.
/// 2. **May they be here?** The `user_acl` filter is evaluated against the token
///    claims and request metadata. A separate `admin_acl` decides whether they
///    may administer the installation. Both are evaluated per request, so a
///    change to the configuration takes effect immediately.
/// 3. **Whose records are they acting on?** Normally their own; an administrator
///    may redirect this with `X-Impersonate-User`.
///
/// Because the credential is a bearer header rather than an automatically
/// attached cookie, a cross-site page cannot forge an authenticated request, so
/// no CSRF defence is required.
pub async fn api_auth<S: Services + Send + Sync + 'static>(
    req: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    use actix_web::HttpMessage;
    use actix_web::http::StatusCode;

    let Some(services) = req.app_data::<web::Data<S>>().cloned() else {
        return Ok(req.into_response(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Service context unavailable.",
        )));
    };

    let config = services.config();

    // Authenticate via the bearer token when an identity provider is configured.
    let claims = if let Some(oidc) = config.web.oidc() {
        let Some(token) = bearer_token(req.headers()) else {
            return Ok(req.into_response(json_error(
                StatusCode::UNAUTHORIZED,
                "Authentication is required to access this resource.",
            )));
        };

        match validate_token(services.as_ref(), oidc, &token).await {
            Ok(claims) => Some(claims),
            Err(e) => {
                info!("Rejected API request with an invalid bearer token: {e}");
                return Ok(req.into_response(json_error(
                    StatusCode::UNAUTHORIZED,
                    "Your session is invalid or has expired. Please sign in again.",
                )));
            }
        }
    } else {
        None
    };

    let filterable = claims.as_ref().map(filterable_claims);
    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: client_ip(config.web.trust_proxy, req.headers(), req.peer_addr()),
        headers: req.headers(),
        claims: filterable.as_ref(),
    };

    if !config.web.user_acl().matches(&filter).unwrap_or(false) {
        // We only reach the ACL check after authentication has already been
        // resolved: either an identity provider is configured and the session
        // validated above, or there is nothing to sign in to. In both cases a
        // denial here is a permanent authorization failure, so respond `403`.
        // Returning `401` would tell the browser to start a sign-in that cannot
        // change the outcome — and, with no identity provider, would leave the
        // UI bouncing through a sign-in flow that goes nowhere.
        return Ok(req.into_response(json_error(
            StatusCode::FORBIDDEN,
            "Your account is not permitted to access this resource.",
        )));
    }

    let is_admin = config.web.admin_acl().matches(&filter).unwrap_or(false);

    // Determine which account the request acts as. With no identity provider
    // there is nobody to identify, so everything belongs to the installation's
    // local account — which is what a single-user install has always used.
    let account = match account_for(&config, claims.as_ref()) {
        Ok(account) => account,
        Err(err) => {
            warn!("Rejected a sign-in whose account could not be determined: {err}");
            return Ok(req.into_response(json_error(StatusCode::FORBIDDEN, err.description())));
        }
    };

    let mut principal = Principal::new(
        account.clone(),
        is_admin,
        claims.as_ref().map(admin_user_from_claims),
    );

    // The registry belongs to the installation rather than to any one account,
    // so it is reached through the root context.
    if let Some(context) = req.app_data::<web::Data<AppContext>>().cloned() {
        let registry = UserRegistry::new(context.tenant(TenantId::system()));
        let display = principal.to_admin_user();

        match registry
            .record_sign_in(
                &account,
                display.as_ref().map_or("Signed in", |u| u.name.as_str()),
                display.as_ref().and_then(|u| u.email.as_deref()),
                is_admin,
            )
            .await
        {
            // A suspended account is turned away here rather than by the ACL,
            // because suspension is the one lever an administrator has that does
            // not require editing the configuration file.
            Ok(None) => {
                warn!(user.account = %account, "Refused a request from a suspended account.");
                return Ok(req.into_response(json_error(
                    StatusCode::FORBIDDEN,
                    "This account has been suspended.",
                )));
            }
            Ok(Some(_)) => {}
            Err(err) => {
                // Failing to update the registry must not lock everybody out;
                // it is a record of who has signed in, not a gate on doing so.
                warn!(error = %err, "Could not record a sign-in in the user registry.");
                services.session().record_human_error(&err);
            }
        }

        // Acting as somebody else is only meaningful where people have records
        // of their own. In a single-account installation every request already
        // reaches everything, so the header would silently do nothing — which is
        // a worse answer than saying it is not available.
        if !config.web.auth.multi_tenant && req.headers().contains_key(IMPERSONATE_HEADER) {
            return Ok(req.into_response(json_error(
                StatusCode::BAD_REQUEST,
                "This installation keeps everything in one account, so there is nobody to act as. Enable web.auth.multi_tenant first.",
            )));
        }

        match resolve_impersonation(&req, &principal, &registry, &local_tenant(&config)).await {
            Ok(Some(subject)) => {
                info!(
                    admin.account = %principal.actor(),
                    user.account = %subject,
                    "An administrator is acting as another user."
                );

                record_impersonation(&context, &req, principal.actor(), &subject).await;
                principal = principal.impersonating(subject);
            }
            Ok(None) => {}
            Err(response) => return Ok(req.into_response(response)),
        }
    }

    req.extensions_mut().insert(principal);

    next.call(req).await
}

/// Records that an administrator changed something while acting as another user.
///
/// Only requests that change something are recorded. Every request during an
/// impersonation session passes through here and the browser polls, so auditing
/// reads as well would bury the writes — which are what anyone reviewing this
/// afterwards actually wants to find. The entry is written to the impersonated
/// account's log, so the person affected can see it, and names the administrator
/// as the actor.
async fn record_impersonation(
    context: &AppContext,
    req: &ServiceRequest,
    actor: &TenantId,
    subject: &TenantId,
) {
    use crate::db::{AuditCategory, AuditEntry, AuditOutcome, AuditStore};

    if req.method().is_safe() {
        return;
    }

    let entry = AuditEntry::new(
        AuditCategory::Administration,
        "impersonated",
        AuditOutcome::Success,
    )
    .subject(subject)
    .actor(actor)
    .message(format!(
        "{actor} made a change while acting as this account."
    ))
    .detail(serde_json::json!({
        "method": req.method().as_str(),
        "path": req.path(),
    }));

    if let Err(err) = context.tenant(subject.clone()).audit().record(entry).await {
        // Losing the record is bad, but refusing the request over it would let a
        // full disk lock an administrator out mid-investigation.
        error!(error = %err, "Failed to record an impersonated action in the audit log.");
        context.session().record_human_error(&err);
    }
}

/// The account an installation with no identity provider acts as.
/// Which account a request acts as.
///
/// Signing in tells us who somebody is. Whether that should give them records of
/// their own is a separate question, and one the operator answers: an
/// installation that has been running with an identity provider already has
/// every workflow and connection under its single account, so reading the
/// signed-in identity as an account name would move all of them out from under
/// the people using them. Nothing is partitioned until `multi_tenant` says so.
///
/// Separated from the middleware so it can be tested without standing up an
/// identity provider — the decision is small and the thing it protects against
/// is an upgrade, which is exactly the case that is awkward to reach through a
/// request.
fn account_for(
    config: &crate::config::Config,
    claims: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<TenantId, human_errors::Error> {
    match claims {
        Some(claims) if config.web.auth.multi_tenant => username_from_claims(
            claims,
            config.web.oidc().and_then(|o| o.username_claim.as_deref()),
        ),
        _ => Ok(local_tenant(config)),
    }
}

fn local_tenant(config: &crate::config::Config) -> TenantId {
    config
        .web
        .auth
        .local_user
        .as_deref()
        .and_then(|name| TenantId::new(name).ok())
        .unwrap_or_else(TenantId::local)
}

/// Resolves the `X-Impersonate-User` header, if present.
///
/// Returns the account to act as, `None` when the header is absent, and an error
/// response when the request asks for something it may not have.
async fn resolve_impersonation<S: Services>(
    req: &ServiceRequest,
    principal: &Principal,
    registry: &UserRegistry<S>,
    local: &TenantId,
) -> Result<Option<TenantId>, HttpResponse> {
    use actix_web::http::StatusCode;

    let Some(requested) = req
        .headers()
        .get(IMPERSONATE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    if !principal.is_admin() {
        warn!(
            user.account = %principal.actor(),
            "Refused an impersonation attempt from an account that is not an administrator."
        );
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "Only administrators may act as another user.",
        ));
    }

    // The installation's own account owns everything from before `multi_tenant`
    // was switched on, and nobody signs into it — so it is absent from the
    // registry, and its default name is one `TenantId::new` refuses. Reaching it
    // is the whole of what an administrator needs after that switch is thrown.
    // The system tenant is deliberately not reachable this way: it holds the
    // registry and the webhook indexes, which are the agent's own bookkeeping
    // rather than anybody's records.
    if requested.eq_ignore_ascii_case(local.as_str()) {
        return Ok((local != principal.actor()).then(|| local.clone()));
    }

    let subject =
        TenantId::new(requested).map_err(|err| json_error(StatusCode::BAD_REQUEST, err))?;

    // Acting as yourself is the same as not impersonating at all, and letting it
    // through would put a misleading impersonation banner in front of an
    // administrator looking at their own records.
    if &subject == principal.actor() {
        return Ok(None);
    }

    // Requiring the account to be known stops a typo silently creating an empty
    // namespace that looks like a user with no workflows.
    match registry.get(&subject).await {
        Ok(Some(user)) if !user.disabled => Ok(Some(subject)),
        Ok(Some(_)) => Err(json_error(
            StatusCode::FORBIDDEN,
            format!("The account '{subject}' has been suspended."),
        )),
        Ok(None) => Err(json_error(
            StatusCode::NOT_FOUND,
            format!("There is no account named '{subject}'."),
        )),
        Err(err) => {
            error!(error = %err, "Failed to look up an account to impersonate.");
            Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "We could not look up that account.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};

    use crate::filter::Filter;
    use crate::services::AppContext;
    use crate::users::User;

    /// Builds a context with no identity provider and the given access filters.
    ///
    /// Leaving OIDC unconfigured keeps these tests free of a JWKS server while
    /// still exercising the whole middleware: the account resolves to the
    /// installation's local user, and `admin_acl` decides whether it may
    /// impersonate. What is being tested is the authorisation logic, which is
    /// identical either way.
    async fn context(user_acl: &str, admin_acl: &str) -> AppContext {
        AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(Filter::new(user_acl).unwrap());
            config.web.auth.admin_acl = Some(Filter::new(admin_acl).unwrap());
            // Most of what is tested here — impersonation, per-account records —
            // only exists once an operator has asked for it.
            config.web.auth.multi_tenant = true;
        })
        .await
        .unwrap()
    }

    /// Registers an account so that it can be impersonated.
    async fn register(context: &AppContext, username: &str, disabled: bool) -> User {
        let registry = UserRegistry::new(context.tenant(TenantId::system()));
        let username = TenantId::new(username).unwrap();

        registry
            .record_sign_in(&username, username.as_str(), None, false)
            .await
            .unwrap();

        if disabled {
            registry.set_disabled(&username, true).await.unwrap();
        }

        registry.get(&username).await.unwrap().unwrap()
    }

    macro_rules! app {
        ($context:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($context.tenant(TenantId::local())))
                    .app_data(web::Data::new($context.clone()))
                    .service(configure()),
            )
            .await
        };
    }

    #[actix_web::test]
    async fn acl_denial_without_oidc_is_forbidden_not_unauthorized() {
        let app = app!(context("false", "false").await);

        let req = test::TestRequest::get().uri("/api/v1/me").to_request();
        let resp = test::call_service(&app, req).await;

        // A denial while OIDC is disabled is permanent — there is nothing to sign
        // in to — so it must be a 403, never a 401 that would send the admin UI
        // into a sign-in flow that can never succeed.
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn acl_allow_without_oidc_reports_no_signed_in_user() {
        let app = app!(context("true", "true").await);

        let req = test::TestRequest::get().uri("/api/v1/me").to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[actix_web::test]
    async fn mutating_request_without_oidc_is_allowed_by_a_permissive_acl() {
        // With OIDC disabled there is no bearer to present; access is governed by
        // the ACL alone, and a bearer-only model has no CSRF token to reject a
        // mutating request on.
        let app = app!(context("true", "true").await);

        let req = test::TestRequest::delete()
            .uri("/api/v1/kv/cache?key=foo")
            .to_request();
        let resp = test::call_service(&app, req).await;

        // Reaches the handler (the ACL allows it); the key simply doesn't exist.
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn signing_in_records_the_account_in_the_registry() {
        let context = context("true", "false").await;
        let app = app!(context);

        test::call_service(
            &app,
            test::TestRequest::get().uri("/api/v1/me").to_request(),
        )
        .await;

        let registry = UserRegistry::new(context.tenant(TenantId::system()));
        let accounts = registry.list().await.unwrap();

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].username, TenantId::local());
        assert!(!accounts[0].is_admin);
    }

    #[actix_web::test]
    async fn the_local_account_can_be_named_so_adopting_oidc_keeps_existing_records() {
        let context = AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(Filter::new("true").unwrap());
            config.web.auth.local_user = Some("alice@example.com".into());
        })
        .await
        .unwrap();
        let app = app!(context);

        test::call_service(
            &app,
            test::TestRequest::get().uri("/api/v1/me").to_request(),
        )
        .await;

        let accounts = UserRegistry::new(context.tenant(TenantId::system()))
            .list()
            .await
            .unwrap();

        assert_eq!(accounts[0].username.as_str(), "alice@example.com");
    }

    /// A signed-in identity, as the provider would give it.
    fn claims_for(username: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::json!({ "preferred_username": username, "sub": "abc123" })
            .as_object()
            .unwrap()
            .clone()
    }

    async fn config_with(multi_tenant: bool) -> std::sync::Arc<crate::config::Config> {
        AppContext::new_mock(move |config| {
            config.web.auth.user_acl = Some(Filter::new("true").unwrap());
            config.web.auth.multi_tenant = multi_tenant;
        })
        .await
        .unwrap()
        .config()
    }

    #[tokio::test]
    async fn signing_in_does_not_move_anybody_out_of_the_installations_account() {
        // The upgrade this protects. An installation already running with an
        // identity provider has everything under one account; reading the
        // identity as an account name would take every workflow away from the
        // people using it, and they would be told nothing.
        let config = config_with(false).await;

        assert_eq!(
            account_for(&config, Some(&claims_for("alice"))).unwrap(),
            TenantId::local(),
        );
    }

    #[tokio::test]
    async fn once_an_operator_asks_for_it_a_sign_in_names_the_account() {
        let config = config_with(true).await;

        assert_eq!(
            account_for(&config, Some(&claims_for("alice"))).unwrap(),
            TenantId::new("alice").unwrap(),
        );
    }

    #[tokio::test]
    async fn an_installation_with_nobody_to_identify_uses_its_own_account() {
        for multi_tenant in [false, true] {
            let config = config_with(multi_tenant).await;

            assert_eq!(
                account_for(&config, None).unwrap(),
                TenantId::local(),
                "with no identity provider there is nobody to partition by",
            );
        }
    }

    #[actix_web::test]
    async fn a_single_account_installation_keeps_everything_in_one_place() {
        // The upgrade this protects: an installation that has been running with
        // an identity provider already has every workflow and connection under
        // its single account. Reading the signed-in identity as an account name
        // would move all of them out from under the people using them, so
        // nothing is partitioned until an operator says so.
        let context = AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(Filter::new("true").unwrap());
            config.web.auth.multi_tenant = false;
        })
        .await
        .unwrap();
        let app = app!(context);

        // A record that was already there before anybody signed in.
        context
            .tenant(TenantId::local())
            .kv()
            .set("notes", "existing", "still here")
            .await
            .unwrap();

        let entries: Vec<automate_api::KeyValueEntry> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get().uri("/api/v1/kv").to_request(),
        )
        .await;

        assert!(
            entries.iter().any(|entry| entry.key == "existing"),
            "a signed-in request should still reach the records that were already there",
        );

        let accounts = UserRegistry::new(context.tenant(TenantId::system()))
            .list()
            .await
            .unwrap();

        assert_eq!(
            accounts[0].username,
            TenantId::local(),
            "everybody should still be working in the installation's own account",
        );
    }

    #[actix_web::test]
    async fn acting_as_somebody_else_is_refused_where_there_is_nobody_to_act_as() {
        // Silently ignoring the header would be worse: an administrator would
        // believe they were looking at another person's records while actually
        // looking at the only set there is.
        let context = AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(Filter::new("true").unwrap());
            config.web.auth.admin_acl = Some(Filter::new("true").unwrap());
            config.web.auth.multi_tenant = false;
        })
        .await
        .unwrap();
        let app = app!(context);

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn a_suspended_account_is_refused() {
        let context = context("true", "true").await;
        let app = app!(context);

        // Sign in once so the account exists, then suspend it.
        test::call_service(
            &app,
            test::TestRequest::get().uri("/api/v1/me").to_request(),
        )
        .await;
        UserRegistry::new(context.tenant(TenantId::system()))
            .set_disabled(&TenantId::local(), true)
            .await
            .unwrap();

        let resp = test::call_service(
            &app,
            test::TestRequest::get().uri("/api/v1/me").to_request(),
        )
        .await;

        // Suspension is the one lever that does not require editing the
        // configuration file, so it has to work even against a permissive ACL.
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn impersonation_is_refused_for_an_account_that_is_not_an_administrator() {
        let context = context("true", "false").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "alice"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn an_administrator_may_act_as_another_user() {
        let context = context("true", "true").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "alice"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[actix_web::test]
    async fn impersonating_an_unknown_account_is_reported_rather_than_silently_creating_one() {
        // A typo would otherwise open an empty namespace that looks exactly like
        // a real user who happens to have no workflows.
        let app = app!(context("true", "true").await);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "nobody"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_suspended_account_cannot_be_impersonated() {
        let context = context("true", "true").await;
        register(&context, "alice", true).await;
        let app = app!(context);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "alice"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn an_impersonation_header_naming_an_unusable_account_is_a_bad_request() {
        let app = app!(context("true", "true").await);

        // The reserved namespace holds the user registry itself.
        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "!system"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Signs in as an administrator against a real identity provider.
    ///
    /// The other impersonation tests run without one, which resolves the
    /// administrator's own account to the installation's — and acting as
    /// yourself is a no-op, so the account below could never be reached from
    /// there.
    async fn an_administrator_and_an_installation_account()
    -> (crate::testing::oidc::TestIdentityProvider, AppContext) {
        let provider = crate::testing::oidc::TestIdentityProvider::start().await;
        let context = provider
            .context_with(|config| {
                config.web.auth.admin_acl = Some(Filter::new("true").unwrap());
            })
            .await;

        (provider, context)
    }

    #[actix_web::test]
    async fn the_installations_own_account_can_be_acted_as() {
        use actix_web::http::header::AUTHORIZATION;

        // Everything configured before `multi_tenant` was switched on still
        // belongs to this account, and nobody can sign into it, so acting as it
        // is the only way those records are ever reachable again.
        let (provider, context) = an_administrator_and_an_installation_account().await;
        let app = app!(context);

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header((
                    AUTHORIZATION,
                    format!("Bearer {}", provider.sign_in_as("admin")),
                ))
                .insert_header((IMPERSONATE_HEADER, TenantId::LOCAL))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let user: automate_api::AdminUser = test::read_body_json(response).await;
        assert_eq!(user.username, Some(TenantId::local()));
        assert_eq!(
            user.impersonated_by,
            Some(TenantId::new("admin").unwrap()),
            "the administrator should still be the one the change is attributed to",
        );
    }

    #[actix_web::test]
    async fn the_installations_bookkeeping_account_still_cannot_be_acted_as() {
        use actix_web::http::header::AUTHORIZATION;

        // The system tenant holds the user registry and the webhook indexes.
        // Those are the agent's own records, not somebody's, and reaching them
        // through the ordinary endpoints would put the registry in the data
        // browser where a stray delete would lock everybody out.
        let (provider, context) = an_administrator_and_an_installation_account().await;
        let app = app!(context);

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header((
                    AUTHORIZATION,
                    format!("Bearer {}", provider.sign_in_as("admin")),
                ))
                .insert_header((IMPERSONATE_HEADER, TenantId::SYSTEM))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_installations_own_account_is_listed_so_it_can_be_found() {
        use actix_web::http::header::AUTHORIZATION;

        // An administrator cannot act as an account they were never shown.
        let (provider, context) = an_administrator_and_an_installation_account().await;
        let app = app!(context);

        let accounts: Vec<automate_api::Account> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/admin/users")
                .insert_header((
                    AUTHORIZATION,
                    format!("Bearer {}", provider.sign_in_as("admin")),
                ))
                .to_request(),
        )
        .await;

        let local = accounts
            .iter()
            .find(|account| account.username == TenantId::local())
            .expect("the installation's own account should be listed");

        assert!(local.reserved);
        assert!(
            local.first_seen_at.is_none(),
            "nobody has signed into it, so it has no sign-in dates to report",
        );
    }

    #[actix_web::test]
    async fn an_empty_impersonation_header_is_ignored() {
        let app = app!(context("true", "true").await);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "   "))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[actix_web::test]
    async fn an_impersonated_change_is_recorded_against_the_account_it_affected() {
        use crate::db::{AuditQuery, AuditStore};

        let context = context("true", "true").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let req = test::TestRequest::delete()
            .uri("/api/v1/kv/notes?key=anything")
            .insert_header((IMPERSONATE_HEADER, "alice"))
            .to_request();
        test::call_service(&app, req).await;

        // Written to the impersonated account's log, so the person affected can
        // see it, and attributed to the administrator rather than to them.
        let entries = context
            .tenant(TenantId::new("alice").unwrap())
            .audit()
            .audit(AuditQuery::recent(10))
            .await
            .unwrap();

        assert_eq!(
            entries.len(),
            1,
            "expected one audit entry, got {entries:?}"
        );
        assert_eq!(entries[0].action, "impersonated");
        assert_eq!(
            entries[0].actor.as_deref(),
            Some(TenantId::local().as_str())
        );
        assert_eq!(entries[0].subject.as_deref(), Some("alice"));
        assert_eq!(entries[0].detail.as_ref().unwrap()["method"], "DELETE");
    }

    #[actix_web::test]
    async fn merely_reading_while_impersonating_is_not_audited() {
        use crate::db::{AuditQuery, AuditStore};

        // Every request during an impersonation session passes through the
        // middleware and the browser polls, so auditing reads would bury the
        // changes, which are what anyone reviewing this afterwards is looking
        // for.
        let context = context("true", "true").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let req = test::TestRequest::get()
            .uri("/api/v1/me")
            .insert_header((IMPERSONATE_HEADER, "alice"))
            .to_request();
        test::call_service(&app, req).await;

        assert!(
            context
                .tenant(TenantId::new("alice").unwrap())
                .audit()
                .audit(AuditQuery::recent(10))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[actix_web::test]
    async fn a_handler_reads_the_records_of_the_account_being_acted_for() {
        use crate::db::KeyValueStore;

        let context = context("true", "true").await;
        register(&context, "alice", false).await;

        // The same partition and key for both accounts, so a handler that
        // ignored the tenant would return the wrong one rather than nothing.
        context
            .tenant(TenantId::local())
            .kv()
            .set("notes", "shared", "the administrator's note")
            .await
            .unwrap();
        context
            .tenant(TenantId::new("alice").unwrap())
            .kv()
            .set("notes", "shared", "alice's note")
            .await
            .unwrap();

        let app = app!(context);

        let own: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get().uri("/api/v1/kv").to_request(),
        )
        .await;
        assert_eq!(own[0]["payload"], "the administrator's note");

        // While impersonating, the administrator sees exactly what that user
        // would see.
        let impersonated: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/kv")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .to_request(),
        )
        .await;
        assert_eq!(impersonated[0]["payload"], "alice's note");
    }

    #[actix_web::test]
    async fn a_write_lands_on_the_impersonated_account_not_the_administrators() {
        use crate::db::KeyValueStore;

        let context = context("true", "true").await;
        register(&context, "alice", false).await;

        for tenant in [TenantId::local(), TenantId::new("alice").unwrap()] {
            context
                .tenant(tenant)
                .kv()
                .set("notes", "shared", "present")
                .await
                .unwrap();
        }

        let app = app!(context);

        test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/api/v1/kv/notes?key=shared")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .to_request(),
        )
        .await;

        assert_eq!(
            context
                .tenant(TenantId::new("alice").unwrap())
                .kv()
                .get::<String>("notes", "shared")
                .await
                .unwrap(),
            None
        );
        assert!(
            context
                .tenant(TenantId::local())
                .kv()
                .get::<String>("notes", "shared")
                .await
                .unwrap()
                .is_some(),
            "the administrator's own record should be untouched"
        );
    }

    #[actix_web::test]
    async fn installation_wide_endpoints_are_refused_to_non_administrators() {
        let app = app!(context("true", "false").await);

        for uri in ["/api/v1/admin/users", "/api/v1/admin/audit"] {
            let resp =
                test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;

            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{uri} should be refused to a non-administrator"
            );
        }
    }

    #[actix_web::test]
    async fn an_administrator_can_list_and_suspend_accounts() {
        let context = context("true", "true").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let users: Vec<serde_json::Value> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/admin/users")
                .to_request(),
        )
        .await;
        assert!(users.iter().any(|u| u["username"] == "alice"));

        let resp = test::call_service(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/admin/users/alice")
                .set_json(serde_json::json!({ "disabled": true }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        assert!(
            UserRegistry::new(context.tenant(TenantId::system()))
                .get(&TenantId::new("alice").unwrap())
                .await
                .unwrap()
                .unwrap()
                .disabled
        );
    }

    #[actix_web::test]
    async fn suspending_an_unknown_account_is_reported() {
        let app = app!(context("true", "true").await);

        let resp = test::call_service(
            &app,
            test::TestRequest::patch()
                .uri("/api/v1/admin/users/nobody")
                .set_json(serde_json::json!({ "disabled": true }))
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn impersonation_cannot_be_used_to_reach_administrative_endpoints() {
        // Administrator status belongs to whoever signed in, so acting as an
        // administrator must not confer it.
        let context = context("true", "false").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/admin/users")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn a_connection_can_be_created_listed_renamed_and_removed() {
        let app = app!(context("true", "true").await);

        let created: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({
                    "provider": "todoist",
                    "name": "Personal",
                    "key": "tok-Zx91",
                }))
                .to_request(),
        )
        .await;

        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["provider"], "todoist");
        assert_eq!(created["kind"], "api_key");
        assert_eq!(created["status"], "ok");

        let listed: Vec<serde_json::Value> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/connections")
                .to_request(),
        )
        .await;
        assert_eq!(listed.len(), 1);

        let renamed: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::patch()
                .uri(&format!("/api/v1/connections/{id}"))
                .set_json(serde_json::json!({ "name": "Work" }))
                .to_request(),
        )
        .await;
        assert_eq!(renamed["name"], "Work");

        let resp = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/connections/{id}"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("/api/v1/connections/{id}"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn the_api_never_returns_a_stored_credential() {
        let app = app!(context("true", "true").await);

        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({ "provider": "todoist", "key": "tok-Zx91" }))
                .to_request(),
        )
        .await;

        for uri in ["/api/v1/connections", "/api/v1/kv"] {
            let body =
                test::call_and_read_body(&app, test::TestRequest::get().uri(uri).to_request())
                    .await;

            assert!(
                !String::from_utf8_lossy(&body).contains("tok-Zx91"),
                "{uri} returned the stored credential"
            );
        }
    }

    #[actix_web::test]
    async fn connections_belong_to_the_account_being_acted_for() {
        let context = context("true", "true").await;
        register(&context, "alice", false).await;
        let app = app!(context);

        // Created while acting as alice, so it is hers rather than the
        // administrator's.
        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .set_json(serde_json::json!({ "provider": "todoist", "key": "tok" }))
                .to_request(),
        )
        .await;

        let own: Vec<serde_json::Value> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/connections")
                .to_request(),
        )
        .await;
        assert!(
            own.is_empty(),
            "the administrator should have no connections"
        );

        let hers: Vec<serde_json::Value> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/connections")
                .insert_header((IMPERSONATE_HEADER, "alice"))
                .to_request(),
        )
        .await;
        assert_eq!(hers.len(), 1);
    }

    #[actix_web::test]
    async fn a_mistyped_connection_identifier_says_which_word_was_wrong() {
        let app = app!(context("true", "true").await);

        let body = test::call_and_read_body(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/connections/abandon-notaword")
                .to_request(),
        )
        .await;

        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("notaword"), "unhelpful error: {body}");
    }

    #[actix_web::test]
    async fn creating_a_connection_without_a_credential_is_refused() {
        let app = app!(context("true", "true").await);

        for body in [
            serde_json::json!({ "provider": "todoist", "key": "   " }),
            serde_json::json!({ "provider": "  ", "key": "tok" }),
        ] {
            let resp = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/api/v1/connections")
                    .set_json(body)
                    .to_request(),
            )
            .await;

            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[actix_web::test]
    async fn choices_are_refused_for_a_connection_that_offers_none() {
        let app = app!(context("true", "true").await);

        let created: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({ "provider": "spotify", "key": "tok" }))
                .to_request(),
        )
        .await;

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/api/v1/connections/{}/options/projects",
                    created["id"].as_str().unwrap()
                ))
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn choices_for_an_unknown_connection_are_not_found() {
        let app = app!(context("true", "true").await);

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/connections/abandon-abandon/options/projects")
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn an_unknown_kind_of_choice_is_not_found() {
        let app = app!(context("true", "true").await);

        let created: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({ "provider": "todoist", "key": "tok" }))
                .to_request(),
        )
        .await;

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/api/v1/connections/{}/options/nonsense",
                    created["id"].as_str().unwrap()
                ))
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn listing_sections_without_naming_a_project_is_refused() {
        let app = app!(context("true", "true").await);

        let created: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({ "provider": "todoist", "key": "tok" }))
                .to_request(),
        )
        .await;

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/api/v1/connections/{}/options/sections",
                    created["id"].as_str().unwrap()
                ))
                .to_request(),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn bearer_extraction_accepts_either_capitalisation() {
        use actix_web::http::header::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer abc.def.ghi"),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc.def.ghi"));

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("bearer abc.def.ghi"),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc.def.ghi"));

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }
}
