//! Thin client over the agent's `/api/v1` REST endpoints.
//!
//! Authentication relies on the agent's `HttpOnly` session cookie, which the
//! browser attaches automatically to same-origin requests — the UI never sees a
//! token. Mutating requests additionally carry a double-submit CSRF token in the
//! `X-CSRF-Token` header (fetched once from `/api/v1/csrf` and cached). A `401`
//! first triggers a transparent session renewal ([`crate::auth::refresh_session`])
//! and a single retry; only when the session truly cannot be renewed is it
//! surfaced as [`ApiError::Unauthorized`] so callers can redirect to login.

use std::cell::RefCell;

use automate_api::{AdminUser, CsrfToken, KeyValueEntry, QueueMessage};
use gloo_net::http::Request;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The base path of the REST API. Requests are made relative to the current
/// origin so the same bundle works behind any host.
const API_BASE: &str = "/api/v1";

/// The header carrying the double-submit CSRF token on mutating requests.
const CSRF_HEADER: &str = "X-CSRF-Token";

thread_local! {
    /// The cached CSRF token for this document, fetched lazily on the first
    /// mutating request and refreshed if the server rejects it.
    static CSRF_TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// An error returned by an API call.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The request was rejected because the session is missing or invalid.
    Unauthorized,
    /// The caller's account is not permitted to perform the action.
    Forbidden,
    /// A transport-level failure (the request never produced a response).
    Network(String),
    /// The server returned an error response with the given message.
    Server(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "Your session has expired. Please sign in again."),
            ApiError::Forbidden => {
                write!(f, "Your account is not permitted to perform this action.")
            }
            ApiError::Network(msg) => write!(f, "Network error: {msg}"),
            ApiError::Server(msg) => write!(f, "{msg}"),
        }
    }
}

#[derive(serde::Deserialize)]
struct ServerError {
    error: String,
}

/// Reads the cached CSRF token, if one has been fetched.
fn cached_csrf() -> Option<String> {
    CSRF_TOKEN.with(|t| t.borrow().clone())
}

/// Fetches a fresh CSRF token from the server and caches it.
async fn fetch_csrf() -> Result<String, ApiError> {
    let url = format!("{API_BASE}/csrf");
    let resp = send_with_session(|| Request::get(&url).build()).await?;
    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }
    let token = resp
        .json::<CsrfToken>()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?
        .token;
    CSRF_TOKEN.with(|t| *t.borrow_mut() = Some(token.clone()));
    Ok(token)
}

/// Returns a usable CSRF token, fetching one if it is not already cached.
async fn ensure_csrf() -> Result<String, ApiError> {
    match cached_csrf() {
        Some(token) => Ok(token),
        None => fetch_csrf().await,
    }
}

/// Clears the cached CSRF token so the next mutating request fetches a fresh one.
fn invalidate_csrf() {
    CSRF_TOKEN.with(|t| *t.borrow_mut() = None);
}

/// Converts a non-success response into an [`ApiError`], reading the JSON error
/// body when available.
async fn error_from_response(resp: gloo_net::http::Response) -> ApiError {
    let status = resp.status();
    if status == 401 {
        return ApiError::Unauthorized;
    }
    if status == 403 {
        return ApiError::Forbidden;
    }
    match resp.json::<ServerError>().await {
        Ok(body) => ApiError::Server(body.error),
        Err(_) => ApiError::Server(format!(
            "The server returned an unexpected error ({status})."
        )),
    }
}

/// Sends a request, transparently renewing the session and retrying once when
/// the server rejects it with a `401` (the session cookie lapsed, but the agent
/// may hold a refresh token it can redeem). The renewal is coalesced across
/// concurrent callers, so a page's parallel fetches failing together produce a
/// single refresh. When renewal fails the original `401` is returned untouched.
async fn send_with_session<F>(build: F) -> Result<gloo_net::http::Response, ApiError>
where
    F: Fn() -> Result<Request, gloo_net::Error>,
{
    let net = |e: gloo_net::Error| ApiError::Network(e.to_string());
    let resp = build().map_err(net)?.send().await.map_err(net)?;
    if resp.status() != 401 {
        return Ok(resp);
    }
    if crate::auth::refresh_session().await.is_err() {
        return Ok(resp);
    }
    build().map_err(net)?.send().await.map_err(net)
}

