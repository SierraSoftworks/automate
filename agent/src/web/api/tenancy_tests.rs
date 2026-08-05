//! Proof that two people signed in to the same installation cannot see or
//! affect one another's records.
//!
//! The storage layer already refuses to reach across accounts — a `TenantDb`
//! handle has no method that names another one — and there are database-level
//! tests for that. What these cover is the seam above it: that a request
//! authenticated as one person cannot reach another person's workflows,
//! connections, key-value records or queue *through the HTTP API*. That seam is
//! where the mistakes actually happen; a recent bug let a request name whose
//! account a GitHub installation landed in, which the storage tests could never
//! have caught because the storage layer did exactly what it was told.
//!
//! # How two people are reached
//!
//! [`AppContext::new_mock`] configures no identity provider, so every request
//! resolves to the installation's local account and there is no second
//! credential to present. The reachable way to drive two accounts through the
//! real middleware is the one the impersonation tests already use: enable
//! `multi_tenant`, and have an administrator act as alice for one request and as
//! bob for the next. Nothing downstream of the middleware can tell the
//! difference, because [`Scoped`](super::scope::Scoped) is built from
//! `Principal::effective` alone and never learns how that account was decided.
//!
//! # What is deliberately not here
//!
//! *A non-administrator cannot act as anybody.* Already covered by
//! `super::tests::impersonation_is_refused_for_an_account_that_is_not_an_administrator`,
//! and repeating it here would just be a second copy of the same assertion.
//!
//! *Webhook ingress.* Delivering to somebody else's address, a rotated address,
//! and a deleted one are covered in `crate::web::webhooks`. What is tested here
//! is the half that endpoint depends on: that the address itself never appears
//! in anybody else's view.

use actix_web::http::StatusCode;
use actix_web::{App, test, web};

use automate_api::{ConnectionId, TenantId, Workflow, WorkflowId};

use crate::filter::Filter;
use crate::services::AppContext;
use crate::users::UserRegistry;

use super::{IMPERSONATE_HEADER, configure};

/// Two ordinary people, neither of whom administers anything.
const ALICE: &str = "alice";
const BOB: &str = "bob";

fn account(name: &str) -> TenantId {
    TenantId::new(name).unwrap()
}

/// An installation where alice and bob have both signed in.
///
/// The ACLs admit everybody and grant administration, because the request
/// actually being made is the administrator's — acting as one of them is how a
/// second account is reached at all. What each test asserts is about the two
/// accounts, never about who is permitted to act as them.
async fn two_people() -> AppContext {
    let context = AppContext::new_mock(|config| {
        config.web.auth.user_acl = Some(Filter::new("true").unwrap());
        config.web.auth.admin_acl = Some(Filter::new("true").unwrap());
        // Nothing is partitioned until an operator asks for it, so without this
        // there is only one account and nothing to keep apart.
        config.web.auth.multi_tenant = true;
    })
    .await
    .unwrap();

    // Impersonation refuses an account it has never seen, so both have to have
    // signed in before either can be acted as.
    let registry = UserRegistry::new(context.tenant(TenantId::system()));
    for name in [ALICE, BOB] {
        registry
            .record_sign_in(&account(name), name, None, false)
            .await
            .unwrap();
    }

    context
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

/// Sends a request while acting as `who`, through the whole middleware.
macro_rules! acting_as {
    ($app:expr, $who:expr, $request:expr) => {
        test::call_service(
            &$app,
            $request
                .insert_header((IMPERSONATE_HEADER, $who))
                .to_request(),
        )
        .await
    };
}

/// As [`acting_as!`], deserialising the response body.
macro_rules! read_acting_as {
    ($app:expr, $who:expr, $request:expr) => {
        test::read_body_json(acting_as!($app, $who, $request)).await
    };
}

/// As [`acting_as!`], returning the response body as text.
macro_rules! text_acting_as {
    ($app:expr, $who:expr, $request:expr) => {
        String::from_utf8_lossy(&test::read_body(acting_as!($app, $who, $request)).await)
            .to_string()
    };
}

/// A workflow configuration the RSS type accepts.
fn feed(name: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "rss",
        "config": {
            "name": name,
            "url": "https://example.com/rss/",
            "homepage": "https://example.com/",
        },
        "schedule": "@daily",
    })
}

/// Creates a workflow as `who`, failing loudly if the fixture did not take.
///
/// A test asserting that something is *absent* from somebody else's view is
/// worthless if the thing was never created, so this is checked rather than
/// assumed.
macro_rules! given_workflow {
    ($app:expr, $who:expr, $body:expr) => {{
        let response = acting_as!(
            $app,
            $who,
            test::TestRequest::post()
                .uri("/api/v1/workflows")
                .set_json($body)
        );

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "the fixture this test rests on was not created",
        );

        test::read_body_json::<Workflow, _>(response).await
    }};
}

