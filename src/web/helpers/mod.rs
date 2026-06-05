//! Reusable helpers shared across the web endpoints.
//!
//! Modules at the `web::*` level are intended to focus on wiring up endpoints
//! and the trivial helpers used only within a single endpoint group. Anything
//! that is reused across endpoint groups — interpreting forwarded request
//! metadata, CSRF tokens, and the OpenID Connect machinery — lives here.

pub mod csrf;
pub mod oidc;
pub mod request;
