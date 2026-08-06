//! Managing the workflows a user has configured.
//!
//! Everything here acts on the account the request is for, so an administrator
//! acting as somebody else edits that person's workflows rather than their own.
//!
//! # Writes end by reconciling
//!
//! Creating, editing or deleting a workflow does not arm or disarm its schedule
//! directly. It writes the record and then asks the reconciler to make the
//! schedules match, which is the same thing that happens at startup. There is
//! one implementation of "what schedules should exist", so an edit made through
//! the API and a restart cannot disagree about the answer — and a reconciliation
//! that is skipped is corrected by the next one rather than leaving a workflow
//! stranded.

use actix_web::{HttpResponse, http::StatusCode, web};
use automate_api::{WorkflowId, WorkflowTrigger, WorkflowTypeDescriptor};

use super::json_error;
use super::scope::Scoped;
use crate::db::{AuditCategory, AuditEntry, AuditOutcome, AuditStore};
use crate::prelude::*;
use crate::workflow_store::WorkflowDraft;

/// The body of a request to create a workflow.
#[derive(serde::Deserialize)]
pub struct CreateWorkflow {
    /// Which kind of workflow this is, e.g. `rss`.
    #[serde(rename = "type")]
    pub type_id: String,

    /// The values collected for that type's fields.
    pub config: serde_json::Value,

    /// When to run it. Left out, a cron workflow takes its type's default.
    #[serde(default)]
    pub schedule: Option<String>,

    /// Whether it should start running straight away.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// The body of a request to replace a workflow's configuration.
///
/// Deliberately a whole configuration rather than a set of changes: a form
/// submits every field it collected, and merging a partial update into a stored
/// configuration would make clearing a field indistinguishable from omitting it.
#[derive(serde::Deserialize)]
pub struct UpdateWorkflow {
    pub config: serde_json::Value,

