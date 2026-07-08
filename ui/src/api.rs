//! Thin client over the agent's `/api/v1` REST endpoints.
//!
//! Authenticated calls attach the stored ID token as an `Authorization: Bearer` header (see
//! [`crate::auth`]). When the agent rejects a token as expired (HTTP 401), the client transparently
//! renews it from the stored refresh token and retries the request once; interactive sign-in is
//! handled separately via a popup (see [`crate::auth::begin_login`]). A `401` that survives a refresh
//! is surfaced as [`ApiError::Unauthorized`] so callers can prompt for sign-in.

use automate_api::{AdminUser, KeyValueEntry, QueueMessage};
use gloo_net::http::{Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::auth;

/// The base path of the REST API. Requests are made relative to the current origin so the same
/// bundle works behind any host.
const API_BASE: &str = "/api/v1";

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

/// The HTTP verbs used by the client. A small enum so a request can be rebuilt for the
/// post-refresh retry.
#[derive(Clone, Copy)]
enum Verb {
    Get,
    Post,
    Delete,
}

/// Builds a request with the bearer token (when present) and an optional JSON body.
fn build<B: Serialize>(
    verb: Verb,
    url: &str,
    token: Option<&str>,
    body: Option<&B>,
) -> Result<Request, ApiError> {
    let builder = match verb {
        Verb::Get => Request::get(url),
        Verb::Post => Request::post(url),
        Verb::Delete => Request::delete(url),
    };
    let builder = match token {
        Some(token) => builder.header("Authorization", &format!("Bearer {token}")),
        None => builder,
    };
    match body {
        Some(body) => builder
            .json(body)
            .map_err(|e| ApiError::Network(e.to_string())),
        None => builder
            .build()
            .map_err(|e| ApiError::Network(e.to_string())),
    }
}

/// Sends a request, attaching the stored bearer token. On a `401` (when a session is configured) the
/// token is transparently renewed from the refresh token and the request retried once; if renewal
/// fails the stored session is dropped and the `401` is returned.
async fn send<B: Serialize>(
    verb: Verb,
    path: &str,
    body: Option<&B>,
) -> Result<Response, ApiError> {
    let url = format!("{API_BASE}{path}");
    let token = auth::stored_token();
    let response = build(verb, &url, token.as_deref(), body)?
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;

    if response.status() != 401 {
        return Ok(response);
    }

    if let Ok(fresh) = auth::refresh_session().await {
        return build(verb, &url, Some(&fresh), body)?
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()));
    }

    auth::clear_token();
    Ok(response)
}

/// Converts a non-success response into an [`ApiError`], reading the JSON error body when available.
async fn error_from_response(resp: Response) -> ApiError {
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

/// Performs a GET request and deserializes the JSON response body.
async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    let resp = send::<()>(Verb::Get, path, None).await?;
    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))
}

/// Performs a POST request with a JSON body, expecting an empty success response.
async fn post_empty<B: Serialize>(path: &str, body: &B) -> Result<(), ApiError> {
    let resp = send(Verb::Post, path, Some(body)).await?;
    if resp.ok() {
        Ok(())
    } else {
        Err(error_from_response(resp).await)
    }
}

/// Performs a DELETE request, expecting an empty success response.
async fn delete(path: &str) -> Result<(), ApiError> {
    let resp = send::<()>(Verb::Delete, path, None).await?;
    if resp.ok() {
        Ok(())
    } else {
        Err(error_from_response(resp).await)
    }
}

/// Fetches the signed-in user's identity, if any.
pub async fn me() -> Result<Option<AdminUser>, ApiError> {
    let resp = send::<()>(Verb::Get, "/me", None).await?;
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

/// A configured integration provider the admin may connect.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct OAuthProvider {
    pub provider: String,
    pub name: String,
}

/// Lists the configured integration providers.
pub async fn list_oauth_providers() -> Result<Vec<OAuthProvider>, ApiError> {
    get_json("/oauth").await
}

#[derive(serde::Deserialize)]
struct StartResponse {
    authorize_url: String,
}

/// Begins connecting an integration provider, returning the provider authorization URL to open in a
/// popup. The agent has already set the transient state cookie the callback verifies.
pub async fn start_oauth(provider: &str) -> Result<String, ApiError> {
    let resp = send(
        Verb::Post,
        &format!("/oauth/{}/start", urlencode(provider)),
        Some(&serde_json::Value::Null),
    )
    .await?;
    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }
    resp.json::<StartResponse>()
        .await
        .map(|r| r.authorize_url)
        .map_err(|e| ApiError::Network(e.to_string()))
}

/// Percent-encodes a path/query component.
fn urlencode(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}