/// Performs a GET request and deserializes the JSON response body.
async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    let url = format!("{API_BASE}{path}");
    let resp = send_with_session(|| Request::get(&url).build()).await?;

    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }

    resp.json::<T>()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))
}

/// Performs a POST request with a JSON body, attaching the CSRF token. On a
/// `403` (a stale CSRF token) the token is refreshed and the request retried
/// once.
async fn post_empty<B: Serialize>(path: &str, body: &B) -> Result<(), ApiError> {
    let url = format!("{API_BASE}{path}");
    let mut refreshed = false;
    loop {
        let token = ensure_csrf().await?;
        let resp =
            send_with_session(|| Request::post(&url).header(CSRF_HEADER, &token).json(body))
                .await?;

        if resp.ok() {
            return Ok(());
        }

        if resp.status() == 403 && !refreshed {
            invalidate_csrf();
            fetch_csrf().await?;
            refreshed = true;
            continue;
        }

        return Err(error_from_response(resp).await);
    }
}

/// Performs a DELETE request, attaching the CSRF token. On a `403` (a stale CSRF
/// token) the token is refreshed and the request retried once.
async fn delete(path: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}{path}");
    let mut refreshed = false;
    loop {
        let token = ensure_csrf().await?;
        let resp =
            send_with_session(|| Request::delete(&url).header(CSRF_HEADER, &token).build())
                .await?;

        if resp.ok() {
            return Ok(());
        }

        if resp.status() == 403 && !refreshed {
            invalidate_csrf();
            fetch_csrf().await?;
            refreshed = true;
            continue;
        }

        return Err(error_from_response(resp).await);
    }
}

/// Clears the server-side session.
pub async fn logout() -> Result<(), ApiError> {
    post_empty("/auth/logout", &serde_json::Value::Null).await
}

/// Fetches the signed-in user's identity, if any.
pub async fn me() -> Result<Option<AdminUser>, ApiError> {
    let url = format!("{API_BASE}/me");
    let resp = send_with_session(|| Request::get(&url).build()).await?;

    if resp.status() == 204 {
        return Ok(None);
    }
    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }
    resp.json::<AdminUser>()
        .await
        .map(Some)
        .map_err(|e| ApiError::Network(e.to_string()))
}

/// Lists every key-value entry across all partitions.
pub async fn list_kv() -> Result<Vec<KeyValueEntry>, ApiError> {
    get_json("/kv").await
}

/// Deletes a single key-value entry.
pub async fn delete_kv(partition: &str, key: &str) -> Result<(), ApiError> {
    delete(&format!(
        "/kv/{}?key={}",
        urlencode(partition),
        urlencode(key)
    ))
    .await
}

/// Lists every queued message across all partitions.
pub async fn list_queue() -> Result<Vec<QueueMessage>, ApiError> {
    get_json("/queue").await
}

/// The body sent to re-enqueue (trigger) a queued message.
#[derive(Serialize)]
struct TriggerRequest {
    key: String,
    payload: serde_json::Value,
}

/// Re-enqueues a queued message so it becomes immediately available.
pub async fn trigger_queue(
    partition: &str,
    key: &str,
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    post_empty(
        &format!("/queue/{}/trigger", urlencode(partition)),
        &TriggerRequest {
            key: key.to_string(),
            payload,
        },
    )
    .await
}

/// Removes a queued message.
pub async fn delete_queue(partition: &str, key: &str) -> Result<(), ApiError> {
    delete(&format!(
        "/queue/{}?key={}",
        urlencode(partition),
        urlencode(key)
    ))
    .await
}

/// Percent-encodes a path/query component.
fn urlencode(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}
