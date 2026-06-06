use serde::{Deserialize, Serialize};

/// The response body of `GET /api/v1/csrf`, carrying the double-submit CSRF
/// token the browser must echo back in the `X-CSRF-Token` header on mutating
/// requests. The same value is also set as a (non-`HttpOnly`) cookie so the
/// server can compare the two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsrfToken {
    pub token: String,
}
