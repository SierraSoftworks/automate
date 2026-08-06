//! Thin client over the agent's `/api/v1` REST endpoints.
//!
//! Authenticated calls attach the stored ID token as an `Authorization: Bearer` header (see
//! [`crate::auth`]). When the agent rejects a token as expired (HTTP 401), the client transparently
//! renews it from the stored refresh token and retries the request once; interactive sign-in is
//! handled separately via a popup (see [`crate::auth::begin_login`]). A `401` that survives a refresh
//! is surfaced as [`ApiError::Unauthorized`] so callers can prompt for sign-in.
//!
//! # Demo mode
//!
//! This is also the one place that knows about [`crate::fixtures`]. When the URL asks for demo mode
//! every call is served from an in-memory store instead of the network, which is why no page needs
//! a demo branch of its own — and why a page cannot accidentally leave one out.

use automate_api::{
    AdminUser, Connection, ConnectionSummary, IntegrationInfo, KeyValueEntry, OptionItem,
    QueueMessage, Workflow, WorkflowTypeDescriptor,
};
use gloo_net::http::{Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::auth;
#[cfg(debug_assertions)]
use crate::fixtures;

/// Serves a call from the demo store when demo mode is active.
///
/// Written as a macro so each call site reads as one line above the real
/// request, and so the whole thing vanishes from a release build rather than
/// relying on the optimiser to notice that it cannot be reached.
macro_rules! demo {
    ($($body:tt)*) => {
        #[cfg(debug_assertions)]
        if fixtures::is_demo() {
            return { $($body)* };
        }
    };
}

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

/// The refusal the demo store returns when it is asked for something it does not
/// hold, standing in for the agent's own 404.
#[cfg(debug_assertions)]
fn not_found(what: &str) -> ApiError {
    ApiError::Server(format!("That {what} no longer exists."))
}

#[derive(serde::Deserialize)]
struct ServerError {
    error: String,
}

/// The HTTP verbs used by the client. A small enum so a request can be rebuilt for the
/// post-refresh retry.
#[derive(Clone, Copy)]
#[allow(dead_code)]
enum Verb {
    Get,
    Post,
    Put,
    Patch,
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
        Verb::Put => Request::put(url),
        Verb::Patch => Request::patch(url),
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
    demo!(Ok(Some(fixtures::admin_user())));

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
    demo!(Ok(fixtures::kv_entries()));

    get_json("/kv").await
}

/// Deletes a single key-value entry.
pub async fn delete_kv(partition: &str, key: &str) -> Result<(), ApiError> {
    demo!(fixtures::delete_kv(partition, key); Ok(()));

    delete(&format!(
        "/kv/{}?key={}",
        urlencode(partition),
        urlencode(key)
    ))
    .await
}

/// Lists every queued message across all partitions.
pub async fn list_queue() -> Result<Vec<QueueMessage>, ApiError> {
    demo!(Ok(fixtures::queue_messages()));

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
    demo!(fixtures::trigger_queue(partition, key, payload); Ok(()));

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
    demo!(fixtures::delete_queue(partition, key); Ok(()));

    delete(&format!(
        "/queue/{}?key={}",
        urlencode(partition),
        urlencode(key)
    ))
    .await
}

/// The services this account has linked.
pub async fn list_service_connections() -> Result<Vec<ConnectionSummary>, ApiError> {
    demo!(Ok(fixtures::service_connections()));

    get_json("/connections").await
}

/// Links a service using a token the user obtained from it.
pub async fn create_service_connection(
    provider: &str,
    name: &str,
    key: &str,
) -> Result<ConnectionSummary, ApiError> {
    demo!(Ok(fixtures::create_service_connection(provider, name)));

    let body = serde_json::json!({ "provider": provider, "name": name, "key": key });
    let response = send(Verb::Post, "/connections", Some(&body)).await?;

    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    response
        .json::<ConnectionSummary>()
        .await
        .map_err(|err| ApiError::Server(err.to_string()))
}

/// Updates a linked service, optionally replacing its write-only API key.
pub async fn update_service_connection(
    id: &str,
    name: &str,
    key: Option<&str>,
) -> Result<ConnectionSummary, ApiError> {
    demo!(fixtures::update_service_connection(id, name, key).ok_or(not_found("connection")));

    let body = match key.filter(|key| !key.trim().is_empty()) {
        Some(key) => serde_json::json!({ "name": name, "key": key }),
        None => serde_json::json!({ "name": name }),
    };
    let response = send(
        Verb::Patch,
        &format!("/connections/{}", urlencode(id)),
        Some(&body),
    )
    .await?;

    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    response
        .json::<ConnectionSummary>()
        .await
        .map_err(|err| ApiError::Server(err.to_string()))
}

/// Unlinks a service.
pub async fn delete_service_connection(id: &str) -> Result<(), ApiError> {
    demo!(fixtures::delete_service_connection(id); Ok(()));

    delete(&format!("/connections/{}", urlencode(id))).await
}

/// The kinds of workflow that can be created, and the form that configures each.
///
/// Fetched rather than compiled in, so a workflow type added to the agent is one
/// this can offer without being rebuilt.
pub async fn list_workflow_types() -> Result<Vec<WorkflowTypeDescriptor>, ApiError> {
    demo!(Ok(fixtures::workflow_types()));

    get_json("/workflow-types").await
}

/// The workflows this account has configured.
pub async fn list_workflows() -> Result<Vec<Workflow>, ApiError> {
    demo!(Ok(fixtures::workflows()));

    get_json("/workflows").await
}

/// Configures a new workflow.
pub async fn create_workflow(
    type_id: &str,
    config: &serde_json::Value,
    schedule: Option<&str>,
    enabled: bool,
) -> Result<Workflow, ApiError> {
    demo!(
        fixtures::create_workflow(type_id, config, schedule, enabled)
            .ok_or(not_found("workflow type"))
    );

    let body = serde_json::json!({
        "type": type_id,
        "config": config,
        "schedule": schedule,
        "enabled": enabled,
    });

    json_response(send(Verb::Post, "/workflows", Some(&body)).await?).await
}

/// Replaces a workflow's configuration.
///
/// Sends the whole configuration rather than the parts that changed, because
/// the agent cannot otherwise tell a field that was cleared from one that was
/// simply not mentioned.
pub async fn update_workflow(
    id: &str,
    config: &serde_json::Value,
    schedule: Option<&str>,
    enabled: bool,
) -> Result<Workflow, ApiError> {
    demo!(fixtures::update_workflow(id, config, schedule, enabled).ok_or(not_found("workflow")));

    let body = serde_json::json!({
        "config": config,
        "schedule": schedule,
        "enabled": enabled,
    });

    json_response(
        send(
            Verb::Put,
            &format!("/workflows/{}", urlencode(id)),
            Some(&body),
        )
        .await?,
    )
    .await
}

/// Issues a new webhook address, and stops the old one working.
pub async fn rotate_webhook(id: &str) -> Result<Workflow, ApiError> {
    demo!(fixtures::rotate_webhook(id).ok_or(not_found("workflow")));

    json_response(
        send(
            Verb::Post,
            &format!("/workflows/{}/rotate-webhook", urlencode(id)),
            None::<&()>,
        )
        .await?,
    )
    .await
}

/// Removes a workflow.
pub async fn delete_workflow(id: &str) -> Result<(), ApiError> {
    demo!(fixtures::delete_workflow(id); Ok(()));

    delete(&format!("/workflows/{}", urlencode(id))).await
}

/// Runs a scheduled workflow now, without disturbing its schedule.
pub async fn trigger_workflow(id: &str) -> Result<(), ApiError> {
    demo!(fixtures::trigger_workflow(id).ok_or(not_found("workflow")));

    let resp = send::<()>(
        Verb::Post,
        &format!("/workflows/{}/trigger", urlencode(id)),
        None,
    )
    .await?;

    if resp.ok() {
        Ok(())
    } else {
        Err(error_from_response(resp).await)
    }
}

/// Forgets what a workflow remembers between runs, reporting how many stored
/// values were cleared.
pub async fn reset_workflow(id: &str) -> Result<usize, ApiError> {
    demo!(fixtures::reset_workflow(id).ok_or(not_found("workflow")));

    let resp = send::<()>(
        Verb::Post,
        &format!("/workflows/{}/reset", urlencode(id)),
        None,
    )
    .await?;

    if !resp.ok() {
        return Err(error_from_response(resp).await);
    }

    resp.json::<ResetSummary>()
        .await
        .map(|summary| summary.cleared)
        .map_err(|e| ApiError::Network(e.to_string()))
}

/// What a reset cleared.
#[derive(serde::Deserialize)]
struct ResetSummary {
    cleared: usize,
}

/// The choices a picker should offer, fetched through a linked account.
pub async fn list_connection_options(
    connection: &str,
    source: &str,
    parent: Option<&str>,
) -> Result<Vec<OptionItem>, ApiError> {
    demo!(Ok(fixtures::connection_options(source, parent)));

    let mut url = format!(
        "/connections/{}/options/{}",
        urlencode(connection),
        urlencode(source)
    );
    if let Some(parent) = parent {
        url.push_str(&format!("?parent={}", urlencode(parent)));
    }

    get_json(&url).await
}

/// Reads a JSON body, turning a refusal into the message it carries.
async fn json_response<T: serde::de::DeserializeOwned>(
    response: gloo_net::http::Response,
) -> Result<T, ApiError> {
    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    response
        .json::<T>()
        .await
        .map_err(|err| ApiError::Server(err.to_string()))
}

/// Lists the integrations configured on the agent.
pub async fn list_integrations() -> Result<Vec<IntegrationInfo>, ApiError> {
    demo!(Ok(fixtures::integrations()));

    get_json("/integrations").await
}

/// Lists the accounts currently connected to an integration.
pub async fn list_connections(integration: &str) -> Result<Vec<Connection>, ApiError> {
    demo!(Ok(fixtures::integration_connections(integration)));

    get_json(&format!(
        "/integrations/{}/connections",
        urlencode(integration)
    ))
    .await
}

/// Severs a connection. For GitHub this uninstalls the App from the account; for
/// an OAuth2 provider it discards the stored credential.
pub async fn disconnect(integration: &str, connection: &str) -> Result<(), ApiError> {
    demo!(fixtures::disconnect(integration, connection); Ok(()));

    delete(&format!(
        "/integrations/{}/connections/{}",
        urlencode(integration),
        urlencode(connection)
    ))
    .await
}

#[derive(serde::Deserialize)]
struct StartResponse {
    authorize_url: String,
}

/// Begins connecting an integration, returning the provider authorization URL to open in a popup.
/// The agent has already set the transient state cookie the callback verifies.
pub async fn start_setup(integration: &str) -> Result<String, ApiError> {
    // There is no provider to send anybody to, and opening a popup at a URL that
    // cannot work is worse than saying so.
    demo!(Err(ApiError::Server(
        "Connecting an integration needs a running agent, so it is unavailable in demo mode."
            .to_string(),
    )));

    let resp = send(
        Verb::Post,
        &format!("/integrations/{}/setup/start", urlencode(integration)),
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
