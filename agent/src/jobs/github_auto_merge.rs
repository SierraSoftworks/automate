use automate_api::ConnectionId;

use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::jobs::subject_key;
use crate::prelude::*;
use crate::publishers::TodoistTarget;
use crate::publishers::{TodoistUpsertTask, TodoistUpsertTaskPayload};
use crate::services::{AutoMergeOutcome, GitHubAppClient, GitHubClient};
use crate::webhooks::GitHubPullRequestEvent;

/// What the GitHub webhook hands this job.
///
/// The settings travel with the event rather than being read from the
/// installation's configuration, because they belong to the workflow the
/// delivery arrived for.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GitHubAutoMergeTask {
    pub config: GitHubAutoMergeConfig,

    /// The connection naming the GitHub App installation the workflow serves.
    ///
    /// Carried as an identifier rather than as a minted token, for two reasons.
    /// The token is then obtained as late as possible, so a delivery that waited
    /// behind a backlog uses the connection as it stands when it runs — which is
    /// how a reinstall, issuing a fresh installation id for the same account,
    /// takes effect on work that was already queued. And a queue message never
    /// holds the means to write to somebody's repositories.
    ///
    /// Optional for the same reason [`crate::webhooks::GitHubWebhookConfig::connection`]
    /// is, and defaulted so that a message queued before this field existed
    /// still deserialises rather than poisoning the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionId>,

    pub event: GitHubPullRequestEvent,
}

impl std::fmt::Display for GitHubAutoMergeTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github/auto-merge")
    }
}

/// The pull requests an untouched auto-merge section acts on.
///
/// Held as its source text rather than only as a built [`Filter`], so that the
/// form's default and the `serde(default)` are literally the same string. The
/// two used to be unable to disagree because the form did not offer the field
/// at all; now that it does, sharing the constant is what keeps "left the form
/// alone" and "left the section out" meaning the same thing.
pub const DEFAULT_AUTO_MERGE_FILTER: &str =
    r#"action == "opened" && author in ["dependabot[bot]", "dependabot-preview[bot]"]"#;

/// The review body left on a pull request when approval is switched on.
pub const DEFAULT_APPROVAL_MESSAGE: &str =
    "This pull request has been automatically approved because it was raised by a trusted account.";

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubAutoMergeConfig {
    /// Whether this workflow enables auto-merge at all.
    ///
    /// Off unless somebody says so. This is the setting that has us approve and
    /// merge pull requests in their repositories, which is not a capability
    /// anybody should acquire by upgrading — and an explicit switch is also what
    /// lets the rest of these settings be reached by a dotted path, which an
    /// `Option` could not be.
    #[serde(default)]
    pub enabled: bool,

    /// Selects the pull requests which should have auto-merge enabled. Defaults
    /// to newly opened Dependabot pull requests.
    #[serde(default = "default_auto_merge_filter")]
    pub filter: Filter,

    /// Whether to leave an approving review on matching pull requests, which
    /// repositories with a required-review branch protection rule need before
    /// auto-merge can complete.
    #[serde(default)]
    pub approve: bool,

    #[serde(default = "default_approval_message")]
    pub approval_message: String,

    /// Where the reminders to turn on a repository's "Allow auto-merge" setting
    /// are filed.
    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Default for GitHubAutoMergeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            filter: default_auto_merge_filter(),
            approve: false,
            approval_message: default_approval_message(),
            todoist: TodoistTarget::default(),
        }
    }
}

fn default_auto_merge_filter() -> Filter {
    Filter::new(DEFAULT_AUTO_MERGE_FILTER).expect("the default auto-merge filter is always valid")
}

fn default_approval_message() -> String {
    DEFAULT_APPROVAL_MESSAGE.to_string()
}

/// Enables GitHub's native auto-merge behaviour on pull requests raised by
/// trusted accounts, so that they merge themselves once their required checks
/// have passed.
#[derive(Clone)]
pub struct GitHubAutoMergeWorkflow;