    #[serde(default)]
    pub schedule: Option<String>,

    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// `GET /api/v1/workflow-types` — the kinds of workflow that can be created,
/// and the form that configures each.
///
/// Served rather than compiled into the browser, so that a workflow type added
/// to the agent is one the UI can offer without being rebuilt.
pub async fn types() -> HttpResponse {
    let descriptors: Vec<WorkflowTypeDescriptor> = crate::workflows::descriptors();
    HttpResponse::Ok().json(descriptors)
}

/// `GET /api/v1/workflows` — the workflows this account has configured.
pub async fn list(services: Scoped) -> HttpResponse {
    match services.workflows().list().await {
        Ok(workflows) => HttpResponse::Ok().json(workflows),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `GET /api/v1/workflows/{workflow}` — one workflow.
pub async fn get(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let store = services.workflows();

    match store.find(id).await {
        Ok(Some(record)) => match store.present_record(record) {
            Ok(workflow) => HttpResponse::Ok().json(workflow),
            Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
        },
        Ok(None) => not_found(id),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `POST /api/v1/workflows` — configures a new workflow.
pub async fn create(services: Scoped, body: web::Json<CreateWorkflow>) -> HttpResponse {
    let body = body.into_inner();

    let draft = WorkflowDraft {
        type_id: body.type_id,
        config: body.config,
        schedule: body.schedule,
        enabled: body.enabled,
    };

    match services.workflows().create(draft).await {
        Ok(workflow) => {
            reconcile(&services).await;
            record(
                &services,
                "created",
                workflow.id,
                format!("Created the workflow '{}'.", workflow.name),
            )
            .await;

            HttpResponse::Created().json(workflow)
        }
        // A rejected draft is the user's form to fix, not a fault: the type may
        // not exist, the configuration may not load, or the schedule may not
        // parse. All three are things they can see and correct.
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.description()),
    }
}

/// `PUT /api/v1/workflows/{workflow}` — replaces a workflow's configuration.
pub async fn update(
    services: Scoped,
    id: web::Path<String>,
    body: web::Json<UpdateWorkflow>,
) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let body = body.into_inner();
    let store = services.workflows();

    // The type is taken from the stored record rather than the request, because
    // it is not the caller's to change; `update` refuses a mismatch, so asking
    // for one is the only way to get that error.
    let existing = match store.find(id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found(id),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    };

    let draft = WorkflowDraft {
        type_id: existing.type_id,
        config: body.config,
        schedule: body.schedule,
        enabled: body.enabled,
    };

    match store.update(id, draft).await {
        Ok(workflow) => {
            reconcile(&services).await;
            record(
                &services,
                "updated",
                workflow.id,
                format!("Changed the workflow '{}'.", workflow.name),
            )
            .await;

            HttpResponse::Ok().json(workflow)
        }
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.description()),
    }
}

/// `DELETE /api/v1/workflows/{workflow}` — removes a workflow.
pub async fn delete(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let store = services.workflows();

    let name = match store.find(id).await {
        Ok(Some(record)) => store
            .present_record(record)
            .map(|workflow| workflow.name)
            .unwrap_or_else(|_| id.to_string()),
        Ok(None) => return not_found(id),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    };

    match store.delete(id).await {
        Ok(()) => {
            reconcile(&services).await;
            record(
                &services,
                "deleted",
                id,
                format!("Deleted the workflow '{name}'."),
            )
            .await;

            HttpResponse::NoContent().finish()
        }
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `POST /api/v1/workflows/{workflow}/rotate-webhook` — issues a new address.
///
/// The way a leaked URL is dealt with. The old one stops working immediately,
/// which will break whatever is still calling it — that being the point.
pub async fn rotate_webhook(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let store = services.workflows();

    match store.rotate_webhook(id).await {
        Ok(_) => {
            record(
                &services,
                "webhook-rotated",
                id,
                "Issued a new webhook address; the previous one no longer works.",
            )
            .await;

            match store.find(id).await {
                Ok(Some(record)) => match store.present_record(record) {
                    Ok(workflow) => HttpResponse::Ok().json(workflow),
                    Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
                },
                Ok(None) => not_found(id),
                Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
            }
        }
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.description()),
    }
}

/// `POST /api/v1/workflows/{workflow}/trigger` — runs a scheduled workflow now.
///
/// Dispatches exactly the message the schedule would have, so a run asked for
/// here and a run that came round on its own are the same run. Anything this
/// path did differently would be a difference nobody could see until it
/// mattered.
///
/// The schedule is deliberately left alone: running now is not a reason for the
/// next run to move, and re-arming here would make "run now" quietly skip one.
pub async fn trigger(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let store = services.workflows();

    let stored = match store.find(id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return not_found(id),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    };

    let workflow_type = match crate::workflows::lookup(&stored.type_id) {
        Ok(workflow_type) => workflow_type,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, err.description()),
    };

    // A webhook workflow runs on the delivery it was sent. There is no delivery
    // here, and inventing one would run the workflow against a payload nobody
    // sent it.
    if !matches!(
        workflow_type.descriptor().trigger,
        WorkflowTrigger::Cron { .. }
    ) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "This workflow runs when its webhook is called, so there is nothing to run on demand.",
        );
    }

    // Not refused for a paused workflow. Pausing stops the schedule; asking for
    // a run is an explicit instruction, and being able to try one is most of the
    // reason to pause a workflow you are still working on.
    //
    // Keyed by the workflow, as the schedule keys it, so asking twice while one
    // is still waiting collapses onto the run already queued instead of running
    // the same work twice.
    if let Err(err) = services
        .queue()
        .enqueue(
            workflow_type.partition(),
            stored.config,
            Some(id.to_string().into()),
            None,
        )
        .await
    {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }

    if let Err(err) = store.mark_run(id, chrono::Utc::now()).await {
        warn!(error = %err, "Failed to record that a workflow was run on demand.");
    }

    record(
        &services,
        "triggered",
        id,
        "Ran this workflow now, outside its schedule.",
    )
    .await;