/// Links a service as `who`, failing loudly if the fixture did not take.
macro_rules! given_connection {
    ($app:expr, $who:expr, $name:expr) => {{
        let response = acting_as!(
            $app,
            $who,
            test::TestRequest::post()
                .uri("/api/v1/connections")
                .set_json(serde_json::json!({
                    "provider": "todoist",
                    "name": $name,
                    "key": "tok-fixture",
                }))
        );

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "the fixture this test rests on was not created",
        );

        test::read_body_json::<serde_json::Value, _>(response).await
    }};
}

#[actix_web::test]
async fn a_workflow_one_person_configured_is_neither_listed_nor_fetchable_by_another() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_workflow!(app, ALICE, feed("Alice's feed"));

    // The positive half: alice can see her own. Without this the absence below
    // would also be satisfied by a create that silently did nothing.
    let alices: Vec<Workflow> = read_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri("/api/v1/workflows")
    );
    assert_eq!(alices.len(), 1);
    assert_eq!(alices[0].id, hers.id);

    let bobs: Vec<Workflow> =
        read_acting_as!(app, BOB, test::TestRequest::get().uri("/api/v1/workflows"));
    assert!(
        bobs.is_empty(),
        "bob should not be shown a workflow that is not his: {bobs:?}",
    );

    let by_id = acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri(&format!("/api/v1/workflows/{}", hers.id))
    );

    // A 404 rather than a 403, and that is the right answer. `Scoped` hands the
    // handler a store that only holds bob's records, so the lookup genuinely
    // finds nothing and there is no decision to refuse. A 403 would be worse
    // than merely unhelpful: it would confirm that the identifier names a real
    // workflow belonging to somebody, turning this endpoint into an oracle for
    // enumerating other people's records without ever reading one.
    assert_eq!(by_id.status(), StatusCode::NOT_FOUND);
    assert_ne!(
        by_id.status(),
        StatusCode::FORBIDDEN,
        "refusing rather than denying knowledge would confirm the workflow exists",
    );

    // Indistinguishable from an identifier nobody was ever issued, which is what
    // "not an oracle" actually means.
    let never_issued = acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri(&format!(
            "/api/v1/workflows/{}",
            WorkflowId::from_entropy(0x0D15EA5E)
        ))
    );
    assert_eq!(
        by_id.status(),
        never_issued.status(),
        "somebody else's workflow should look exactly like one that never existed",
    );
}

#[actix_web::test]
async fn one_person_cannot_delete_another_persons_workflow() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_workflow!(app, ALICE, feed("Alice's feed"));

    let refused = acting_as!(
        app,
        BOB,
        test::TestRequest::delete().uri(&format!("/api/v1/workflows/{}", hers.id))
    );
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    assert_ne!(refused.status(), StatusCode::NO_CONTENT);

    // Asserting the status alone would pass against a handler that deleted the
    // record and then reported not finding it, so the record itself is checked.
    let still_there = acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri(&format!("/api/v1/workflows/{}", hers.id))
    );
    assert_eq!(
        still_there.status(),
        StatusCode::OK,
        "alice's workflow should have survived bob asking for it to be deleted",
    );
}

#[actix_web::test]
async fn one_person_cannot_edit_another_persons_workflow() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_workflow!(app, ALICE, feed("Alice's feed"));

    let refused = acting_as!(
        app,
        BOB,
        test::TestRequest::put()
            .uri(&format!("/api/v1/workflows/{}", hers.id))
            .set_json(serde_json::json!({
                "config": {
                    "name": "Bob got here",
                    "url": "https://bob.example.com/rss/",
                    "homepage": "https://bob.example.com/",
                },
                "schedule": "@hourly",
                "enabled": false,
            }))
    );
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    // An edit that was refused but had already been written would be the worst
    // outcome of the three, because nothing in the response would say so.
    let unchanged: Workflow = read_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri(&format!("/api/v1/workflows/{}", hers.id))
    );
    assert_eq!(unchanged.name, "Alice's feed");
    assert_eq!(unchanged.schedule.as_deref(), Some("@daily"));
    assert!(unchanged.enabled);
}

