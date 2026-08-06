//! Reading back what happened.
//!
//! The log the agent writes as it works is only useful if the person whose
//! workflows it concerns can read it. This is that view, scoped to their own
//! account: a workflow that has been quietly failing every night, a webhook
//! whose signature never matched, or a connection that was replaced.
//!
//! The administrative endpoint alongside it (`/admin/audit`) reads the same
//! table across every account. Both exist because they answer different
//! questions, and giving a user the wider one would show them everybody else's.

use actix_web::{HttpResponse, http::StatusCode, web};

use super::json_error;
use super::scope::Scoped;
use crate::db::{AuditQuery, AuditStore};
use crate::prelude::*;

/// How many entries a single request will return.
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

/// Query parameters for reading the log.
#[derive(serde::Deserialize)]
pub struct AuditParams {
    /// Return entries older than this id, for paging backwards through history.
    #[serde(default)]
    pub before: Option<i64>,

    /// Restrict to entries about one workflow or connection.
    #[serde(default)]
    pub subject: Option<String>,

    /// Restrict to one area of the system.
    #[serde(default)]
    pub category: Option<String>,

    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /api/v1/audit` — this account's own history, most recent first.
pub async fn list(services: Scoped, params: web::Query<AuditParams>) -> HttpResponse {
    let params = params.into_inner();

    let category = match params.category.as_deref() {
        Some(value) => match automate_api::AuditCategory::parse(value) {
            Some(category) => Some(category),
            None => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("'{value}' is not an area of the system we record."),
                );
            }
        },
        None => None,
    };

    let query = AuditQuery {
        // Clamped so that a caller cannot ask for the whole history in one
        // response and hold the database open while it is serialised.
        limit: params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT),
        subject: params.subject,
        before: params.before,
        category,
    };

    match services.audit().audit(query).await {
        Ok(entries) => HttpResponse::Ok().json(entries),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.description()),
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};
    use automate_api::{AuditCategory, AuditOutcome, AuditRecord, TenantId};

    use crate::db::AuditEntry;
    use crate::filter::Filter;
    use crate::prelude::Services;
    use crate::services::AppContext;
    use crate::web::api::configure;

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

    /// Writes an entry for one account, so a test can tell whose it was.
    async fn record(context: &AppContext, tenant: TenantId, subject: &str, outcome: AuditOutcome) {
        use crate::db::AuditStore;

        context
            .tenant(tenant)
            .audit()
            .record(AuditEntry::new(AuditCategory::WorkflowRun, "ran", outcome).subject(subject))
            .await
            .unwrap();
    }

    #[actix_web::test]
    async fn the_log_is_returned_most_recent_first() {
        let context = context().await;
        record(&context, TenantId::local(), "older", AuditOutcome::Success).await;
        record(&context, TenantId::local(), "newer", AuditOutcome::Failure).await;

        let app = app!(context);
        let req = test::TestRequest::get().uri("/api/v1/audit").to_request();
        let entries: Vec<AuditRecord> = test::call_and_read_body_json(&app, req).await;

        let subjects: Vec<_> = entries
            .iter()
            .map(|entry| entry.subject.as_deref().unwrap())
            .collect();
        assert_eq!(subjects, vec!["newer", "older"]);
    }

    #[actix_web::test]
    async fn one_account_cannot_read_anothers_history() {
        // The point of the endpoint being scoped: the administrative view reads
        // the same table across every account, and this one must not.
        let context = context().await;
        record(
            &context,
            TenantId::new("somebody-else").unwrap(),
            "theirs",
            AuditOutcome::Success,
        )
        .await;
        record(&context, TenantId::local(), "mine", AuditOutcome::Success).await;

        let app = app!(context);
        let req = test::TestRequest::get().uri("/api/v1/audit").to_request();
        let entries: Vec<AuditRecord> = test::call_and_read_body_json(&app, req).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].subject.as_deref(), Some("mine"));
    }

    #[actix_web::test]
    async fn the_log_can_be_narrowed_to_one_subject() {
        let context = context().await;
        record(&context, TenantId::local(), "wanted", AuditOutcome::Success).await;
        record(&context, TenantId::local(), "other", AuditOutcome::Success).await;

        let app = app!(context);
        let req = test::TestRequest::get()
            .uri("/api/v1/audit?subject=wanted")
            .to_request();
        let entries: Vec<AuditRecord> = test::call_and_read_body_json(&app, req).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].subject.as_deref(), Some("wanted"));
    }

    #[actix_web::test]
    async fn an_area_we_do_not_record_is_refused_rather_than_ignored() {
        // Silently returning everything would have a caller believe its filter
        // was applied, which is worse than being told it was not.
        let app = app!(context().await);

        let req = test::TestRequest::get()
            .uri("/api/v1/audit?category=nonsense")
            .to_request();
        let response = test::call_service(&app, req).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
