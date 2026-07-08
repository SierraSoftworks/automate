//! Browser-side OIDC Authorization Code login (without PKCE), via a popup.
//!
//! A public browser client must never hold the client secret, so the agent performs the
//! confidential code exchange on the SPA's behalf: the browser runs the authorization request in a
//! popup, the popup POSTs the resulting `code` to the agent, which exchanges it (using its
//! server-held secret) and returns an ID token plus a refresh token.
//!
//! Sign-in uses a popup so the main page is never navigated away from: [`begin_login`] opens the
//! authorization URL in a popup and waits for it to report success. The popup loads the SPA at the
//! callback URL, [`complete_callback`] exchanges the code and hands the tokens back to the opener
//! through a short-lived `localStorage` slot (popups don't share `sessionStorage` with their
//! opener), then closes. The opener stores the tokens in `sessionStorage` and sends the ID token as
//! an `Authorization: Bearer` header (see [`crate::api`]). The refresh token lets [`refresh_session`]
//! renew an expired ID token without another interactive login.
//!
//! Everything the browser calls is same-origin (the agent), so the provider never needs to permit
//! cross-origin requests.

use std::time::Duration;

use base64::prelude::*;
use serde::Deserialize;

/// sessionStorage key holding the current ID token (the admin bearer).
const TOKEN_KEY: &str = "automate.admin.token";
/// sessionStorage key holding the refresh token used to renew the session.
const REFRESH_KEY: &str = "automate.admin.refresh";
/// sessionStorage key holding the in-flight OAuth `state` value.
const STATE_KEY: &str = "automate.oidc.state";
/// Short-lived `localStorage` slot the popup uses to hand tokens back to its opener.
const POPUP_RESULT_KEY: &str = "automate.oidc.popup_result";
/// The OAuth redirect lands on this SPA route; the app detects `?code&state` there and finishes the
/// exchange (see [`crate::pages::AuthCallback`]).
const CALLBACK_PATH: &str = "/auth/callback";
/// How long the opener waits between polls of the popup handoff slot.
const POPUP_POLL_INTERVAL: Duration = Duration::from_millis(300);
const POPUP_MAX_POLLS: u32 = 2_000; // ~10 minutes

mod single_flight {
    //! A single-flight coordinator: it collapses concurrent runs of an async operation onto one
    //! shared execution so the operation runs at most once at a time, no matter how many callers ask
    //! for it together (see [`super::refresh_session`] for why that matters). It stores its state in
    //! an `Rc`/`RefCell`, so it lives behind a `thread_local` rather than a lock.

    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::rc::Rc;

    use futures::FutureExt;
    use futures::future::{LocalBoxFuture, Shared};

    struct Inner<T: Clone> {
        /// The run currently in flight, tagged with the generation that started it; cloned for every
        /// caller that joins it and cleared once it settles, so the next call starts a fresh run.
        current: RefCell<Option<(u64, Shared<LocalBoxFuture<'static, T>>)>>,
        generation: Cell<u64>,
    }

    pub struct SingleFlight<T: Clone> {
        inner: Rc<Inner<T>>,
    }

    impl<T: Clone + 'static> SingleFlight<T> {
        pub fn new() -> Self {
            Self {
                inner: Rc::new(Inner {
                    current: RefCell::new(None),
                    generation: Cell::new(0),
                }),
            }
        }

        /// Runs the future produced by `start`, unless a run begun by an earlier, still-unsettled
        /// call is already in flight — in which case that one is joined instead and `start` is never
        /// invoked. The returned future is self-contained (it owns a handle to the shared state), so
        /// it can be awaited outside the borrow that produced it.
        pub fn run<F>(&self, start: F) -> impl Future<Output = T> + use<F, T>
        where
            F: FnOnce() -> LocalBoxFuture<'static, T>,
        {
            let inner = self.inner.clone();
            async move {
                let (generation, shared) = {
                    let mut current = inner.current.borrow_mut();
                    if let Some((generation, existing)) = current.as_ref() {
                        (*generation, existing.clone())
                    } else {
                        let generation = inner.generation.get().wrapping_add(1);
                        inner.generation.set(generation);
                        let shared = start().shared();
                        *current = Some((generation, shared.clone()));
                        (generation, shared)
                    }
                };

                let result = shared.await;

                // Retire this run so the next call starts afresh, but only if a later run hasn't
                // already replaced it — otherwise we'd strand the newer one and let a second
                // concurrent run start.
                let mut current = inner.current.borrow_mut();
                if matches!(current.as_ref(), Some((g, _)) if *g == generation) {
                    *current = None;
                }
                result
            }
        }
    }
}