#[actix_web::test]
async fn a_connection_one_person_linked_is_neither_listed_nor_fetchable_by_another() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_connection!(app, ALICE, "Alice's Todoist");
    let id = hers["id"].as_str().unwrap().to_string();

    let alices: Vec<serde_json::Value> = read_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri("/api/v1/connections")
    );
    assert_eq!(alices.len(), 1);

    let bobs: Vec<serde_json::Value> = read_acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri("/api/v1/connections")
    );
    assert!(
        bobs.is_empty(),
        "bob should not be shown a linked account that is not his: {bobs:?}",
    );

    // Connections matter more than workflows here: even the summary says which
    // service somebody uses and under what name, which is worth keeping to
    // oneself regardless of the credential never being returned.
    let by_id = acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri(&format!("/api/v1/connections/{id}"))
    );
    assert_eq!(by_id.status(), StatusCode::NOT_FOUND);
    assert_ne!(by_id.status(), StatusCode::FORBIDDEN);

    let never_issued = acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri(&format!(
            "/api/v1/connections/{}",
            ConnectionId::from_entropy(0x0D15EA5E)
        ))
    );
    assert_eq!(
        by_id.status(),
        never_issued.status(),
        "somebody else's connection should look exactly like one that never existed",
    );
}

#[actix_web::test]
async fn one_person_cannot_delete_another_persons_connection() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_connection!(app, ALICE, "Alice's Todoist");
    let id = hers["id"].as_str().unwrap().to_string();

    let refused = acting_as!(
        app,
        BOB,
        test::TestRequest::delete().uri(&format!("/api/v1/connections/{id}"))
    );
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    assert_ne!(refused.status(), StatusCode::NO_CONTENT);

    // Unlinking somebody's account breaks every workflow that publishes through
    // it, so a deletion that reported failure but happened anyway would look
    // like the provider had revoked the token.
    let still_there = acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri(&format!("/api/v1/connections/{id}"))
    );
    assert_eq!(still_there.status(), StatusCode::OK);
}

#[actix_web::test]
async fn one_person_cannot_edit_another_persons_connection() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_connection!(app, ALICE, "Alice's Todoist");
    let id = hers["id"].as_str().unwrap().to_string();

    // The credential is the dangerous half of this endpoint: replacing it would
    // point alice's workflows at an account bob controls, and every task they
    // filed would land there without anything looking broken.
    let refused = acting_as!(
        app,
        BOB,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/connections/{id}"))
            .set_json(serde_json::json!({ "name": "Bob got here", "key": "tok-bobs" }))
    );
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    let unchanged: serde_json::Value = read_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri(&format!("/api/v1/connections/{id}"))
    );
    assert_eq!(unchanged["name"], "Alice's Todoist");
}

#[actix_web::test]
async fn the_key_value_browser_shows_only_the_acting_accounts_records() {
    use crate::db::KeyValueStore;
    use crate::prelude::Services;

    let context = two_people().await;

    // The same partition and the same key for both, so a handler that ignored
    // the account would hand back the wrong record rather than nothing — which
    // is a failure a test using distinct keys would miss.
    for (name, note) in [(ALICE, "alice's note"), (BOB, "bob's note")] {
        context
            .tenant(account(name))
            .kv()
            .set("notes", "shared", note)
            .await
            .unwrap();
    }

    let app = app!(context);

    let alices: Vec<automate_api::KeyValueEntry> =
        read_acting_as!(app, ALICE, test::TestRequest::get().uri("/api/v1/kv"));
    assert_eq!(alices.len(), 1, "alice should see one record: {alices:?}");
    assert_eq!(alices[0].payload, "alice's note");

    let body = text_acting_as!(app, ALICE, test::TestRequest::get().uri("/api/v1/kv"));
    assert!(
        !body.contains("bob's note"),
        "the key-value browser handed alice bob's record: {body}",
    );

    let bobs = text_acting_as!(app, BOB, test::TestRequest::get().uri("/api/v1/kv"));
    assert!(
        !bobs.contains("alice's note"),
        "the key-value browser handed bob alice's record: {bobs}",
    );
}

#[actix_web::test]
async fn the_queue_browser_shows_only_the_acting_accounts_messages() {
    use crate::db::Queue;
    use crate::prelude::Services;

    let context = two_people().await;

    for name in [ALICE, BOB] {
        context
            .tenant(account(name))
            .queue()
            .enqueue(
                "cron",
                serde_json::json!({ "owner": name }),
                Some("shared".into()),
                None,
            )
            .await
            .unwrap();
    }

    let app = app!(context);

    // The queue browser is where somebody would notice work they did not ask
    // for, so showing one person another's pending jobs would both leak what
    // they run and invite them to trigger or purge it.
    let alices: Vec<automate_api::QueueMessage> =
        read_acting_as!(app, ALICE, test::TestRequest::get().uri("/api/v1/queue"));
    assert_eq!(alices.len(), 1, "alice should see one message: {alices:?}");
    assert_eq!(alices[0].payload["owner"], ALICE);

    let body = text_acting_as!(app, ALICE, test::TestRequest::get().uri("/api/v1/queue"));
    assert!(
        !body.contains(BOB),
        "the queue browser handed alice bob's message: {body}",
    );

    // Checked in both directions, because a handler pinned to one fixed account
    // would still look right from that account's side.
    let bobs = text_acting_as!(app, BOB, test::TestRequest::get().uri("/api/v1/queue"));
    assert!(
        !bobs.contains(ALICE),
        "the queue browser handed bob alice's message: {bobs}",
    );
}