    HttpResponse::NoContent().finish()
}

/// What a reset cleared, so the browser can say so rather than guess.
#[derive(serde::Serialize)]
pub struct ResetSummary {
    /// How many stored values were removed.
    pub cleared: usize,
}

/// `POST /api/v1/workflows/{workflow}/reset` — forgets what a workflow remembers
/// between runs.
///
/// This is the supported way to do what previously meant finding the right
/// entries in the Data view and deleting them by hand: the workflow type says
/// where its own state lives, so the operator does not have to know that an RSS
/// watermark is keyed by feed URL under `rss/feed`.
///
/// Deliberately does not run the workflow afterwards. Clearing a watermark and
/// running are separate decisions — the usual reason to reset is to fix
/// something before the next scheduled run, and re-filing a year of backlog as
/// a side effect of a repair is not what anybody asked for.
pub async fn reset(services: Scoped, id: web::Path<String>) -> HttpResponse {
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let store = services.workflows();

    let stored = match store.find(id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return not_found(id),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    };

    let workflow_type = match crate::workflows::lookup(&stored.type_id) {
        Ok(workflow_type) => workflow_type,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, err.description()),
    };

    let state = match workflow_type.state(&stored.config) {
        Ok(state) => state,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, err.description()),
    };

    // Refused rather than reported as a reset that cleared nothing, because the
    // two are the same response and only one of them means the workflow will
    // behave differently afterwards.
    if state.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "This workflow does not remember anything between runs, so there is nothing to reset.",
        );
    }

    let cleared = state.len();

    for entry in state {
        if let Err(err) = services.kv().remove(entry.partition, entry.key).await {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    }

    record(
        &services,
        "reset",
        id,
        format!("Cleared {cleared} stored values this workflow remembered between runs."),
    )
    .await;

    HttpResponse::Ok().json(ResetSummary { cleared })
}

/// How a file should be applied.
#[derive(serde::Deserialize)]
pub struct ImportQuery {
    /// Whether workflows absent from the file should be deleted.
    ///
    /// Off unless asked for, because the common case is a file describing part
    /// of an installation and the surprising outcome is losing the rest.
    #[serde(default)]
    pub prune: bool,
}

/// `GET /api/v1/workflows/export` — this account's workflows as TOML.
pub async fn export(services: Scoped) -> HttpResponse {
    let store = services.workflows();

    let records = match store.records().await {
        Ok(records) => records,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    };

    match crate::workflow_toml::export(&records) {
        Ok(document) => HttpResponse::Ok()
            .content_type("application/toml; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                "attachment; filename=\"workflows.toml\"",
            ))
            .body(document),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

/// `POST /api/v1/workflows/import` — applies a TOML document to this account.
pub async fn import(
    services: Scoped,
    query: web::Query<ImportQuery>,
    body: String,
) -> HttpResponse {
    let store = services.workflows();

    match crate::workflow_toml::import(&store, &body, query.prune).await {
        Ok(summary) => {
            reconcile(&services).await;

            let entry = AuditEntry::new(
                AuditCategory::WorkflowConfig,
                "imported",
                AuditOutcome::Success,
            )
            .message(format!(
                "Applied a workflow file: {} created, {} updated, {} deleted.",
                summary.created, summary.updated, summary.deleted
            ));
            if let Err(err) = services.audit().record(entry).await {
                warn!(error = %err, "Failed to record a workflow import in the audit log.");
            }

            HttpResponse::Ok().json(summary)
        }
        // A file that will not apply is the file's problem, and the message says
        // which workflow in it is at fault.
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.description()),
    }
}

/// Brings this account's schedules into line with the change just made.
///
/// A failure here is logged rather than returned: the record has already been
/// written, so reporting an error would describe a change that did happen as one
/// that did not. The next reconciliation puts the schedule right.
async fn reconcile(services: &Scoped) {
    if let Err(err) = crate::jobs::CronJob::reconcile(&**services).await {
        warn!(
            error = %err,
            "Failed to update schedules after a workflow changed; they will be corrected on the next pass.",
        );
    }
}