use futures::FutureExt;
use single_flight::SingleFlight;

/// Returns the browser window, panicking if it is somehow unavailable (it always is in a wasm
/// context).
fn window() -> web_sys::Window {
    web_sys::window().expect("a browser window should be available")
}

fn session() -> Option<web_sys::Storage> {
    window().session_storage().ok().flatten()
}

fn local() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

/// The stored ID token, if a session is active.
pub fn stored_token() -> Option<String> {
    session()?.get_item(TOKEN_KEY).ok().flatten()
}

fn stored_refresh_token() -> Option<String> {
    session()?.get_item(REFRESH_KEY).ok().flatten()
}

/// Persists the ID token, and the refresh token when one was issued (providers that don't rotate
/// refresh tokens omit it, so we keep any one we already hold).
fn store_tokens(token: &str, refresh: Option<&str>) {
    if let Some(storage) = session() {
        let _ = storage.set_item(TOKEN_KEY, token);
        if let Some(refresh) = refresh {
            let _ = storage.set_item(REFRESH_KEY, refresh);
        }
    }
}

/// Drops the stored session (used on sign-out and when a refresh fails).
pub fn clear_token() {
    if let Some(storage) = session() {
        let _ = storage.remove_item(TOKEN_KEY);
        let _ = storage.remove_item(REFRESH_KEY);
    }
}

/// Percent-encodes a URL query-component value.
fn enc(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}

/// A high-entropy, URL-safe random token for the OAuth `state` value.
fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if let Ok(crypto) = window().crypto() {
        let _ = crypto.get_random_values_with_u8_array(&mut buf);
    }
    BASE64_URL_SAFE_NO_PAD.encode(&buf)
}

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

fn redirect_uri() -> String {
    format!("{}{CALLBACK_PATH}", origin())
}

/// Whether this window was opened as a login popup (it has an opener). Used to decide whether
/// [`complete_callback`] should hand tokens back and close, or store them and stay.
fn is_popup() -> bool {
    window()
        .opener()
        .map(|opener| !opener.is_null() && !opener.is_undefined())
        .unwrap_or(false)
}

/// The parameters the SPA needs to begin a login, fetched from the agent (same-origin) so the
/// browser never reads the provider's discovery document cross-origin.
#[derive(Deserialize)]
struct AuthMetadata {
    authorization_endpoint: String,
    client_id: String,
    #[serde(default)]
    scopes: Vec<String>,
}

async fn fetch_metadata() -> Result<AuthMetadata, String> {
    let response = gloo_net::http::Request::get("/api/v1/auth/metadata")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!(
            "the agent could not provide the sign-in parameters (HTTP {})",
            response.status()
        ));
    }
    response
        .json::<AuthMetadata>()
        .await
        .map_err(|e| e.to_string())
}

fn build_authorize_url(meta: &AuthMetadata, state: &str) -> String {
    let mut scopes = vec!["openid".to_string()];
    scopes.extend(meta.scopes.iter().filter(|s| *s != "openid").cloned());
    let scope = scopes.join(" ");
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        meta.authorization_endpoint,
        enc(&meta.client_id),
        enc(&redirect_uri()),
        enc(&scope),
        enc(state),
    )
}

fn callback_params() -> Option<(String, String)> {
    let search = window().location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    Some((params.get("code")?, params.get("state")?))
}

/// Begins an interactive sign-in: opens the provider's authorization URL in a popup and waits for it
/// to report success, returning the new ID token. Returns `Ok(None)` if the popup is dismissed
/// without completing. Must be called from a user gesture so the popup isn't blocked.
pub async fn begin_login() -> Result<Option<String>, String> {
    let state = random_token(24);
    if let Some(storage) = session() {
        let _ = storage.set_item(STATE_KEY, &state);
    }
    // Clear any stale handoff from a previous attempt before opening the popup.
    if let Some(storage) = local() {
        let _ = storage.remove_item(POPUP_RESULT_KEY);
    }

    let meta = fetch_metadata().await?;
    let url = build_authorize_url(&meta, &state);

    let popup = window()
        .open_with_url_and_target_and_features(&url, "automate-login", "popup,width=480,height=720")
        .map_err(|_| "the browser blocked the sign-in popup".to_string())?;

    let Some(popup) = popup else {
        return Err("the browser blocked the sign-in popup".to_string());
    };

    // Poll the handoff slot until the popup reports tokens or is closed.
    for _ in 0..POPUP_MAX_POLLS {
        if let Some(result) = local().and_then(|s| s.get_item(POPUP_RESULT_KEY).ok().flatten()) {
            if let Some(storage) = local() {
                let _ = storage.remove_item(POPUP_RESULT_KEY);
            }
            let tokens: serde_json::Value =
                serde_json::from_str(&result).map_err(|e| e.to_string())?;
            let token = tokens["token"]
                .as_str()
                .ok_or("the sign-in response did not include a token")?
                .to_string();
            store_tokens(&token, tokens["refresh_token"].as_str());
            return Ok(Some(token));
        }
        if popup.closed().unwrap_or(false) {
            return Ok(None);
        }
        gloo_timers::future::sleep(POPUP_POLL_INTERVAL).await;
    }
    Err("the sign-in popup did not complete in time".to_string())
}

