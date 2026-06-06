//! Browser-side helpers for the server-driven OIDC session.
//!
//! Authentication is handled entirely by the agent: there are no tokens or PKCE
//! material in the browser. Signing in is a full-page navigation to the agent's
//! login endpoint (which redirects to the identity provider and back, setting an
//! `HttpOnly` session cookie). Signing out posts to the logout endpoint and then
//! returns to the app root.

use crate::api;

/// Returns the browser window, panicking if it is somehow unavailable (it always
/// is in a wasm context).
fn window() -> web_sys::Window {
    web_sys::window().expect("a browser window should be available")
}

/// The current path + query + hash, used as the post-login destination so the
/// user returns to where they were after authenticating.
fn current_path() -> String {
    let location = window().location();
    let path = location.pathname().unwrap_or_else(|_| "/".to_string());
    let search = location.search().unwrap_or_default();
    let hash = location.hash().unwrap_or_default();
    format!("{path}{search}{hash}")
}

/// Begins the login flow by navigating the browser to the agent's login
/// endpoint, preserving the current location as the post-login destination.
pub fn begin_login() {
    let return_to = current_path();
    let encoded: String = js_sys::encode_uri_component(&return_to).into();
    let url = format!("/api/v1/auth/login?return_to={encoded}");
    let _ = window().location().set_href(&url);
}

/// Signs the user out by clearing the server session, then returns to the app
/// root.
pub async fn logout() {
    let _ = api::logout().await;
    let _ = window().location().set_href("/");
}