#[actix_web::test]
async fn a_webhook_address_never_appears_in_anybody_but_its_owners_view() {
    let context = two_people().await;
    let app = app!(context);

    let hers = given_workflow!(
        app,
        ALICE,
        serde_json::json!({
            "type": "webhook",
            "config": {
                "name": "Alice's deployments",
                "title": "Deployed ${{ environment }}",
                "todoist": { "connection": null },
            },
        })
    );

    // The address *is* the credential: anyone holding it can post a delivery
    // that files a task in alice's account. Showing it to bob would hand him
    // that ability without any further step, and rotating it is the only
    // remedy.
    let address = hers
        .webhook_path
        .clone()
        .expect("a webhook workflow should have been issued an address");
    let token = address.rsplit('/').next().unwrap().to_string();

    let alices = text_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri("/api/v1/workflows")
    );
    assert!(
        alices.contains(&token),
        "alice should be shown her own workflow's address: {alices}",
    );

    let bobs = text_acting_as!(app, BOB, test::TestRequest::get().uri("/api/v1/workflows"));
    assert!(
        !bobs.contains(&token),
        "bob was shown the address of alice's webhook: {bobs}",
    );

    // Nor by asking for the workflow directly, which is the other way the
    // address is served.
    let by_id = acting_as!(
        app,
        BOB,
        test::TestRequest::get().uri(&format!("/api/v1/workflows/{}", hers.id))
    );
    assert_eq!(by_id.status(), StatusCode::NOT_FOUND);

    // Nor by rotating it, which would break alice's address without ever
    // showing bob the replacement.
    let rotate = acting_as!(
        app,
        BOB,
        test::TestRequest::post().uri(&format!("/api/v1/workflows/{}/rotate-webhook", hers.id))
    );
    assert_ne!(rotate.status(), StatusCode::OK);

    let unchanged: Workflow = read_acting_as!(
        app,
        ALICE,
        test::TestRequest::get().uri(&format!("/api/v1/workflows/{}", hers.id))
    );
    assert_eq!(
        unchanged.webhook_path.as_deref(),
        Some(address.as_str()),
        "bob should not have been able to invalidate the address alice published",
    );
}

#[actix_web::test]
async fn the_installations_own_accounts_cannot_be_acted_as() {
    use crate::db::KeyValueStore;
    use crate::prelude::Services;

    let context = two_people().await;

    // Something recognisable in each reserved namespace, so that "the request
    // was refused" and "the request did not come back with those records" are
    // two separate assertions rather than one hoping to imply the other.
    // '!system' already holds the user registry; '!local' gets a record
    // standing in for everything a single-account installation had before it
    // adopted an identity provider.
    context
        .tenant(TenantId::local())
        .kv()
        .set("notes", "legacy", "from before anybody signed in")
        .await
        .unwrap();

    let app = app!(context);

    // `TenantId::new` refuses anything starting with '!' outright, before the
    // registry is ever consulted, so these are turned away as malformed rather
    // than as unknown. The prefix check happens before the name is lowercased,
    // which is why a shouted spelling is refused too — a refactor that
    // normalised first and then compared against a list of reserved names would
    // let that one through.
    for reserved in [TenantId::SYSTEM, TenantId::LOCAL, "!System"] {
        let response = acting_as!(app, reserved, test::TestRequest::get().uri("/api/v1/kv"));

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "'{reserved}' should not be an account anybody can act as",
        );

        // The refusal is what matters, but so is what it protects. '!system'
        // holds the user registry, so a request that got through would be
        // handed every account in the installation; '!local' holds whatever the
        // installation had before it started telling people apart, which is
        // nobody's to browse once it does.
        let body = String::from_utf8_lossy(&test::read_body(response).await).to_string();
        assert!(
            !body.contains(ALICE)
                && !body.contains(BOB)
                && !body.contains("from before anybody signed in"),
            "acting as '{reserved}' returned records from the reserved namespace: {body}",
        );
    }

    // The records really are in there, so the assertions above are refusing
    // something rather than describing empty namespaces.
    let registry = UserRegistry::new(context.tenant(TenantId::system()));
    let known: Vec<String> = registry
        .list()
        .await
        .unwrap()
        .iter()
        .map(|user| user.username.to_string())
        .collect();
    assert!(
        known.iter().any(|name| name == ALICE) && known.iter().any(|name| name == BOB),
        "the reserved namespace should hold the very records the refusals kept back: {known:?}",
    );
}
