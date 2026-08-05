use std::collections::HashMap;

use actix_web::{Responder, web};
use tracing_batteries::prelude::*;

use crate::{db::Queue, prelude::Services, webhooks::WebhookEvent};

#[instrument("webhooks.handle", skip(req, kind, body, services), fields(webhook.kind = %kind))]
pub async fn handle<S: Services>(
    req: actix_web::HttpRequest,
    kind: web::Path<String>,
    body: web::Payload,
    services: web::Data<S>,
) -> impl Responder {
    let body = match body.to_bytes_limited(MAX_BODY).await {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).to_string(),
        Ok(Err(err)) => {
            error!("Failed to read webhook body: {}", err);
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
        if let Ok(value_str) = value.to_str() {
            event.headers.insert(key.to_string(), value_str.to_string());
        }
    });

    if let Err(err) = services
        .get_ref()
        .queue()
        .enqueue(format!("webhooks/{kind}"), event, None, None)
        .await
    {
        error!("Failed to enqueue webhook payload: {}", err);
        services.session().record_human_error(&err);
        return actix_web::HttpResponse::InternalServerError().finish();
    } else {
        services
            .session()
            .record_event(format!("webhook/{kind}"), [].into());
    }

    actix_web::HttpResponse::NoContent().finish()
}

/// The largest delivery we will read.
///
/// The path is anonymous by necessity — a sender has no credential beyond the
/// URL — so the body was previously read with no limit at all, which let anybody
/// who guessed nothing at all spend the agent's memory. A megabyte is far above
/// what any provider sends and far below what hurts.
const MAX_BODY: usize = 1024 * 1024;

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
        return refuse();
    };

    let system = context.tenant(automate_api::TenantId::system());
    let index = crate::webhook_index::WebhookIndex::new(&system);

    let route = match index.lookup(&token).await {
        Ok(Some(route)) => route,
        Ok(None) => return refuse(),
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
        Ok(None) => return refuse(),
        Err(err) => {
            error!(error = %err, "Failed to load a webhook workflow: {err}");
            return actix_web::HttpResponse::InternalServerError().finish();
        }
    };

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

            return refuse();
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
fn refuse() -> actix_web::HttpResponse {
    actix_web::HttpResponse::NotFound().finish()
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};
    use automate_api::{TenantId, WebhookToken, Workflow};

    use crate::db::Queue;
    use crate::prelude::Services;
    use crate::services::AppContext;
    use crate::workflow_store::WorkflowDraft;

    async fn context() -> AppContext {
        AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(crate::filter::Filter::new("true").unwrap());
            config.web.auth.admin_acl = Some(crate::filter::Filter::new("true").unwrap());
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
