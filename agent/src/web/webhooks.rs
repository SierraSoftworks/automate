use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use actix_web::{Responder, web};
use tracing_batteries::prelude::*;

use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::db::{KeyValueStore, Queue};
use crate::job::Job;
use crate::prelude::Services;
use crate::webhooks::{GitHubWebhook, GitHubWebhookConfig, WebhookDelivery, WebhookEvent};
use crate::workflow_store::WorkflowRecord;
use crate::workflows::ConfigurableWorkflow;

/// The largest delivery we will read.
///
/// The path is anonymous by necessity — a sender has no credential beyond the
/// URL — so the body was previously read with no limit at all, which let anybody
/// who guessed nothing at all spend the agent's memory. A megabyte is far above
/// what any provider sends and far below what hurts.
const MAX_BODY: usize = 1024 * 1024;

/// `POST /webhooks/github` — the shared endpoint configured on the GitHub App.
///
/// GitHub identifies the App installation in the payload. That installation is
/// a per-tenant connection, and each enabled workflow selecting that connection
/// receives its own queued delivery.
#[instrument("webhooks.github.deliver", skip(req, body, context))]
pub async fn deliver_github(
    req: actix_web::HttpRequest,
    body: web::Payload,
    context: web::Data<crate::services::AppContext>,
) -> impl Responder {
    let config = context.config();
    let Some(app) = config.connections.github.app.as_ref() else {
        warn!("Received a GitHub App webhook, but no GitHub App is configured.");
        return actix_web::HttpResponse::ServiceUnavailable().finish();
    };

    if app.webhook_secret.is_empty() {
        warn!("Received a GitHub App webhook, but its webhook secret is not configured.");
        return actix_web::HttpResponse::ServiceUnavailable().finish();
    }

    let body = match body.to_bytes_limited(MAX_BODY).await {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).to_string(),
        Ok(Err(err)) => {
            error!("Failed to read GitHub webhook body: {err}");
            return actix_web::HttpResponse::BadRequest().finish();
        }
        Err(_) => return actix_web::HttpResponse::PayloadTooLarge().finish(),
    };

    let Some(signature) = req
        .headers()
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok())
    else {
        warn!("Received a GitHub App webhook without a signature.");
        return actix_web::HttpResponse::Unauthorized().finish();
    };

    if let Err(err) = GitHubWebhook::verify_signature(&app.webhook_secret, &body, signature) {
        warn!(error = %err, "Rejected a GitHub App webhook with an invalid signature: {err}");
        return actix_web::HttpResponse::Unauthorized().finish();
    }

    let event_type = req
        .headers()
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if event_type == "ping" {
        return actix_web::HttpResponse::NoContent().finish();
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(err) => {
            warn!(error = %err, "Rejected a GitHub App webhook whose body was not JSON: {err}");
            return actix_web::HttpResponse::BadRequest().finish();
        }
    };

    let Some(installation_id) = payload
        .get("installation")
        .and_then(|installation| installation.get("id"))
        .and_then(serde_json::Value::as_u64)
    else {
        debug!(
            event = event_type,
            "Ignoring a GitHub App event without an installation id."
        );
        return actix_web::HttpResponse::NoContent().finish();
    };

    let mut event = WebhookEvent {
        body,
        query: req.query_string().to_string(),
        headers: HashMap::new(),
    };

    req.headers().iter().for_each(|(key, value)| {
        if let Ok(value) = value.to_str() {
            event.headers.insert(key.to_string(), value.to_string());
        }
    });

    let tenants = match context.database().tenants().await {
        Ok(tenants) => tenants,
        Err(err) => {
            error!(error = %err, "Failed to enumerate tenants for a GitHub webhook: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    };

    let delivery = event
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-github-delivery"))
        .map(|(_, value)| value.clone());

    for tenant in tenants {
        if tenant == automate_api::TenantId::system() {
            continue;
        }

        let services = context.tenant(tenant);
        let connections = ConnectionStore::for_services(&services);
        let mut matching = HashSet::new();

        let stored_connections = match connections
            .list_for_provider(crate::integrations::github_app::GITHUB_PROVIDER)
            .await
        {
            Ok(connections) => connections,
            Err(err) => {
                error!(error = %err, "Failed to load GitHub connections while routing a webhook: {err}");
                return actix_web::HttpResponse::InternalServerError().finish();
            }
        };

        for connection in stored_connections {
            if matches!(
                connections.open(&connection),
                Ok(ConnectionSecret::GitHubApp { installation_id: stored }) if stored == installation_id
            ) {
                matching.insert(connection.id);
            }
        }

        if matching.is_empty() {
            continue;
        }

        let workflows: Vec<(String, WorkflowRecord)> = match services
            .kv()
            .list(<GitHubWebhook as Job>::partition())
            .await
        {
            Ok(workflows) => workflows,
            Err(err) => {
                error!(error = %err, "Failed to load GitHub workflows while routing a webhook: {err}");
                return actix_web::HttpResponse::InternalServerError().finish();
            }
        };

        for (_, workflow) in workflows {
            if !workflow.enabled
                || workflow.type_id != <GitHubWebhook as ConfigurableWorkflow>::type_id()
            {
                continue;
            }

            let Ok(workflow_config) =
                serde_json::from_value::<GitHubWebhookConfig>(workflow.config.clone())
            else {
                warn!(workflow.id = %workflow.id, "Skipping a GitHub workflow with invalid configuration.");
                continue;
            };

            if !workflow_config
                .connection
                .is_some_and(|id| matching.contains(&id))
            {
                continue;
            }

            let idempotency_key = delivery
                .as_ref()
                .map(|delivery| Cow::Owned(format!("{delivery}/{}", workflow.id)));

            if let Err(err) = services
                .queue()
                .enqueue(
                    <GitHubWebhook as Job>::partition(),
                    WebhookDelivery {
                        workflow: workflow.id,
                        event: event.clone(),
                    },
                    idempotency_key,
                    None,
                )
                .await
            {
                error!(error = %err, workflow.id = %workflow.id, "Failed to enqueue a GitHub webhook delivery: {err}");
                return actix_web::HttpResponse::InternalServerError().finish();
            }

            audit(
                &services,
                workflow.id,
                crate::db::AuditOutcome::Success,
                "Accepted a delivery from the GitHub App.".to_string(),
            )
            .await;
        }
    }

    actix_web::HttpResponse::NoContent().finish()
}

/// `POST /webhooks/w/{token}` — a delivery for one person's workflow.
///
/// The tenant comes from the token rather than from configuration, which is the
/// whole difference from the older path: that one answers for the installation,
/// this one answers for whoever owns the workflow the URL names.
#[instrument("webhooks.deliver", skip(req, token, body, context))]
pub async fn deliver(
    req: actix_web::HttpRequest,
    token: web::Path<String>,
    body: web::Payload,
    context: web::Data<crate::services::AppContext>,
) -> impl Responder {
    // Parsed before anything is read, so a body is never taken from a caller who
    // has not presented a well-formed token.
    let Ok(token) = token.parse::<automate_api::WebhookToken>() else {
        return refuse("the address is not one we could have issued", &context);
    };

    let system = context.tenant(automate_api::TenantId::system());
    let index = crate::webhook_index::WebhookIndex::new(&system);

    let route = match index.lookup(&token).await {
        Ok(Some(route)) => route,
        Ok(None) => return refuse("the address is not one we have issued", &context),
        Err(err) => {
            error!(error = %err, "Failed to look up a webhook token: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    };

    let services = context.tenant(route.tenant.clone());
    let store = crate::workflow_store::WorkflowStore::new(&services);

    // The index said where to go; the record is what decides whether to. A
    // stale or tampered index cannot conjure a delivery into a workflow whose
    // own token does not match.
    let record = match store.find(route.workflow).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return refuse("the workflow this address belonged to is gone", &context);
        }
        Err(err) => {
            error!(error = %err, "Failed to load a webhook workflow: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    };

    let directly_addressed = crate::workflows::lookup(&record.type_id)
        .map(|workflow| {
            matches!(
                workflow.descriptor().trigger,
                automate_api::WorkflowTrigger::Webhook { .. }
            )
        })
        .unwrap_or(false);

    if !directly_addressed {
        return refuse(
            "the workflow no longer accepts deliveries at this address",
            &context,
        );
    }

    match store.webhook_token(&record) {
        Ok(Some(expected)) if expected == token => {}
        Ok(_) => {
            // Only reachable if the index pointed somewhere the record does not
            // agree with, which should not happen — so it is worth a record that
            // its owner can see rather than only a log line.
            audit(
                &services,
                record.id,
                crate::db::AuditOutcome::Denied,
                "Refused a delivery whose address did not match this workflow.".to_string(),
            )
            .await;

            return refuse("the address does not match the workflow it names", &context);
        }
        Err(err) => {
            error!(error = %err, "Failed to read a workflow's webhook token: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    }

    if !record.enabled {
        // Accepted and dropped rather than refused: the URL is real and the
        // sender did nothing wrong, and telling them otherwise would have them
        // retry or raise an alert over a workflow its owner deliberately paused.
        debug!(workflow.id = %record.id, "Discarding a delivery for a paused workflow.");

        // Recorded, because from the outside this looks exactly like a delivery
        // that worked. Somebody wondering why nothing happened should be able to
        // find out that it was paused rather than lost.
        audit(
            &services,
            record.id,
            crate::db::AuditOutcome::Success,
            "Discarded a delivery because this workflow is paused.".to_string(),
        )
        .await;

        return actix_web::HttpResponse::NoContent().finish();
    }

    let body = match body.to_bytes_limited(MAX_BODY).await {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).to_string(),
        Ok(Err(err)) => {
            error!("Failed to read webhook body: {}", err);
            return actix_web::HttpResponse::BadRequest().finish();
        }
        Err(_) => {
            return actix_web::HttpResponse::PayloadTooLarge().finish();
        }
    };

    let mut event = WebhookEvent {
        body,
        query: req.query_string().to_string(),
        headers: HashMap::new(),
    };

    req.headers().iter().for_each(|(key, value)| {
        if let Ok(value) = value.to_str() {
            event.headers.insert(key.to_string(), value.to_string());
        }
    });

    let partition = match crate::workflows::lookup(&record.type_id) {
        Ok(workflow) => workflow.partition().to_string(),
        Err(err) => {
            error!(error = %err, "A stored workflow names a type we no longer have: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    };

    if let Err(err) = services
        .queue()
        .enqueue(
            partition,
            serde_json::json!({ "workflow": record.id, "event": event }),
            None,
            None,
        )
        .await
    {
        error!(error = %err, "Failed to enqueue a webhook delivery: {err}");
        services.session().record_human_error(&err);
        return actix_web::HttpResponse::InternalServerError().finish();
    }

    audit(
        &services,
        record.id,
        crate::db::AuditOutcome::Success,
        format!("Accepted a delivery for '{}'.", record.type_id),
    )
    .await;

    actix_web::HttpResponse::NoContent().finish()
}

/// Notes what became of a delivery, for the workflows we could identify.
///
/// Deliberately not called for a token nobody was issued. We do not know whose
/// account such a delivery would belong to, and the only place to put it would
/// be one shared by everybody — which is an unauthenticated endpoint writing
/// unbounded rows into a table the whole installation reads. A line in the log
/// is the right weight for a request we have no reason to believe is real.
async fn audit(
    services: &crate::services::AppServices,
    workflow: automate_api::WorkflowId,
    outcome: crate::db::AuditOutcome,
    message: String,
) {
    use crate::db::{AuditCategory, AuditEntry, AuditStore};

    let entry = AuditEntry::new(AuditCategory::WebhookDelivery, "received", outcome)
        .subject(workflow)
        .message(message);

    if let Err(err) = services.audit().record(entry).await {
        // The delivery has already been accepted; losing the note about it is
        // not a reason to make the sender retry.
        warn!(error = %err, "Failed to record a webhook delivery in the audit log.");
    }
}

/// The one answer given to every delivery we will not accept.
///
/// A token nobody was issued, one that has been rotated away, a workflow since
/// deleted, and a token that does not match the workflow it points at all look
/// identical from outside. Telling them apart would let somebody sort guesses
/// into "wrong" and "used to be right", and the second pile is a map.
///
/// The refusal is recorded in the telemetry stream rather than the audit log.
/// The audit log belongs to an account and we do not know whose this would be;
/// writing it to a shared one would let an anonymous endpoint fill a table the
/// whole installation reads. Telemetry has neither problem, and it is where
/// somebody debugging "the provider says it is sending and nothing arrives"
/// will actually look — which is the case this exists to make visible.
fn refuse(reason: &'static str, services: &crate::services::AppContext) -> actix_web::HttpResponse {
    warn!(
        webhook.refused = reason,
        "Refused a webhook delivery: {reason}.",
    );

    services.session().record_event(
        "webhook/refused",
        [("reason".to_string(), reason.to_string())].into(),
    );

    refused_response()
}

fn refused_response() -> actix_web::HttpResponse {
    actix_web::HttpResponse::NotFound().finish()
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};
    use automate_api::{TenantId, WebhookToken, Workflow};

    use crate::db::{KeyValueStore, Queue};
    use crate::prelude::Services;
    use crate::services::AppContext;
    use crate::workflow_store::WorkflowDraft;

    async fn context() -> AppContext {
        AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(crate::filter::Filter::new("true").unwrap());
            config.web.auth.admin_acl = Some(crate::filter::Filter::new("true").unwrap());
            config.connections.github.app =
                Some(crate::testing::github_app("https://api.github.com"));
        })
        .await
        .unwrap()
    }

    macro_rules! app {
        ($context:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($context.tenant(TenantId::local())))
                    .app_data(web::Data::new($context.clone()))
                    .route("/webhooks/github", web::post().to(super::deliver_github))
                    .route("/webhooks/w/{token}", web::post().to(super::deliver)),
            )
            .await
        };
    }

    /// Creates a webhook workflow for the local account and returns it.
    async fn workflow(context: &AppContext) -> Workflow {
        let services = context.tenant(TenantId::local());

        let system = context.tenant(TenantId::system());

        crate::workflow_store::WorkflowStore::new(&services)
            .with_index(&system)
            .create(WorkflowDraft {
                type_id: "webhook".into(),
                config: serde_json::json!({
                    "name": "Deployments",
                    "title": "Deployed ${{ environment }}",
                    "todoist": { "connection": null },
                }),
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
    }

    async fn github_workflow(
        context: &AppContext,
        tenant: TenantId,
        installation_id: u64,
    ) -> Workflow {
        let services = context.tenant(tenant);
        let connection = crate::connections::ConnectionStore::for_services(&services)
            .create(
                crate::integrations::github_app::GITHUB_PROVIDER,
                format!("installation-{installation_id}"),
                None,
                crate::connections::ConnectionSecret::GitHubApp { installation_id },
            )
            .await
            .expect("store the GitHub connection");

        crate::workflow_store::WorkflowStore::new(&services)
            .create(WorkflowDraft {
                type_id: "github".into(),
                config: serde_json::json!({
                    "name": format!("GitHub {installation_id}"),
                    "connection": connection.id,
                }),
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the GitHub workflow")
    }

    /// Posts a JSON body to a webhook address and returns the status.
    macro_rules! post {
        ($app:expr, $path:expr) => {{
            let req = test::TestRequest::post()
                .uri($path)
                .insert_header(("Content-Type", "application/json"))
                .set_payload(r#"{"environment":"production"}"#)
                .to_request();

            test::call_service(&$app, req).await.status()
        }};
    }

    #[actix_web::test]
    async fn a_delivery_to_a_workflows_own_address_is_accepted() {
        let context = context().await;
        let workflow = workflow(&context).await;
        let app = app!(context);

        let path = workflow
            .webhook_path
            .as_deref()
            .expect("a webhook workflow should have an address");

        assert_eq!(post!(app, path), StatusCode::NO_CONTENT);

        let queued: Vec<crate::db::PeekedMessage<serde_json::Value>> = context
            .tenant(TenantId::local())
            .queue()
            .peek("webhooks/generic", 10)
            .await
            .unwrap();

        assert_eq!(queued.len(), 1);
        assert_eq!(
            queued[0].payload["workflow"],
            workflow.id.to_string(),
            "a delivery should name the workflow it was addressed to",
        );
    }

    #[actix_web::test]
    async fn a_github_app_delivery_routes_only_to_the_tenant_with_that_installation() {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let context = context().await;
        let alice = TenantId::new("alice").unwrap();
        let bob = TenantId::new("bob").unwrap();
        let alice_workflow = github_workflow(&context, alice.clone(), 42).await;
        let alice_second_workflow = github_workflow(&context, alice.clone(), 42).await;
        github_workflow(&context, bob.clone(), 99).await;
        assert!(alice_workflow.webhook_path.is_none());
        assert!(alice_second_workflow.webhook_path.is_none());
        let app = app!(context);

        let body = serde_json::json!({
            "installation": { "id": 42 },
            "repository": { "full_name": "example/repo" },
        })
        .to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"github-webhook-secret").unwrap();
        mac.update(body.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        let request = test::TestRequest::post()
            .uri("/webhooks/github")
            .insert_header(("X-GitHub-Event", "issues"))
            .insert_header(("X-GitHub-Delivery", "delivery-1"))
            .insert_header(("X-Hub-Signature-256", signature))
            .set_payload(body)
            .to_request();

        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::NO_CONTENT,
        );

        let alice_jobs = context
            .tenant(alice)
            .queue()
            .peek::<_, crate::webhooks::WebhookDelivery>("webhooks/github", 10)
            .await
            .unwrap();
        let bob_jobs = context
            .tenant(bob)
            .queue()
            .peek::<_, crate::webhooks::WebhookDelivery>("webhooks/github", 10)
            .await
            .unwrap();

        assert_eq!(alice_jobs.len(), 2);
        assert!(
            alice_jobs
                .iter()
                .any(|job| job.payload.workflow == alice_workflow.id)
        );
        assert!(
            alice_jobs
                .iter()
                .any(|job| job.payload.workflow == alice_second_workflow.id)
        );
        assert!(
            bob_jobs.is_empty(),
            "another tenant's workflow must not receive the delivery"
        );
    }

    #[actix_web::test]
    async fn a_github_app_delivery_with_an_invalid_signature_is_rejected() {
        let context = context().await;
        let app = app!(context);
        let request = test::TestRequest::post()
            .uri("/webhooks/github")
            .insert_header(("X-GitHub-Event", "issues"))
            .insert_header(("X-Hub-Signature-256", "sha256=0000"))
            .set_payload(r#"{"installation":{"id":42}}"#)
            .to_request();

        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn a_legacy_github_workflow_address_is_refused() {
        let context = context().await;
        let tenant = TenantId::local();
        let workflow = github_workflow(&context, tenant.clone(), 42).await;
        let services = context.tenant(tenant.clone());
        let system = context.tenant(TenantId::system());
        let store = crate::workflow_store::WorkflowStore::new(&services);
        let mut record = store.get(workflow.id).await.unwrap();
        let token = crate::webhook_index::mint();

        record.webhook = Some(
            crate::webhook_index::seal(services.secrets(), &token, &tenant, workflow.id).unwrap(),
        );
        services
            .kv()
            .set("webhooks/github", workflow.id.to_string(), record)
            .await
            .unwrap();
        crate::webhook_index::WebhookIndex::new(&system)
            .insert(
                &token,
                crate::webhook_index::WebhookRoute {
                    tenant,
                    workflow: workflow.id,
                },
            )
            .await
            .unwrap();

        let app = app!(context);
        assert_eq!(
            post!(app, &format!("/webhooks/w/{token}")),
            StatusCode::NOT_FOUND,
        );

        let queued = services
            .queue()
            .peek::<_, crate::webhooks::WebhookDelivery>("webhooks/github", 10)
            .await
            .unwrap();
        assert!(queued.is_empty());
    }

    #[actix_web::test]
    async fn a_token_nobody_was_issued_is_refused() {
        let context = context().await;
        workflow(&context).await;
        let app = app!(context);

        let guess = WebhookToken::from_bytes([0x11; 16]);

        assert_eq!(
            post!(app, &format!("/webhooks/w/{guess}")),
            StatusCode::NOT_FOUND,
        );
    }

    #[actix_web::test]
    async fn a_token_that_is_not_even_a_token_is_refused_the_same_way() {
        // Distinguishing "malformed" from "unknown" would tell somebody probing
        // the endpoint when they had at least got the shape right.
        let context = context().await;
        let app = app!(context);

        for path in [
            "/webhooks/w/not-a-token",
            "/webhooks/w/AAAA",
            "/webhooks/w/../../etc/passwd",
        ] {
            assert_eq!(post!(app, path), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[actix_web::test]
    async fn a_rotated_address_stops_working_and_the_new_one_starts() {
        let context = context().await;
        let workflow = workflow(&context).await;
        let old = workflow.webhook_path.clone().unwrap();

        let services = context.tenant(TenantId::local());
        let system = context.tenant(TenantId::system());
        let store = crate::workflow_store::WorkflowStore::new(&services).with_index(&system);
        let replacement = store.rotate_webhook(workflow.id).await.unwrap();

        let app = app!(context);

        assert_eq!(
            post!(app, &old),
            StatusCode::NOT_FOUND,
            "a leaked address must stop working the moment it is rotated",
        );
        assert_eq!(
            post!(app, &format!("/webhooks/w/{replacement}")),
            StatusCode::NO_CONTENT,
        );
    }

    #[actix_web::test]
    async fn the_address_of_a_deleted_workflow_stops_working() {
        let context = context().await;
        let workflow = workflow(&context).await;
        let path = workflow.webhook_path.clone().unwrap();

        let services = context.tenant(TenantId::local());
        let system = context.tenant(TenantId::system());
        crate::workflow_store::WorkflowStore::new(&services)
            .with_index(&system)
            .delete(workflow.id)
            .await
            .unwrap();

        let app = app!(context);
        assert_eq!(post!(app, &path), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_delivery_for_a_paused_workflow_is_accepted_and_dropped() {
        // The sender did nothing wrong and the address is real; refusing would
        // have them retry or alert over something its owner chose to pause.
        let context = context().await;
        let workflow = workflow(&context).await;
        let path = workflow.webhook_path.clone().unwrap();

        let services = context.tenant(TenantId::local());
        let system = context.tenant(TenantId::system());
        let store = crate::workflow_store::WorkflowStore::new(&services).with_index(&system);

        store
            .update(
                workflow.id,
                WorkflowDraft {
                    type_id: "webhook".into(),
                    config: workflow.config.clone(),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        let app = app!(context);
        assert_eq!(post!(app, &path), StatusCode::NO_CONTENT);

        let queued: Vec<crate::db::PeekedMessage<serde_json::Value>> =
            services.queue().peek("webhooks/generic", 10).await.unwrap();
        assert!(queued.is_empty(), "a paused workflow should not be run");
    }

    #[actix_web::test]
    async fn an_accepted_delivery_leaves_a_record_its_owner_can_find() {
        use crate::db::AuditStore;

        let context = context().await;
        let workflow = workflow(&context).await;
        let app = app!(context);

        assert_eq!(
            post!(app, workflow.webhook_path.as_deref().unwrap()),
            StatusCode::NO_CONTENT,
        );

        let entries = context
            .tenant(TenantId::local())
            .audit()
            .audit(crate::db::AuditQuery::recent(50))
            .await
            .unwrap();

        assert!(
            entries
                .iter()
                .any(|entry| entry.category == crate::db::AuditCategory::WebhookDelivery),
            "a delivery that was accepted should be visible to the account it ran for",
        );
    }

    #[actix_web::test]
    async fn a_token_nobody_was_issued_writes_nothing_to_the_audit_log() {
        // The endpoint is anonymous, so recording every rejected guess would let
        // anybody at all fill a table the whole installation reads.
        use crate::db::AuditStore;

        let context = context().await;
        workflow(&context).await;
        let app = app!(context);

        let guess = WebhookToken::from_bytes([0x42; 16]);
        assert_eq!(
            post!(app, &format!("/webhooks/w/{guess}")),
            StatusCode::NOT_FOUND,
        );

        for tenant in [TenantId::local(), TenantId::system()] {
            let entries = context
                .tenant(tenant.clone())
                .audit()
                .audit(crate::db::AuditQuery::recent(50))
                .await
                .unwrap();

            assert!(
                !entries
                    .iter()
                    .any(|entry| entry.category == crate::db::AuditCategory::WebhookDelivery),
                "an unrecognised token should not be able to write to {tenant}'s audit log",
            );
        }
    }

    #[actix_web::test]
    async fn a_body_larger_than_we_will_read_is_refused() {
        // The path is anonymous by necessity, so an unbounded read is memory
        // anybody at all can spend.
        let context = context().await;
        let workflow = workflow(&context).await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri(workflow.webhook_path.as_deref().unwrap())
            .set_payload("x".repeat(super::MAX_BODY + 1))
            .to_request();

        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
        );
    }
}
