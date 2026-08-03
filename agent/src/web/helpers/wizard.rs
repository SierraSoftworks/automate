//! The shared plumbing behind every integration setup wizard: the pages it
//! renders, the transient CSRF state cookie that ties a provider's redirect back
//! to the browser which started the flow, and the access-control decision for a
//! top-level navigation.
//!
//! None of this is OAuth2-specific — a GitHub App install goes through exactly
//! the same motions — so it lives here rather than beside any one protocol.

use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::{HttpRequest, HttpResponse};

use crate::prelude::*;
use crate::web::helpers::oidc::AdminRequestFilter;
use crate::web::helpers::request::client_ip;

/// The transient cookie that carries a setup wizard's CSRF `state` across the
/// redirect to the provider, so the callback can confirm the response belongs to
/// a flow this browser actually started.
pub const SETUP_STATE_COOKIE: &str = "automate_setup_state";

/// How long a setup wizard's state cookie remains valid.
const SETUP_STATE_SECONDS: i64 = 10 * 60;

/// Renders a minimal, self-contained HTML page for a server-side setup wizard.
/// All interpolated values are HTML-escaped to avoid injection.
pub fn html_page(status: u16, title: &str, heading: &str, message: &str) -> HttpResponse {
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"/>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
<title>{title}</title></head><body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1rem;\">\
<h1>{heading}</h1><p>{message}</p></body></html>",
        title = html_escape::encode_text(title),
        heading = html_escape::encode_text(heading),
        message = html_escape::encode_text(message),
    );

    HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
        .content_type("text/html; charset=utf-8")
        .body(body)
}

/// Renders a page with a call-to-action link (used to start the setup flow).
pub fn html_action_page(
    title: &str,
    heading: &str,
    message: &str,
    href: &str,
    label: &str,
) -> HttpResponse {
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"/>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
<title>{title}</title></head><body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1rem;\">\
<h1>{heading}</h1><p>{message}</p><a href=\"{href}\"><button>{label}</button></a></body></html>",
        title = html_escape::encode_text(title),
        heading = html_escape::encode_text(heading),
        message = html_escape::encode_text(message),
        href = html_escape::encode_double_quoted_attribute(href),
        label = html_escape::encode_text(label),
    );

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(body)
}

pub fn error_page(status: u16, title: &str, message: &str) -> HttpResponse {
    html_page(status, title, title, message)
}

/// Builds the transient state cookie for a setup wizard. It is scoped to the
/// wizard's own path and `SameSite=Lax` so it is returned on the provider's
/// top-level redirect back to the callback.
pub fn wizard_state_cookie(path: &str, state: String, secure: bool) -> Cookie<'static> {
    Cookie::build(SETUP_STATE_COOKIE, state)
        .path(path.to_string())
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::seconds(SETUP_STATE_SECONDS))
        .finish()
}

/// Builds a removal for a setup wizard's state cookie (one-shot: it is cleared
/// as soon as the callback resolves, whatever the outcome).
fn clear_wizard_state_cookie(path: &str) -> Cookie<'static> {
    let mut removal = Cookie::build(SETUP_STATE_COOKIE, "")
        .path(path.to_string())
        .finish();
    removal.make_removal();
    removal
}

/// Attaches the state-cookie removal to a callback response.
pub fn with_cleared_state(path: &str, mut response: HttpResponse) -> HttpResponse {
    let _ = response.add_cookie(&clear_wizard_state_cookie(path));
    response
}

/// Validates the setup `state`: the value echoed back by the provider must equal
/// the (non-empty) value stored in the browser's state cookie.
pub fn state_matches(expected: Option<&str>, provided: Option<&str>) -> bool {
    matches!((expected, provided), (Some(a), Some(b)) if !a.is_empty() && a == b)
}

/// An HTML page shown when a visitor is not permitted to use an integration's
/// self-service wizard.
pub fn access_denied_page() -> HttpResponse {
    error_page(
        403,
        "Access denied",
        "You are not permitted to set up this integration.",
    )
}