/// Which GitHub App installation a delivery may be acted on as.
#[derive(Debug)]
enum ActingInstallation {
    /// Act as this installation. It is the one the workflow's connection names,
    /// or — for a workflow which names no connection — the one the delivery
    /// claims.
    Installation(u64),

    /// No installation could be established at all, so no App token can be
    /// minted and a personal access token is the only credential left.
    Unknown,

    /// The delivery names an installation other than the one this workflow
    /// serves, so it is not this workflow's to act on.
    Foreign,
}

impl GitHubAutoMergeWorkflow {
    /// Reconciles the installation this workflow serves with the one the
    /// delivery claims to have come from.
    ///
    /// The workflow's connection is the authority. It is what the workflow's
    /// owner chose, it is stored on our side, and it names the account whose
    /// repositories they asked us to write to. The `installation` on the payload
    /// is only a claim: the signature proves that *GitHub* sent the delivery and
    /// that nothing rewrote it, which is not the same as proving the delivery is
    /// for the installation this workflow was set up for. A second installation
    /// of the same App produces deliveries which verify perfectly well and have
    /// nothing to do with this workflow, and acting on those would approve and
    /// merge pull requests its owner never asked about.
    async fn acting_installation(
        connection: Option<ConnectionId>,
        event: &GitHubPullRequestEvent,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<ActingInstallation, human_errors::Error> {
        let delivered = event
            .installation
            .as_ref()
            .map(|installation| installation.id);

        let Some(id) = connection else {
            // Transitional, and the one case where nothing is checked. A
            // workflow stored before `connection` existed names no installation
            // and there is no way to infer which one its owner meant, so
            // refusing the delivery outright would silently stop auto-merge
            // working for everybody who upgraded — a change they did not make,
            // reported nowhere they would look. The delivery's own claim is used
            // instead, which is exactly what happened before this check existed,
            // and the warning says out loud that it is being trusted. When the
            // field stops being optional this branch goes with it.
            warn!(
                "The workflow handling pull request {event} names no GitHub installation, so the installation the delivery claims is being trusted unchecked; set its GitHub installation to have this verified."
            );

            return Ok(match delivered {
                Some(installation) => ActingInstallation::Installation(installation),
                None => ActingInstallation::Unknown,
            });
        };

        let store = ConnectionStore::for_services(services);

        let Some(connection) = store.get(id).await? else {
            return Err(human_errors::user(
                format!(
                    "This workflow serves a GitHub installation ('{id}') which is no longer connected."
                ),
                &[
                    "Install the GitHub App on that account again, or point this workflow at a different installation.",
                ],
            ));
        };

        let ConnectionSecret::GitHubApp { installation_id } = store.open(&connection)? else {
            return Err(human_errors::user(
                format!(
                    "The connection '{id}' does not hold a GitHub App installation, so it cannot approve or merge pull requests."
                ),
                &["Point this workflow at the GitHub account you installed the App on."],
            ));
        };

        // A delivery from a plain repository webhook carries no installation at
        // all, so there is nothing for it to disagree with; the connection still
        // says which installation to act as.
        if let Some(delivered) = delivered
            && delivered != installation_id
        {
            warn!(
                connection.id = %id,
                "Ignoring pull request {event}: it was delivered for GitHub installation {delivered}, and this workflow serves installation {installation_id}."
            );

            return Ok(ActingInstallation::Foreign);
        }

        Ok(ActingInstallation::Installation(installation_id))
    }

    /// The credential management calls are made with.
    ///
    /// Prefers an App installation token, so the approval and merge are
    /// attributed to the App and limited to the repositories that installation
    /// covers. Falls back to the personal access token when the App is not
    /// configured, or when [`Self::acting_installation`] could not establish an
    /// installation to mint one for.
    async fn management_token(
        installation: Option<u64>,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Option<String>, human_errors::Error> {
        let config = services.config();

        if let Some(app) = config.connections.github.app.as_ref()
            && let Some(installation) = installation
        {
            let client = GitHubAppClient::new(app, services.http_client())?;
            return Ok(Some(
                client.installation_token(installation, services).await?,
            ));
        }

        Ok(config.connections.github.api_key.clone())
    }

    /// Raises a reminder to turn on the repository's "Allow auto-merge"
    /// setting.
    ///
    /// The payload is keyed on, and derived solely from, the repository, so
    /// every subsequent pull request from the same repository resolves to an
    /// unchanged upsert which Todoist is never told about.
    async fn request_repository_configuration(
        event: &GitHubPullRequestEvent,
        config: &GitHubAutoMergeConfig,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let repository = &event.repository.full_name;
        let unique_key = format!("github/auto-merge/{repository}");

        TodoistUpsertTask::dispatch(
            TodoistUpsertTaskPayload {
                unique_key: unique_key.clone(),
                title: format!(
                    "[**{repository}**](https://github.com/{repository}/settings): Enable auto-merge"
                ),
                description: Some(format!(
                    "Auto-merge could not be enabled on a pull request because {repository} does not allow it.\n\nTurn on **Allow auto-merge** under https://github.com/{repository}/settings."
                )),
                priority: Some(2),
                config: config.todoist.clone(),
                ..Default::default()
            },
            Some(unique_key.into()),
            services,
        )
        .await
    }

    /// Raises a reminder to merge this pull request by hand.
    ///
    /// Keyed and titled the way [`crate::jobs::GitHubAttentionWorkflow`] and the
    /// notifications collector key and title the same pull request, so a repository
    /// which also files reminders ends up with one task for it rather than two.
    async fn request_manual_merge(
        event: &GitHubPullRequestEvent,
        config: &GitHubAutoMergeConfig,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let repository = &event.repository.full_name;
        let unique_key = subject_key(repository, event.number);

        TodoistUpsertTask::dispatch(
            TodoistUpsertTaskPayload {
                unique_key: unique_key.clone(),
                title: format!(
                    "[**{repository}#{}**]({}): {}",
                    event.number, event.pull_request.html_url, event.pull_request.title
                ),
                description: Some(format!(
                    "Auto-merge is not available on {repository}, so this pull request needs merging by hand once its checks have passed."
                )),
                priority: Some(2),
                config: config.todoist.clone(),
                ..Default::default()
            },
            Some(unique_key.into()),
            services,
        )
        .await
    }
}

crate::register_job!(GitHubAutoMergeWorkflow);

impl Job for GitHubAutoMergeWorkflow {
    type JobType = GitHubAutoMergeTask;

    fn partition() -> &'static str {
        "github/auto-merge"
    }

    #[instrument("workflow.github_auto_merge.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let auto_merge = &job.config;
        let connection = job.connection;
        let job = &job.event;

        if !auto_merge.filter.matches(job)? {
            info!("Pull request {job} did not match the auto-merge filter; ignoring.");
            return Ok(());
        }

        // Whose delivery this is, asked only once we would otherwise act on it.
        // Evaluating the filter first has no side effects, so nothing is at risk
        // in the meantime, and this ordering keeps both of the warnings below to
        // the deliveries where they matter rather than repeating them for every
        // pull request the filter drops.
        let installation = match Self::acting_installation(connection, job, services).await? {
            ActingInstallation::Installation(id) => Some(id),
            ActingInstallation::Unknown => None,
            // Already reported. Deliberately not acted on, and deliberately not
            // fallen back to the payload's installation: minting a token for
            // that would be doing the delivery's bidding rather than this
            // workflow owner's.
            ActingInstallation::Foreign => return Ok(()),
        };

        let Some(token) = Self::management_token(installation, services).await? else {
            warn!(
                "Pull request {job} matched the auto-merge filter, but no GitHub credentials are configured; ignoring."
            );
            return Ok(());
        };

        let client = GitHubClient::new(services.http_client(), token);

        if auto_merge.approve
            && !client
                .approve_pull_request(&job.pull_request.node_id, &auto_merge.approval_message)
                .await?
        {
            warn!("Could not approve pull request {job}; continuing to enable auto-merge anyway.");
        }

        match client.enable_auto_merge(&job.pull_request.node_id).await? {
            AutoMergeOutcome::Enabled => {
                info!("Enabled auto-merge on pull request {job}.");
            }
            AutoMergeOutcome::NotAllowed => {
                // A private repository on a plan without auto-merge cannot turn
                // the setting on at all, so asking its owner to would be a task
                // they could never complete; the pull request is what needs a
                // person instead.
                if job.repository.private {
                    warn!(
                        "Auto-merge is not allowed on the private repository {}; raising a reminder to merge pull request {job} by hand.",
                        job.repository.full_name
                    );
                    Self::request_manual_merge(job, auto_merge, services).await?;
                } else {
                    warn!(
                        "Auto-merge is not allowed on {}; raising a reminder to enable it.",
                        job.repository.full_name
                    );
                    Self::request_repository_configuration(job, auto_merge, services).await?;
                }
            }
            AutoMergeOutcome::Declined(reason) => {
                warn!("Auto-merge could not be enabled on pull request {job}: {reason}");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    type TestServices = crate::services::ServicesContainer<crate::db::TenantDb>;

    fn event(action: &str, author: &str) -> GitHubPullRequestEvent {
        serde_json::from_value(serde_json::json!({
            "action": action,
            "number": 1,
            "pull_request": {
                "node_id": "PR_node",
                "html_url": "https://github.com/example/repo/pull/1",
                "title": "Bump serde",
                "draft": false,
                "user": { "login": author },
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "private": false,
                "owner": { "login": "example" },
            },
            "sender": { "login": author },
        }))
        .expect("the sample pull_request event should deserialize")
    }

    /// The sample pull request, delivered for a particular GitHub App
    /// installation.
    ///
    /// This is the field the whole exercise is about not trusting, so the tests
    /// below set it to something other than what the workflow serves and check
    /// that it is never the one acted on. It matches the default filter, so
    /// nothing but the installation check can be what stops a delivery.
    fn delivered_for(installation: u64) -> GitHubPullRequestEvent {
        let mut event = event("opened", "dependabot[bot]");
        event.installation = serde_json::from_value(serde_json::json!({ "id": installation }))
            .expect("the sample installation should deserialize");
        event
    }

    /// Mock services whose GitHub App addresses its calls to `server`.
    ///
    /// No personal access token is configured, so the only credential these can
    /// produce is an installation token — which means a test asserting one was
    /// minted cannot be satisfied by a fallback.
    async fn services_with_app(server: &MockServer) -> TestServices {
        let api_url = server.uri();

        TestServices::new_custom_mock(move |config, _| {
            config.connections.github.app = Some(crate::testing::github_app(api_url));
        })
        .await
        .expect("build mock services")
    }

    /// Links an installation as a connection, the way an `installation` webhook
    /// or the setup wizard does, and hands back what a workflow would name.
    async fn link(services: &TestServices, installation: u64) -> ConnectionId {
        ConnectionStore::for_services(services)
            .create(
                crate::integrations::github_app::GITHUB_PROVIDER,
                "SierraSoftworks",
                Some("SierraSoftworks".into()),
                ConnectionSecret::GitHubApp {
                    installation_id: installation,
                },
            )
            .await
            .expect("link the GitHub App installation")
            .id
    }

    /// A GitHub which will mint a token for exactly this installation.
    ///
    /// Anything else 404s, which is what makes "the outbound request used the
    /// right installation" an assertion rather than a hope: a request for
    /// another installation cannot quietly succeed.
    async fn mints_for(server: &MockServer, installation: u64, token: &str) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/installations/{installation}/access_tokens"
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": token,
                "expires_at": "2026-08-02T12:00:00Z",
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn acting_as(
        services: &TestServices,
        connection: Option<ConnectionId>,
        event: &GitHubPullRequestEvent,
    ) -> ActingInstallation {
        GitHubAutoMergeWorkflow::acting_installation(connection, event, services)
            .await
            .expect("reconciling the installation should succeed")
    }

    /// The credential the job would make its management calls with.
    ///
    /// The tests stop here rather than running [`Job::handle`] whenever they
    /// expect a token back: the GraphQL calls which follow are addressed to
    /// github.com and are not what is under test.
    async fn token_for(services: &TestServices, installation: u64) -> String {
        GitHubAutoMergeWorkflow::management_token(Some(installation), services)
            .await
            .expect("minting the token should succeed")
            .expect("an App installation always yields a token")
    }

    fn matches(action: &str, author: &str) -> bool {
        default_auto_merge_filter()
            .matches(&event(action, author))
            .expect("the default filter should evaluate")
    }

    #[test]
    fn default_filter_selects_new_dependabot_pull_requests() {
        assert!(matches("opened", "dependabot[bot]"));
        assert!(matches("opened", "dependabot-preview[bot]"));
        assert!(!matches("synchronize", "dependabot[bot]"));
        assert!(!matches("opened", "notheotherben"));
    }

    #[test]
    fn a_section_nobody_has_filled_in_is_switched_off() {
        // The switch replaced an `Option` whose presence turned this on, so the
        // reading of an empty object had to change. Off is the only safe answer:
        // this approves and merges pull requests in somebody's repositories.
        let empty: GitHubAutoMergeConfig = serde_json::from_value(serde_json::json!({}))
            .expect("an empty section should load at its defaults");

        assert!(!empty.enabled);
        assert!(!GitHubAutoMergeConfig::default().enabled);
    }

    #[test]
    fn the_settings_survive_being_stored_and_read_back() {
        // These are part of a workflow's stored configuration now rather than an
        // internal job's private state, so they have to write as well as read —
        // and a filter that did not round-trip would be silently replaced with
        // its default the next time the workflow was saved.
        let config = GitHubAutoMergeConfig {
            enabled: true,
            approve: true,
            filter: Filter::new(r#"author == "notheotherben""#).unwrap(),
            ..Default::default()
        };

        let round_tripped: GitHubAutoMergeConfig = serde_json::from_value(
            serde_json::to_value(&config).expect("the settings should write"),
        )
        .expect("the settings should read back");

        assert!(round_tripped.enabled);
        assert!(round_tripped.approve);
        assert_eq!(round_tripped.filter.raw(), r#"author == "notheotherben""#);
        assert_eq!(round_tripped.approval_message, DEFAULT_APPROVAL_MESSAGE);
    }

    #[test]
    fn the_advertised_defaults_are_the_ones_the_struct_uses() {
        // The form offers these constants as its starting values, so if they
        // stopped being what an omitted section falls back to then an untouched
        // form and a missing section would behave differently.
        let empty: GitHubAutoMergeConfig = serde_json::from_value(serde_json::json!({})).unwrap();

        assert_eq!(empty.filter.raw(), DEFAULT_AUTO_MERGE_FILTER);
        assert_eq!(empty.approval_message, DEFAULT_APPROVAL_MESSAGE);
    }

    #[test]
    fn a_task_queued_before_it_named_an_installation_still_loads() {
        // Deliveries sit in the queue across a restart, so an upgrade must not
        // strand the ones written before this field existed — a message that
        // failed to deserialise would be retried forever rather than skipped.
        let queued: GitHubAutoMergeTask = serde_json::from_value(serde_json::json!({
            "config": {},
            "event": serde_json::to_value(event("opened", "dependabot[bot]")).unwrap(),
        }))
        .expect("a task written before the connection existed should load");

        assert!(queued.connection.is_none());
    }

    #[tokio::test]
    async fn repositories_without_auto_merge_raise_a_single_reminder() {
        let services = crate::testing::mock_services()
            .await
            .expect("build mock services");
        let config = GitHubAutoMergeConfig::default();

        let first = event("opened", "dependabot[bot]");
        let mut second = event("opened", "dependabot[bot]");
        second.number = 2;
        second.pull_request.node_id = "PR_node_2".to_string();

        for pull_request in [&first, &second] {
            GitHubAutoMergeWorkflow::request_repository_configuration(
                pull_request,
                &config,
                &services,
            )
            .await
            .expect("the reminder should be raised");
        }

        let reminders = services
            .queue()
            .peek::<_, TodoistUpsertTaskPayload>(TodoistUpsertTask::partition(), 10)
            .await
            .expect("peek the Todoist upsert queue");

        assert_eq!(
            reminders.len(),
            1,
            "every pull request from a repository should collapse onto one reminder"
        );
        assert_eq!(reminders[0].key, "github/auto-merge/example/repo");
        assert_eq!(
            reminders[0].payload.title,
            "[**example/repo**](https://github.com/example/repo/settings): Enable auto-merge"
        );
    }

    /// A private repository cannot necessarily turn auto-merge on at all, so the
    /// reminder is about the pull request rather than the repository's settings —
    /// and it is keyed the way the attention path keys the same pull request, so
    /// the two converge on one task instead of raising a second one.
    #[tokio::test]
    async fn private_repositories_are_reminded_about_the_pull_request() {
        let services = crate::testing::mock_services()
            .await
            .expect("build mock services");
        let config = GitHubAutoMergeConfig::default();

        let mut first = event("opened", "dependabot[bot]");
        first.repository.private = true;

        let mut second = first.clone();
        second.number = 2;
        second.pull_request.html_url = "https://github.com/example/repo/pull/2".to_string();

        for pull_request in [&first, &second] {
            GitHubAutoMergeWorkflow::request_manual_merge(pull_request, &config, &services)
                .await
                .expect("the reminder should be raised");
        }

        let reminders = services
            .queue()
            .peek::<_, TodoistUpsertTaskPayload>(TodoistUpsertTask::partition(), 10)
            .await
            .expect("peek the Todoist upsert queue");

        let keys: Vec<_> = reminders.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "github/attention/example/repo#1",
                "github/attention/example/repo#2"
            ],
            "each pull request needs merging on its own, and shares its key with the attention path"
        );

        assert_eq!(
            reminders[0].payload.title,
            "[**example/repo#1**](https://github.com/example/repo/pull/1): Bump serde",
            "the title must match the one the attention path renders for the same subject"
        );
    }

    #[tokio::test]
    async fn unmatched_pull_requests_are_ignored() {
        // No GitHub API key is configured, so a call to GitHub would fail; the
        // job completing successfully proves the filter short-circuited first.
        let services = TestServices::new_mock().await.expect("build mock services");

        GitHubAutoMergeWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &GitHubAutoMergeTask {
                    config: GitHubAutoMergeConfig::default(),
                    connection: None,
                    event: event("opened", "notheotherben"),
                },
            )
            .await
            .expect("an unmatched pull request should be ignored without erroring");
    }

    /// The ordinary case: the delivery came from the installation this workflow
    /// was set up for, so it is acted on, with a token for that installation.
    #[tokio::test]
    async fn a_delivery_from_the_installation_the_workflow_serves_is_acted_on() {
        let server = MockServer::start().await;
        mints_for(&server, 42, "ghs_installation_42").await;

        let services = services_with_app(&server).await;
        let connection = link(&services, 42).await;

        let ActingInstallation::Installation(installation) =
            acting_as(&services, Some(connection), &delivered_for(42)).await
        else {
            panic!("a delivery from the installation this workflow serves should be acted on");
        };

        assert_eq!(installation, 42);
        assert_eq!(
            token_for(&services, installation).await,
            "ghs_installation_42"
        );
    }

    /// The defect this exists for. A delivery for a *different* installation of
    /// the same App verifies its signature perfectly well — GitHub really did
    /// send it — and is still nothing to do with this workflow. Acting on it
    /// would approve and merge pull requests in repositories its owner never
    /// pointed us at.
    #[tokio::test]
    async fn a_delivery_naming_a_different_installation_is_not_acted_on() {
        let server = MockServer::start().await;
        let services = services_with_app(&server).await;
        let connection = link(&services, 42).await;

        assert!(
            matches!(
                acting_as(&services, Some(connection), &delivered_for(99)).await,
                ActingInstallation::Foreign
            ),
            "a delivery for another installation is not this workflow's to act on"
        );

        // And the whole job, not merely the reconciliation, does nothing: it
        // completes without approving, merging, or filing a reminder. Nothing is
        // mounted for installation 99, so had a token been minted for it the
        // call would have 404'd and this would have failed rather than passed
        // quietly.
        GitHubAutoMergeWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &GitHubAutoMergeTask {
                    config: GitHubAutoMergeConfig::default(),
                    connection: Some(connection),
                    event: delivered_for(99),
                },
            )
            .await
            .expect("a delivery for another installation should be dropped without erroring");

        assert!(
            server
                .received_requests()
                .await
                .expect("the mock server should be recording requests")
                .is_empty(),
            "no token may be minted for an installation this workflow does not serve"
        );

        assert!(
            services
                .queue()
                .peek::<_, TodoistUpsertTaskPayload>(TodoistUpsertTask::partition(), 10)
                .await
                .expect("peek the Todoist upsert queue")
                .is_empty()
        );
    }

    /// The property the change is really about: which installation the outbound
    /// request addressed. The connection names 42 and the payload names nothing
    /// at all, so a token for 42 could only have come from the connection —
    /// code reading the payload would have no installation to use and would fall
    /// through to a personal access token that is not configured.
    ///
    /// The other half of the claim, that a payload naming a *different*
    /// installation does not get a token minted for it, is
    /// [`a_delivery_naming_a_different_installation_is_not_acted_on`]: there is
    /// deliberately no case where the two disagree and a token is still minted,
    /// so it cannot be asserted here.
    #[tokio::test]
    async fn the_token_is_minted_from_the_connections_installation_not_the_payloads() {
        let server = MockServer::start().await;
        mints_for(&server, 42, "ghs_from_the_connection").await;

        let services = services_with_app(&server).await;
        let connection = link(&services, 42).await;

        // A plain repository webhook carries no `installation` at all, which is
        // also why its absence must not be treated as a disagreement.
        let delivery = event("opened", "dependabot[bot]");
        assert!(delivery.installation.is_none());

        let ActingInstallation::Installation(installation) =
            acting_as(&services, Some(connection), &delivery).await
        else {
            panic!("the connection alone is enough to say which installation to act as");
        };

        assert_eq!(installation, 42);
        assert_eq!(
            token_for(&services, installation).await,
            "ghs_from_the_connection"
        );
    }

    /// Transitional: a workflow stored before it could name an installation has
    /// to keep working, so it falls back to the delivery's own claim — which is
    /// exactly what every workflow did before this check existed.
    #[tokio::test]
    async fn a_workflow_naming_no_installation_still_works_as_it_did() {
        let server = MockServer::start().await;
        mints_for(&server, 99, "ghs_from_the_payload").await;

        let services = services_with_app(&server).await;

        let ActingInstallation::Installation(installation) =
            acting_as(&services, None, &delivered_for(99)).await
        else {
            panic!("a workflow naming no installation has nothing to check against");
        };

        assert_eq!(installation, 99);
        assert_eq!(
            token_for(&services, installation).await,
            "ghs_from_the_payload"
        );
    }

    /// A workflow left pointing at an installation somebody has since removed is
    /// a configuration problem its owner has to fix. Falling back to the
    /// payload's installation would turn "your connection is gone" into
    /// "anything GitHub sends will do", which is the very thing being closed.
    #[tokio::test]
    async fn a_workflow_whose_installation_has_been_disconnected_says_so() {
        let server = MockServer::start().await;
        let services = services_with_app(&server).await;
        let connection = link(&services, 42).await;

        ConnectionStore::for_services(&services)
            .delete(connection)
            .await
            .expect("remove the connection");

        let err = GitHubAutoMergeWorkflow::acting_installation(
            Some(connection),
            &delivered_for(42),
            &services,
        )
        .await
        .expect_err("a workflow serving a connection that is gone should report it");

        assert!(err.to_string().contains("no longer connected"), "{err}");
        assert!(
            server
                .received_requests()
                .await
                .expect("the mock server should be recording requests")
                .is_empty(),
            "nothing may be minted for a workflow whose installation is gone"
        );
    }
}