async fn record(services: &Scoped, action: &'static str, id: WorkflowId, message: impl ToString) {
    let entry = AuditEntry::new(AuditCategory::WorkflowConfig, action, AuditOutcome::Success)
        .subject(id)
        .message(message);

    if let Err(err) = services.audit().record(entry).await {
        // Losing the record should not fail a change that has already happened.
        warn!(error = %err, "Failed to record a workflow change in the audit log.");
    }
}

fn parse_id(raw: &str) -> Result<WorkflowId, HttpResponse> {
    raw.parse::<WorkflowId>()
        .map_err(|err| json_error(StatusCode::BAD_REQUEST, err.to_string()))
}

fn not_found(id: WorkflowId) -> HttpResponse {
    json_error(
        StatusCode::NOT_FOUND,
        format!("There is no workflow called '{id}'."),
    )
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};
    use automate_api::{TenantId, Workflow, WorkflowTypeDescriptor};

    use crate::db::{KeyValueStore, Queue};
    use crate::filter::Filter;
    use crate::prelude::Services;
    use crate::services::AppContext;
    use crate::web::api::configure;

    /// The messages waiting in a partition for the local account.
    async fn queued(
        context: &AppContext,
        partition: &'static str,
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        context
            .tenant(TenantId::local())
            .queue()
            .peek(partition, 10)
            .await
            .unwrap()
    }

    /// The schedules currently armed for the local account.
    async fn armed(context: &AppContext) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        queued(context, "cron").await
    }

    async fn context() -> AppContext {
        AppContext::new_mock(|config| {
            config.web.auth.user_acl = Some(Filter::new("true").unwrap());
            config.web.auth.admin_acl = Some(Filter::new("true").unwrap());
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
                    .service(configure()),
            )
            .await
        };
    }

    fn valid_body() -> serde_json::Value {
        serde_json::json!({
            "type": "rss",
            "config": {
                "name": "Citation Needed",
                "url": "https://example.com/rss/",
                "homepage": "https://example.com/",
            },
            "schedule": "@daily",
        })
    }

    #[actix_web::test]
    async fn the_available_workflow_types_describe_their_own_forms() {
        let app = app!(context().await);

        let req = test::TestRequest::get()
            .uri("/api/v1/workflow-types")
            .to_request();
        let types: Vec<WorkflowTypeDescriptor> = test::call_and_read_body_json(&app, req).await;

        let rss = types
            .iter()
            .find(|descriptor| descriptor.id == "rss")
            .expect("the RSS workflow type should be offered");

        assert!(
            !rss.fields.is_empty(),
            "a type with no fields would render as a form nobody could fill in",
        );
    }

    #[actix_web::test]
    async fn a_created_workflow_comes_back_in_the_list() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        let created: Workflow = test::read_body_json(resp).await;
        assert_eq!(created.name, "Citation Needed");

        let req = test::TestRequest::get()
            .uri("/api/v1/workflows")
            .to_request();
        let listed: Vec<Workflow> = test::call_and_read_body_json(&app, req).await;

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
    }

    #[actix_web::test]
    async fn creating_a_workflow_arms_its_schedule() {
        // The endpoint does not arm anything itself; it asks the reconciler to
        // make the schedules match, which is the same path startup takes.
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let armed = armed(&context).await;
        assert_eq!(armed.len(), 1);
        assert_eq!(armed[0].payload["workflow"], created.id.to_string());
    }

    #[actix_web::test]
    async fn deleting_a_workflow_disarms_its_schedule() {
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::delete()
            .uri(&format!("/api/v1/workflows/{}", created.id))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NO_CONTENT,
        );

        assert!(
            armed(&context).await.is_empty(),
            "deleting a workflow should stop it running without waiting for a restart",
        );
    }

    #[actix_web::test]
    async fn a_workflow_can_be_run_on_demand() {
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/trigger", created.id))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NO_CONTENT,
        );

        // The same message the schedule would have dispatched, in the same
        // partition, so a run asked for and a run that came round are one thing.
        let dispatched = queued(&context, "rss/todoist").await;
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].payload["url"], "https://example.com/rss/");
    }

    #[actix_web::test]
    async fn running_a_workflow_on_demand_leaves_its_schedule_where_it_was() {
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let before = armed(&context).await;

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/trigger", created.id))
            .to_request();
        test::call_service(&app, req).await;

        // Running now is not a reason for the next run to move. Re-arming here
        // would make asking for a run quietly skip the one that was coming.
        let after = armed(&context).await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].hidden_until, before[0].hidden_until);
    }

    #[actix_web::test]
    async fn a_paused_workflow_can_still_be_run_on_demand() {
        // Pausing stops the schedule. Asking for a run is an explicit
        // instruction, and being able to try one is most of the reason to pause
        // a workflow you are still working on.
        let context = context().await;
        let app = app!(context);

        let mut body = valid_body();
        body["enabled"] = serde_json::json!(false);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(body)
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/trigger", created.id))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NO_CONTENT,
        );

        assert_eq!(queued(&context, "rss/todoist").await.len(), 1);
    }

    #[actix_web::test]
    async fn a_webhook_workflow_cannot_be_run_on_demand() {
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(serde_json::json!({
                "type": "webhook",
                "config": {
                    "name": "Deployments",
                    "title": "Deployed ${{ environment }}",
                    "todoist": { "connection": null },
                },
            }))
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        // It runs on the delivery it was sent, and there is no delivery here.
        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/trigger", created.id))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    /// The state an RSS workflow keeps, as its collector addresses it.
    async fn rss_state(context: &AppContext) -> Option<serde_json::Value> {
        context
            .tenant(TenantId::local())
            .kv()
            .get("rss/feed", "https://example.com/rss/")
            .await
            .unwrap()
    }

    #[actix_web::test]
    async fn resetting_a_workflow_forgets_what_it_remembered_between_runs() {
        // The point of the endpoint: an operator should not have to know that an
        // RSS watermark is keyed by feed URL under `rss/feed` to clear one.
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;
        assert!(
            created.resettable,
            "an RSS workflow keeps a watermark, so it has something to reset",
        );

        context
            .tenant(TenantId::local())
            .kv()
            .set(
                "rss/feed",
                "https://example.com/rss/",
                serde_json::json!({ "published": "2024-01-01T00:00:00Z" }),
            )
            .await
            .unwrap();

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/reset", created.id))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let summary: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(summary["cleared"], 1);

        assert!(
            rss_state(&context).await.is_none(),
            "the watermark should be gone, or the next run picks up where it left off",
        );
    }

    #[actix_web::test]
    async fn resetting_a_workflow_does_not_run_it() {
        // Clearing a watermark and running are separate decisions. Re-filing a
        // year of backlog as a side effect of a repair is not what was asked
        // for.
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/reset", created.id))
            .to_request();
        assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);

        assert!(queued(&context, "rss/todoist").await.is_empty());
    }

    #[actix_web::test]
    async fn a_workflow_with_nothing_to_forget_cannot_be_reset() {
        // A webhook workflow acts on whatever it is handed. Reporting a reset
        // that cleared nothing would be the same response as one that changed
        // how the workflow behaves.
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(serde_json::json!({
                "type": "webhook",
                "config": {
                    "name": "Deployments",
                    "title": "Deployed ${{ environment }}",
                    "todoist": { "connection": null },
                },
            }))
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;
        assert!(!created.resettable);

        let req = test::TestRequest::post()
            .uri(&format!("/api/v1/workflows/{}/reset", created.id))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn resetting_a_workflow_that_is_not_there_says_so() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows/copper-tiger-canyon/reset")
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NOT_FOUND,
        );
    }

    #[actix_web::test]
    async fn running_a_workflow_that_is_not_there_says_so() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows/copper-tiger-canyon/trigger")
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NOT_FOUND,
        );
    }

    #[actix_web::test]
    async fn a_configuration_the_handler_could_not_read_is_the_users_to_fix() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(serde_json::json!({
                "type": "rss",
                "config": { "name": "Missing its feed" },
            }))
            .to_request();

        // A form that does not add up is the person's to correct, not a fault
        // to report as ours.
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn a_schedule_nobody_could_run_is_refused() {
        let app = app!(context().await);

        let mut body = valid_body();
        body["schedule"] = serde_json::json!("every other tuesday");

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(body)
            .to_request();

        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn an_unknown_workflow_type_is_refused_rather_than_stored() {
        let app = app!(context().await);

        let mut body = valid_body();
        body["type"] = serde_json::json!("not-a-real-type");

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(body)
            .to_request();

        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn an_identifier_that_is_not_one_is_told_apart_from_one_that_is_unused() {
        let app = app!(context().await);

        let req = test::TestRequest::get()
            .uri("/api/v1/workflows/not-real-words")
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );

        let unused = automate_api::WorkflowId::from_entropy(12345);
        let req = test::TestRequest::get()
            .uri(&format!("/api/v1/workflows/{unused}"))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::NOT_FOUND,
        );
    }

    #[actix_web::test]
    async fn workflows_can_be_taken_out_as_a_file_and_put_back() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::get()
            .uri("/api/v1/workflows/export")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let document = String::from_utf8(test::read_body(resp).await.to_vec()).unwrap();
        assert!(
            document.contains(&created.id.to_string()),
            "an exported workflow should carry its identifier, or applying the file would make a second copy: {document}",
        );

        // Applying it back changes nothing, because everything in it is already
        // there under the identifier it names.
        let req = test::TestRequest::post()
            .uri("/api/v1/workflows/import")
            .insert_header(("Content-Type", "text/plain"))
            .set_payload(document)
            .to_request();
        let summary: serde_json::Value = test::call_and_read_body_json(&app, req).await;

        assert_eq!(summary["created"], 0);
        assert_eq!(summary["updated"], 1);
        assert_eq!(summary["deleted"], 0);
    }

    #[actix_web::test]
    async fn an_imported_workflow_is_scheduled_without_a_restart() {
        let context = context().await;
        let app = app!(context);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows/import")
            .insert_header(("Content-Type", "text/plain"))
            .set_payload(
                r#"
                [[workflows.rss]]
                name = "From a file"
                url = "https://example.com/rss/"
                homepage = "https://example.com/"
                cron = "@daily"
                "#,
            )
            .to_request();
        assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);

        assert_eq!(
            armed(&context).await.len(),
            1,
            "applying a file should leave its workflows scheduled, like any other way of creating one",
        );
    }

    #[actix_web::test]
    async fn a_file_that_will_not_apply_is_the_files_problem() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows/import")
            .insert_header(("Content-Type", "text/plain"))
            .set_payload("this is not toml")
            .to_request();

        assert_eq!(
            test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[actix_web::test]
    async fn export_is_not_mistaken_for_a_workflow_called_export() {
        // The route is a sibling of /workflows/{workflow}, so it has to be
        // matched first or it would be read as an identifier and refused.
        let app = app!(context().await);

        let req = test::TestRequest::get()
            .uri("/api/v1/workflows/export")
            .to_request();

        assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn editing_a_workflow_replaces_its_configuration() {
        let app = app!(context().await);

        let req = test::TestRequest::post()
            .uri("/api/v1/workflows")
            .set_json(valid_body())
            .to_request();
        let created: Workflow = test::call_and_read_body_json(&app, req).await;

        let req = test::TestRequest::put()
            .uri(&format!("/api/v1/workflows/{}", created.id))
            .set_json(serde_json::json!({
                "config": {
                    "name": "Renamed",
                    "url": "https://example.com/rss/",
                    "homepage": "https://example.com/",
                },
                "schedule": "@hourly",
                "enabled": false,
            }))
            .to_request();
        let updated: Workflow = test::call_and_read_body_json(&app, req).await;

        assert_eq!(
            updated.id, created.id,
            "editing should not rename the thing"
        );
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.schedule.as_deref(), Some("@hourly"));
        assert!(!updated.enabled);
    }
}