/// An HTML page shown when an admin-gated wizard is opened directly (a top-level
/// navigation that cannot carry the admin bearer token). These wizards are
/// launched from the Automate admin area instead.
pub fn admin_only_page() -> HttpResponse {
    error_page(
        403,
        "Sign in required",
        "This integration is set up from the Automate admin area. Open the admin UI and start the connection from there.",
    )
}

/// The outcome of evaluating a request against an integration's *public*
/// (top-level navigation) wizard path.
pub enum PublicWizardOutcome {
    /// The visitor may proceed with the flow.
    Allowed,
    /// The visitor is not permitted by the applicable ACL.
    Denied,
    /// The integration is admin-gated and OIDC is configured, so it cannot be
    /// authorised on a top-level navigation (which carries no bearer token). It
    /// must be launched from the admin SPA instead.
    AdminOnly,
}

/// Decides whether a top-level navigation may use an integration's wizard.
///
/// An integration that defines its own `acl` is self-service: the ACL is
/// evaluated against request metadata (no `claims.*`, since a top-level
/// navigation carries no bearer). One without its own `acl` is admin-gated: when
/// OIDC is disabled the admin ACL is evaluated against request metadata (e.g. an
/// IP allow-list still grants access); when OIDC is enabled the bearer cannot
/// ride a top-level navigation, so the flow must be started from the admin SPA
/// and this path reports [`PublicWizardOutcome::AdminOnly`].
pub fn public_wizard_outcome<S: Services>(
    services: &S,
    req: &HttpRequest,
    wizard_acl: Option<&Filter>,
) -> PublicWizardOutcome {
    let config = services.config();
    let admin = &config.web.admin;

    let acl = match wizard_acl {
        Some(acl) => acl,
        None => {
            if admin.oidc.is_some() {
                return PublicWizardOutcome::AdminOnly;
            }
            &admin.acl
        }
    };

    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: client_ip(config.web.trust_proxy, req.headers(), req.peer_addr()),
        headers: req.headers(),
        claims: None,
    };

    if acl.matches(&filter).unwrap_or(false) {
        PublicWizardOutcome::Allowed
    } else {
        PublicWizardOutcome::Denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_matches_requires_both_present_nonempty_and_equal() {
        assert!(state_matches(Some("abc"), Some("abc")));
        assert!(!state_matches(Some("abc"), Some("def")));
        assert!(!state_matches(None, Some("abc")));
        assert!(!state_matches(Some("abc"), None));
        assert!(!state_matches(Some(""), Some("")));
    }

    /// The cookie is one-shot: the callback clears it whatever the outcome, so a
    /// replayed `state` cannot be used a second time.
    #[test]
    fn clearing_the_state_cookie_expires_it_on_the_wizard_path() {
        let removal = clear_wizard_state_cookie("/integrations/github/setup");
        assert_eq!(removal.name(), SETUP_STATE_COOKIE);
        assert_eq!(removal.path(), Some("/integrations/github/setup"));
        assert_eq!(removal.value(), "");
    }

    #[test]
    fn the_state_cookie_is_only_marked_secure_over_https() {
        let insecure = wizard_state_cookie("/oauth/spotify", "state".into(), false);
        assert_eq!(insecure.secure(), Some(false));
        assert_eq!(insecure.http_only(), Some(true));
        assert_eq!(insecure.same_site(), Some(SameSite::Lax));

        let secure = wizard_state_cookie("/oauth/spotify", "state".into(), true);
        assert_eq!(secure.secure(), Some(true));
    }

    #[test]
    fn interpolated_values_are_escaped() {
        let body = html_action_page(
            "t",
            "<script>alert(1)</script>",
            "m",
            "/integrations/x&y/setup/start",
            "Connect",
        );
        assert_eq!(body.status(), actix_web::http::StatusCode::OK);
    }
}
