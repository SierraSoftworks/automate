use std::collections::HashMap;

use actix_web::{Responder, web};
use tracing_batteries::prelude::*;

use crate::db::Queue;
use crate::prelude::Services;
use crate::webhooks::{Delivered, WebhookEvent};

/// The largest delivery we will read.
///
/// The path is anonymous by necessity — a sender has no credential beyond the
/// URL — so the body was previously read with no limit at all, which let anybody
/// who guessed nothing at all spend the agent's memory. A megabyte is far above
/// what any provider sends and far below what hurts.
const MAX_BODY: usize = 1024 * 1024;

/// `POST /webhooks/{source}` — the shared address a service posts every user's
/// deliveries to.
///
/// One endpoint for every such service. What differs between them — the signing
/// scheme, which account a delivery names, how that account maps onto stored
/// connections — is declared by each service's
/// [`crate::webhooks::WebhookSource`], and the routing they all share lives in
/// [`crate::webhooks::route`]. This is only the part that needs the HTTP stack:
/// reading a bounded body, and turning the outcome into a status code.
///
/// The existing `/webhooks/github` and `/webhooks/todoist` addresses are served
/// by this, so nothing configured with a provider has to change.
#[instrument("webhooks.source.deliver", skip(req, body, context), fields(webhook.source = %source))]
pub async fn deliver_source(
    req: actix_web::HttpRequest,
    source: web::Path<String>,
    body: web::Payload,
    context: web::Data<crate::services::AppContext>,
) -> impl Responder {
    let Some(source) = crate::webhooks::source(&source) else {
        return actix_web::HttpResponse::NotFound().finish();
    };

    let body = match body.to_bytes_limited(MAX_BODY).await {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).to_string(),
        Ok(Err(err)) => {
            error!("Failed to read a {} webhook body: {err}", source.id());
            return actix_web::HttpResponse::BadRequest().finish();
        }
        Err(_) => return actix_web::HttpResponse::PayloadTooLarge().finish(),
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

    match crate::webhooks::route(source, &context, event).await {
        // 200 rather than 204: Todoist documents anything other than a 200 as a
        // failed delivery, retries it three times, and then stops sending.
        Delivered::Accepted => actix_web::HttpResponse::Ok().finish(),
        Delivered::Unavailable => actix_web::HttpResponse::ServiceUnavailable().finish(),
        Delivered::Unauthorized => actix_web::HttpResponse::Unauthorized().finish(),
        Delivered::Malformed => actix_web::HttpResponse::BadRequest().finish(),
        Delivered::Failed => actix_web::HttpResponse::InternalServerError().finish(),
    }
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
        //
        // Not recorded. A busy sender talking to a paused workflow is the worst
        // case for the log, and the workflow already says it is paused, with an
        // entry saying when it was paused and by whom.
        debug!(workflow.id = %record.id, "Discarding a delivery for a paused workflow.");

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

    // Deliberately not recorded. A delivery that arrives is the ordinary case,
    // and on a busy installation it is thousands of rows a day burying the ones
    // worth reading. What became of it is kept against the workflow instead, as
    // one record that is overwritten rather than a history that grows.
    actix_web::HttpResponse::NoContent().finish()
}