/// If the current URL is an OIDC callback, exchange the code for tokens. In a popup, the tokens are
/// handed back to the opener and the popup closes (returning `None`); otherwise (a direct
/// navigation) the tokens are stored and the new ID token is returned. `None` means there was no
/// callback to process or the work was delegated to the opener.
pub async fn complete_callback() -> Result<Option<String>, String> {
    let Some((code, state)) = callback_params() else {
        return Ok(None);
    };
    let storage = session().ok_or("session storage is unavailable")?;

    let expected_state = storage.get_item(STATE_KEY).ok().flatten();
    if expected_state.as_deref() != Some(state.as_str()) {
        return Err("the login response state did not match (possible CSRF or stale login)".into());
    }

    // The agent exchanges the code with its client secret and returns the tokens.
    let body = serde_json::json!({ "code": code, "redirect_uri": redirect_uri() });
    let response = gloo_net::http::Request::post("/api/v1/auth/token")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.ok() {
        return Err(format!(
            "the sign-in could not be completed (HTTP {})",
            response.status()
        ));
    }

    let tokens: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let token = tokens["token"]
        .as_str()
        .ok_or("the sign-in response did not include a token")?
        .to_string();

    let _ = storage.remove_item(STATE_KEY);

    if is_popup() {
        // Hand the tokens back to the opener through localStorage, then close. The opener picks them
        // up in `begin_login` and stores them in its own sessionStorage.
        if let Some(local) = local() {
            let _ = local.set_item(POPUP_RESULT_KEY, &tokens.to_string());
        }
        let _ = window().close();
        return Ok(None);
    }

    // Direct navigation (popup was blocked and we fell back, or the user opened the link): store the
    // tokens and scrub the code/state from the address bar.
    store_tokens(&token, tokens["refresh_token"].as_str());
    if let Ok(history) = window().history() {
        let _ =
            history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(CALLBACK_PATH));
    }

    Ok(Some(token))
}

thread_local! {
    /// Coalesces concurrent, 401-driven refreshes onto a single redemption of the stored refresh
    /// token (see [`refresh_session`]).
    static REFRESH: SingleFlight<Result<String, String>> = SingleFlight::new();
}

/// Renews the session from the stored refresh token, returning a fresh ID token.
///
/// Concurrent callers are coalesced onto a single renewal: the first starts the refresh and the rest
/// await its result. This matters because providers may *rotate* the refresh token — redeeming it
/// returns a new one and invalidates the old. Independent polling loops in the SPA can wake together
/// and all hit a 401 at once; were each to redeem the stored refresh token independently, only the
/// first would succeed and the rest would spuriously fail. Sharing one redemption avoids that race.
pub async fn refresh_session() -> Result<String, String> {
    REFRESH
        .with(|flight| flight.run(|| do_refresh_session().boxed_local()))
        .await
}

/// Performs a single session renewal against the agent. The agent uses its server-held secret to
/// perform the refresh, so the browser only supplies the refresh token. On failure the stored
/// session is dropped so the UI can prompt for an interactive sign-in. Callers go through
/// [`refresh_session`], which coalesces concurrent renewals onto one invocation of this.
async fn do_refresh_session() -> Result<String, String> {
    let refresh = stored_refresh_token().ok_or("no refresh token is available")?;
    let body = serde_json::json!({ "refresh_token": refresh });
    let response = gloo_net::http::Request::post("/api/v1/auth/refresh")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.ok() {
        clear_token();
        return Err(format!(
            "the session could not be renewed (HTTP {})",
            response.status()
        ));
    }

    let tokens: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let token = tokens["token"]
        .as_str()
        .ok_or("the refresh response did not include a token")?
        .to_string();
    store_tokens(&token, tokens["refresh_token"].as_str());
    Ok(token)
}

/// Signs the user out by clearing the stored session.
pub fn logout() {
    clear_token();
}