/// Notes a delivery we would not accept, against the workflow it named.
///
/// Only refusals reach the log. A delivery that works says nothing the run
/// record does not, whereas one turned away is both rare and the thing somebody
/// is looking for when a provider insists it is sending and nothing arrives.
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
        // The delivery has already been answered; losing the note about it is
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
            config.connections.todoist.app = Some(crate::config::TodoistAppConfig {
                client_id: "todoist-client".into(),
                client_secret: "todoist-client-secret".into(),
                // Held apart from the client secret here, so the tests exercise
                // the key deliveries are actually checked against.
                webhook_secret: Some(TODOIST_SECRET.into()),
                scopes: vec!["data:read_write".into()],
                api_url: None,
                acl: None,
            });
        })
        .await
        .unwrap()
    }

    const TODOIST_SECRET: &str = "todoist-client-secret";

    macro_rules! app {
        ($context:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($context.tenant(TenantId::local())))
                    .app_data(web::Data::new($context.clone()))
                    .route("/webhooks/w/{token}", web::post().to(super::deliver))
                    .route("/webhooks/{source}", web::post().to(super::deliver_source)),
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
            StatusCode::OK,
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

    async fn todoist_workflow(context: &AppContext, tenant: TenantId, account: &str) -> Workflow {
        let services = context.tenant(tenant);
        let connection = crate::connections::ConnectionStore::for_services(&services)
            .create(
                crate::publishers::TODOIST_PROVIDER,
                format!("todoist-{account}"),
                Some(account.to_string()),
                crate::connections::ConnectionSecret::OAuth2 {
                    access_token: "access".into(),
                    refresh_token: "refresh".into(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .expect("store the Todoist connection");

        crate::workflow_store::WorkflowStore::new(&services)
            .create(WorkflowDraft {
                type_id: "todoist".into(),
                config: serde_json::json!({
                    "name": format!("Todoist {account}"),
                    "connection": connection.id,
                    "title": "Follow up",
                }),
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the Todoist workflow")
    }

    /// A second service routed through the same endpoint, which is the point of
    /// the endpoint being shared: nothing here knows what Todoist is.
    #[actix_web::test]
    async fn a_todoist_delivery_routes_only_to_the_tenant_who_owns_that_account() {
        use base64::Engine;
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let context = context().await;
        let alice = TenantId::new("alice").unwrap();
        let bob = TenantId::new("bob").unwrap();
        let alice_workflow = todoist_workflow(&context, alice.clone(), "2671355").await;
        todoist_workflow(&context, bob.clone(), "9900001").await;
        let app = app!(context);

        let body = serde_json::json!({
            "event_name": "item:completed",
            "user_id": "2671355",
            "event_data": { "content": "Buy milk" },
        })
        .to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(TODOIST_SECRET.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        let request = test::TestRequest::post()
            .uri("/webhooks/todoist")
            .insert_header(("X-Todoist-Hmac-SHA256", signature))
            .insert_header(("X-Todoist-Delivery-ID", "delivery-1"))
            .set_payload(body)
            .to_request();

        // Exactly 200: Todoist treats any other status as a failed delivery,
        // and disables the webhook after three of them.
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::OK,
        );

        let alice_jobs = context
            .tenant(alice)
            .queue()
            .peek::<_, crate::webhooks::WebhookDelivery>("webhooks/todoist", 10)
            .await
            .unwrap();
        let bob_jobs = context
            .tenant(bob)
            .queue()
            .peek::<_, crate::webhooks::WebhookDelivery>("webhooks/todoist", 10)
            .await
            .unwrap();

        assert_eq!(alice_jobs.len(), 1);
        assert_eq!(alice_jobs[0].payload.workflow, alice_workflow.id);
        assert!(
            bob_jobs.is_empty(),
            "another account's workflow must not receive the delivery"
        );
    }

    /// An address nothing is registered at is a 404 rather than an accepted
    /// delivery that goes nowhere.
    #[actix_web::test]
    async fn a_delivery_for_a_service_we_do_not_serve_is_refused() {
        let context = context().await;
        let app = app!(context);

        assert_eq!(post!(app, "/webhooks/gitlab"), StatusCode::NOT_FOUND);
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
    async fn an_accepted_delivery_writes_nothing_to_the_audit_log() {
        // A GitHub App on a busy organisation delivers thousands of times a day.
        // One row each would bury every entry somebody actually wanted to read,
        // so what became of a delivery is kept against the workflow instead.
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
            !entries
                .iter()
                .any(|entry| entry.category == crate::db::AuditCategory::WebhookDelivery),
            "an ordinary delivery is not news; only one we turned away is",
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
