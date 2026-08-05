//! Moving the workflows in `config.toml` into the database.
//!
//! Workflows used to be described entirely by the configuration file: the agent
//! read `[workflows]` at start-up and pushed a schedule for every entry it found
//! there. They are records now, so that several people can each hold their own
//! and edit them from a browser without restarting anything. An installation
//! upgrading across that change has its workflows in the old place and nothing
//! in the new one, and would keep running them from the file forever.
//!
//! This moves them, once, and then never looks at the file again.
//!
//! # Why once, rather than on every start
//!
//! A deterministic mapping from file entry to workflow would be idempotent, and
//! re-deriving the records at every start would keep the two in step without
//! needing to remember anything. It would also mean that a workflow somebody has
//! since edited in the browser is silently overwritten by the file the next time
//! the agent restarts — the file would still be in charge, which is precisely
//! what moving the workflows out of it was meant to end.
//!
//! So the database is the source of truth once the move has happened, and the
//! file is the thing being left behind. A marker written into the tenant's own
//! key-value space records that the move happened, and after that the
//! `[workflows]` section is not read at all. Re-applying a file is what the TOML
//! import endpoint is for, and it is opt-in on purpose.
//!
//! # The order things happen in
//!
//! Import, then purge the schedules the file used to push, then write the
//! marker. A crash part-way through therefore leaves the migration to run again
//! and produce duplicates, rather than leaving workflows behind unimported.
//! Duplicates are visible in the browser and can be deleted; a workflow that was
//! never imported is invisible, and its owner only finds out when the thing it
//! was watching stops being watched.
//!
//! # What is not moved
//!
//! `[[workflows.github_notifications]]` and
//! `[workflows.github_notifications_cleanup]` stay where they are. They are the
//! installation's own maintenance rather than somebody's workflow — nobody chose
//! them, and nobody should be able to delete them from a browser — so they have
//! no [`ConfigurableWorkflow`] implementation and no record to be moved into.
//! They keep scheduling themselves from `setup()`, carrying their configuration
//! inline on the queue, which is why [`CronJobTask::task`] still exists.

use chrono::{DateTime, Utc};

use human_errors::Error;

use crate::db::KeyValueStore;
use crate::jobs::{
    CRON_PARTITION, CalendarWorkflow, CronJobConfig, CronJobTask, GitHubReleasesWorkflow,
    RssWorkflow, XkcdWorkflow, YnabStocksWorkflow, YouTubeWorkflow,
};
use crate::prelude::*;
use crate::workflow_store::{WorkflowDraft, WorkflowStore};
use crate::workflows::ConfigurableWorkflow;

/// The key-value partition recording which one-off migrations have run.
///
/// Kept in the tenant's own space rather than somewhere installation-wide,
/// because what the marker describes is that tenant's workflows. The move itself
/// only ever runs for the local tenant — see
/// [`import_configured_workflows`] — but the record of it belongs beside the
/// records it produced.
pub const MIGRATIONS_PARTITION: &str = "migrations";

/// The marker saying this tenant's configured workflows have been moved.
pub const WORKFLOWS_FROM_CONFIG: &str = "workflows-from-config";

/// The most armed schedules we will look at when clearing out the ones the file
/// pushed.
///
/// Matches the bound the reconciler works under, for the same reason: a tenant
/// with more armed schedules than this has something wrong with it, and walking
/// an unbounded list at start-up is how that becomes a slow start nobody can
/// explain.
const MAX_SCHEDULES: usize = 1000;

/// What was written down when the move happened.
///
/// The count and the timestamp are not read by anything; they are here so that
/// somebody looking at the database afterwards can tell what this did and when,
/// which a bare `true` would not.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Marker {
    at: DateTime<Utc>,
    imported: usize,
}

/// Moves the workflows described by the configuration file into the database.
///
/// Called for the local tenant alone, because the configuration file describes
/// one installation rather than any particular person's workflows.
///
/// Returns how many were imported. Zero means either that there were none to
/// import or that the move has already happened; both are ordinary, and neither
/// is worth distinguishing to a caller whose only job is to log a failure.
///
/// The store built here has no webhook token index, because none of the types
/// the configuration file can describe are reached by a URL — the file predates
/// webhook workflows entirely — so there is no address for this to mint.
#[instrument("workflow_migration.import", skip(services), err(Display))]
pub async fn import_configured_workflows<S: Services>(services: &S) -> Result<usize, Error> {
    if services
        .kv()
        .get::<Marker>(MIGRATIONS_PARTITION, WORKFLOWS_FROM_CONFIG)
        .await?
        .is_some()
    {
        return Ok(0);
    }

    let config = services.config();
    let workflows = &config.workflows;
    let store = WorkflowStore::new(services);

    let mut imported = 0;
    imported += import_entries(&store, &workflows.rss).await?;
    imported += import_entries(&store, &workflows.calendars).await?;
    imported += import_entries(&store, &workflows.youtube).await?;
    imported += import_entries(&store, &workflows.github_releases).await?;
    imported += import_entries(&store, &workflows.xkcd).await?;
    imported += import_entries(&store, &workflows.ynab_stocks).await?;

    let purged = purge_inline_schedules(services).await?;

    // Written even when there was nothing to import, so that an installation
    // which never had a `[workflows]` section does not re-scan for one at every
    // start for the rest of its life.
    services
        .kv()
        .set(
            MIGRATIONS_PARTITION,
            WORKFLOWS_FROM_CONFIG,
            Marker {
                at: Utc::now(),
                imported,
            },
        )
        .await?;

    if imported > 0 {
        info!(
            workflows.imported = imported,
            schedules.purged = purged,
            "Moved {imported} workflow(s) out of your configuration file and into the database. \
             The [workflows] section is no longer read, so you can delete it from config.toml; \
             edit these workflows in the browser, or through the workflow import and export \
             endpoints, from now on.",
        );
    }

    Ok(imported)
}

/// Writes one type's configured entries out as workflow records.
///
/// The entry's own settings become the workflow's configuration, unchanged: the
/// file and the record describe the same thing, and the handler is handed the
/// same value either way.
async fn import_entries<J, S>(
    store: &WorkflowStore<S>,
    entries: &[CronJobConfig<J>],
) -> Result<usize, Error>
where
    J: ConfigurableWorkflow,
    J::JobType: serde::Serialize + std::fmt::Display,
    S: Services,
{
    let mut imported = 0;

    for entry in entries {
        // Named the way the old scheduler named it — this is the identity it was
        // armed under — so a warning here can be matched against the entry in the
        // file it came from.
        let name = entry.job.to_string();

        let config = match serde_json::to_value(&entry.job) {
            Ok(config) => config,
            Err(err) => {
                warn!(
                    workflow.type = J::type_id(),
                    workflow.name = %name,
                    error = %err,
                    "Leaving '{name}' in your configuration file because its settings could not be read: {err}",
                );
                continue;
            }
        };

        let draft = WorkflowDraft {
            type_id: J::type_id().to_string(),
            config,
            // The pattern as croner understood it, which is the file's schedule
            // with its shorthands already expanded: `@daily` arrives here as
            // `0 0 * * *`. Equivalent, and the only form available, since a
            // parsed schedule does not keep the text it came from.
            schedule: Some(entry.cron.to_string()),
            // The file had no way to say "configured but not running", so
            // everything it describes is something the installation is doing
            // right now and must go on doing.
            enabled: true,
        };

        match store.create(draft).await {
            Ok(workflow) => {
                imported += 1;
                debug!(
                    workflow.id = %workflow.id,
                    workflow.type = J::type_id(),
                    "Moved '{name}' out of the configuration file.",
                );
            }
            Err(err) => {
                // One bad entry in a configuration file should not cost somebody
                // every other workflow they had, so it is reported by name and
                // the rest carry on. It stays in the file, where it can be
                // fixed and imported through the TOML endpoint.
                warn!(
                    workflow.type = J::type_id(),
                    workflow.name = %name,
                    error = %err,
                    "Could not move '{name}' out of your configuration file, so it has been left there: {err}",
                );
            }
        }
    }

    Ok(imported)
}

/// Removes the schedules the configuration file used to push for the types that
/// have just become records.
///
/// A schedule carrying its own configuration inline is one the old start-up push
/// armed. Now that the same work has a record, the reconciler arms a second
/// schedule naming that record, and the workflow runs twice — once from each.
///
/// Rows belonging to the installation's own maintenance carry their
/// configuration inline too, and are deliberately left alone: those have no
/// record to be a duplicate of, and purging them would stop the housekeeping
/// until the next restart re-armed it.
async fn purge_inline_schedules<S: Services>(services: &S) -> Result<usize, Error> {
    let converted = converted_partitions();

    let armed: Vec<crate::db::PeekedMessage<CronJobTask>> =
        services.queue().peek(CRON_PARTITION, MAX_SCHEDULES).await?;

    let mut purged = 0;

    for message in armed {
        // A schedule naming a workflow already belongs to a record, and the
        // reconciler is what looks after it.
        if message.payload.workflow.is_some() {
            continue;
        }

        if !converted.contains(&message.payload.kind.as_str()) {
            continue;
        }

        services
            .queue()
            .purge(CRON_PARTITION, message.key.clone())
            .await?;
        purged += 1;
    }

    Ok(purged)
}

/// The queue partitions the converted workflow types dispatch into.
///
/// Asked of the types themselves rather than written out as strings, so that
/// renaming a partition cannot leave this quietly matching nothing and the
/// duplicate schedules going unnoticed.
fn converted_partitions() -> [&'static str; 6] {
    [
        <RssWorkflow as Job>::partition(),
        <CalendarWorkflow as Job>::partition(),
        <YouTubeWorkflow as Job>::partition(),
        <GitHubReleasesWorkflow as Job>::partition(),
        <XkcdWorkflow as Job>::partition(),
        <YnabStocksWorkflow as Job>::partition(),
    ]
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::services::ServicesContainer;

    type TestServices = ServicesContainer<crate::db::TenantDb>;

    /// Builds mock services whose configuration is the given file.
    ///
    /// Written as TOML rather than assembled from structs so that the test is
    /// exercising the same path an operator's `config.toml` takes, including the
    /// types whose fields are private to their own module and could not be
    /// constructed here at all.
    async fn services_configured_with(file: &str) -> TestServices {
        let file = file.to_string();

        ServicesContainer::new_custom_mock(move |config, _| {
            *config = toml::from_str(&file).expect("the test configuration should parse");
        })
        .await
        .unwrap()
    }

    /// Whether the marker saying the move has happened is present.
    async fn migration_recorded(services: &TestServices) -> bool {
        services
            .kv()
            .get::<Marker>(MIGRATIONS_PARTITION, WORKFLOWS_FROM_CONFIG)
            .await
            .unwrap()
            .is_some()
    }

    async fn armed(services: &TestServices) -> Vec<crate::db::PeekedMessage<CronJobTask>> {
        services.queue().peek(CRON_PARTITION, 100).await.unwrap()
    }

    /// Arms a schedule the way the old start-up push did: carrying the whole
    /// configuration, naming no record.
    async fn arm_inline(services: &TestServices, kind: &str, key: &str) {
        services
            .queue()
            .enqueue(
                CRON_PARTITION,
                CronJobTask {
                    cron: croner::Cron::from_str("@daily").unwrap(),
                    kind: kind.to_string(),
                    idempotency_key: Some(key.to_string()),
                    task: serde_json::json!({ "name": "From the file" }),
                    workflow: None,
                },
                Some(key.to_string().into()),
                None,
            )
            .await
            .unwrap();
    }

    /// One of every type the configuration file could describe, so that a type
    /// left out of the migration is a failing test rather than a workflow that
    /// quietly stops running after an upgrade.
    const EVERY_TYPE: &str = r#"
        [[workflows.rss]]
        name = "Citation Needed"
        homepage = "https://citationneeded.news/"
        url = "https://citationneeded.news/rss/"
        cron = "0 */6 * * *"

        [[workflows.calendars]]
        name = "Personal"
        url = "https://example.com/calendar.ics"
        cron = "*/30 * * * *"

        [[workflows.youtube]]
        name = "Technology Connections"
        channel_id = "UCy0tKL1T7wFoYcxCe0xjN6Q"
        cron = "0 */6 * * *"

        [[workflows.github_releases]]
        repository = "SierraSoftworks/automate"
        cron = "0 */8 * * *"

        [[workflows.xkcd]]
        cron = "0 8 * * *"

        [[workflows.ynab_stocks]]
        budget = "00000000-0000-0000-0000-000000000000"
        cron = "0 */12 * * *"
    "#;

    #[tokio::test]
    async fn every_workflow_in_the_configuration_file_becomes_a_record() {
        let services = services_configured_with(EVERY_TYPE).await;

        let imported = import_configured_workflows(&services).await.unwrap();
        assert_eq!(imported, 6);

        let store = WorkflowStore::new(&services);
        let mut types: Vec<String> = store
            .records()
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.type_id)
            .collect();
        types.sort();

        assert_eq!(
            types,
            vec![
                "calendar",
                "github-releases",
                "rss",
                "xkcd",
                "ynab-stocks",
                "youtube",
            ],
            "a type the migration forgets is a workflow that stops running after an upgrade",
        );
    }

    #[tokio::test]
    async fn an_imported_workflow_keeps_the_schedule_and_the_settings_it_had() {
        // The whole point of moving these is that they carry on doing exactly
        // what they were doing, at the same times, without anybody re-entering
        // them.
        let services = services_configured_with(EVERY_TYPE).await;
        import_configured_workflows(&services).await.unwrap();

        let store = WorkflowStore::new(&services);
        let records = store.records().await.unwrap();

        let rss = records
            .iter()
            .find(|record| record.type_id == "rss")
            .expect("the RSS entry should have been imported");

        assert_eq!(rss.schedule.as_deref(), Some("0 */6 * * *"));
        assert_eq!(rss.config["name"], "Citation Needed");
        assert_eq!(rss.config["url"], "https://citationneeded.news/rss/");
        assert!(
            rss.enabled,
            "a workflow the installation was running should still be running afterwards",
        );
    }

    #[tokio::test]
    async fn a_schedule_written_as_a_shorthand_survives_as_the_pattern_it_means() {
        // A parsed schedule does not keep the text it was written as, so
        // `@daily` comes back as its expansion. That is the same schedule, which
        // is what matters; this is here so the behaviour is a decision rather
        // than a surprise.
        let services = services_configured_with(
            r#"
            [[workflows.rss]]
            name = "Word of the Day"
            homepage = "https://www.merriam-webster.com/word"
            url = "https://www.merriam-webster.com/word/feed/rss2"
            cron = "@daily"
            "#,
        )
        .await;

        import_configured_workflows(&services).await.unwrap();

        let records = WorkflowStore::new(&services).records().await.unwrap();
        assert_eq!(records[0].schedule.as_deref(), Some("0 0 * * *"));
    }

    #[tokio::test]
    async fn running_the_migration_twice_imports_nothing_the_second_time() {
        let services = services_configured_with(EVERY_TYPE).await;

        assert_eq!(import_configured_workflows(&services).await.unwrap(), 6);
        assert_eq!(
            import_configured_workflows(&services).await.unwrap(),
            0,
            "the marker should stop the file being read a second time",
        );

        assert_eq!(
            WorkflowStore::new(&services).records().await.unwrap().len(),
            6,
            "a second run must not leave the installation with two of everything",
        );
    }

    #[tokio::test]
    async fn a_workflow_edited_after_the_move_is_not_overwritten_by_a_later_run() {
        // This is the whole reason the marker exists. Re-deriving the records
        // from the file at every start would be idempotent against the file and
        // destructive against the person: somebody who corrects a feed URL in
        // the browser would find it reverted by the next restart, with nothing
        // to say why.
        let services = services_configured_with(
            r#"
            [[workflows.rss]]
            name = "Citation Needed"
            homepage = "https://citationneeded.news/"
            url = "https://citationneeded.news/rss/"
            cron = "@daily"
            "#,
        )
        .await;

        import_configured_workflows(&services).await.unwrap();

        let store = WorkflowStore::new(&services);
        let imported = store.records().await.unwrap().remove(0);

        store
            .update(
                imported.id,
                WorkflowDraft {
                    type_id: "rss".into(),
                    config: serde_json::json!({
                        "name": "Citation Needed",
                        "homepage": "https://citationneeded.news/",
                        "url": "https://citationneeded.news/feed.xml",
                    }),
                    schedule: Some("0 */2 * * *".into()),
                    enabled: false,
                },
            )
            .await
            .unwrap();

        import_configured_workflows(&services).await.unwrap();

        let records = store.records().await.unwrap();
        assert_eq!(records.len(), 1, "the file must not add a second copy");
        assert_eq!(
            records[0].config["url"],
            "https://citationneeded.news/feed.xml"
        );
        assert_eq!(records[0].schedule.as_deref(), Some("0 */2 * * *"));
        assert!(
            !records[0].enabled,
            "pausing a workflow must not be undone by the file it came from",
        );
    }

    #[tokio::test]
    async fn an_installation_with_no_workflows_in_its_file_still_records_the_move() {
        // Otherwise every start would go looking through a section that is never
        // going to have anything in it, and a `[workflows]` section added later
        // by hand would be imported behind the operator's back.
        let services = services_configured_with("").await;

        assert_eq!(import_configured_workflows(&services).await.unwrap(), 0);
        assert!(
            migration_recorded(&services).await,
            "an installation with nothing to import has still finished migrating",
        );
    }

    #[tokio::test]
    async fn a_stale_inline_schedule_for_a_converted_workflow_is_purged() {
        // The record the migration just wrote gets its own schedule from the
        // reconciler. Leaving the one the file pushed would have the feed polled
        // and the task filed twice.
        let services = services_configured_with(
            r#"
            [[workflows.rss]]
            name = "Citation Needed"
            homepage = "https://citationneeded.news/"
            url = "https://citationneeded.news/rss/"
            cron = "@daily"
            "#,
        )
        .await;

        arm_inline(&services, "rss/todoist", "rss/Citation Needed").await;

        import_configured_workflows(&services).await.unwrap();

        assert!(
            armed(&services).await.is_empty(),
            "the schedule the file pushed should be gone, leaving the record's own to be armed",
        );
    }

    #[tokio::test]
    async fn the_installations_own_maintenance_keeps_its_inline_schedule() {
        // These have no record to be a duplicate of, because nobody configured
        // them and nobody can. Purging them would stop the housekeeping until
        // something restarted the agent.
        let services = services_configured_with("").await;

        arm_inline(
            &services,
            "github/notifications/cleanup",
            "github-notifications-cleanup",
        )
        .await;
        arm_inline(&services, "workflow/todoist-cleanup", "todoist-cleanup").await;

        import_configured_workflows(&services).await.unwrap();

        let armed = armed(&services).await;
        assert_eq!(
            armed.len(),
            2,
            "the installation's own schedules are not anybody's workflow and are not ours to remove",
        );
    }

    #[tokio::test]
    async fn a_schedule_that_already_names_a_workflow_is_left_alone() {
        // A record's schedule belongs to the reconciler. Removing one here would
        // silently unschedule a workflow that is working perfectly well.
        let services = services_configured_with("").await;

        let store = WorkflowStore::new(&services);
        let existing = store
            .create(WorkflowDraft {
                type_id: "rss".into(),
                config: serde_json::json!({
                    "name": "Already a record",
                    "homepage": "https://example.com/",
                    "url": "https://example.com/rss/",
                }),
                schedule: Some("@daily".into()),
                enabled: true,
            })
            .await
            .unwrap();

        crate::jobs::CronJob::reconcile(&services).await.unwrap();
        assert_eq!(armed(&services).await.len(), 1);

        import_configured_workflows(&services).await.unwrap();

        let armed = armed(&services).await;
        assert_eq!(armed.len(), 1);
        assert_eq!(armed[0].payload.workflow, Some(existing.id));
    }

    #[tokio::test]
    async fn one_unimportable_entry_does_not_prevent_the_others_being_imported() {
        // A configuration file is written by hand, and the failure worth
        // defending against is the one where a single bad entry takes everything
        // else with it. Somebody upgrading should lose the entry they got wrong,
        // not the twenty they got right.
        let services = ServicesContainer::new_mock().await.unwrap();
        let store = WorkflowStore::new(&services);

        let entries = vec![
            half_broken_entry("First", Some("https://example.com/first")),
            half_broken_entry("Cannot be imported", None),
            half_broken_entry("Third", Some("https://example.com/third")),
        ];

        let imported = import_entries(&store, &entries).await.unwrap();

        assert_eq!(imported, 2, "only the entries that were stored are counted");

        let mut names: Vec<String> = store
            .records()
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.config["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();

        assert_eq!(names, vec!["First", "Third"]);
    }

    fn half_broken_entry(name: &str, url: Option<&str>) -> CronJobConfig<HalfBrokenWorkflow> {
        CronJobConfig {
            job: HalfBrokenJob {
                name: name.to_string(),
                url: url.map(|url| url.to_string()),
            },
            cron: croner::Cron::from_str("@daily").unwrap(),
        }
    }

    /// A workflow type whose stored configuration asks for more than the payload
    /// its handler is given.
    ///
    /// [`ConfigurableWorkflow::ConfigType`] is deliberately allowed to differ
    /// from [`Job::JobType`], so an entry can serialize perfectly well and still
    /// be something the store refuses to write down. None of the six real types
    /// can produce that — their two types are the same, so anything the file
    /// parsed into is by construction something the store accepts — which is why
    /// the case is built here rather than borrowed from one of them.
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct HalfBrokenJob {
        name: String,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    }

    impl std::fmt::Display for HalfBrokenJob {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "half-broken/{}", self.name)
        }
    }

    /// The stored shape, which insists on the URL the payload treats as optional.
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct HalfBrokenConfig {
        name: String,
        url: String,
    }

    struct HalfBrokenWorkflow;

    crate::register_job!(HalfBrokenWorkflow);
    crate::register_workflow_type!(HalfBrokenWorkflow);

    impl Job for HalfBrokenWorkflow {
        type JobType = HalfBrokenJob;

        fn partition() -> &'static str {
            "test/half-broken"
        }

        async fn handle(
            &self,
            _ctx: JobContext<impl Services + Send + Sync + 'static>,
            _job: &Self::JobType,
        ) -> Result<(), Error> {
            Ok(())
        }
    }

    impl ConfigurableWorkflow for HalfBrokenWorkflow {
        type ConfigType = HalfBrokenConfig;

        fn type_id() -> &'static str {
            "test-half-broken"
        }

        fn describe(config: &Self::ConfigType) -> String {
            config.name.clone()
        }

        fn descriptor() -> automate_api::WorkflowTypeDescriptor {
            use automate_api::{
                FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor,
            };

            WorkflowTypeDescriptor {
                id: <Self as ConfigurableWorkflow>::type_id().to_string(),
                name: "Half Broken".to_string(),
                description: "Exists only so that a refused entry can be tested.".to_string(),
                documentation: "## Not a real workflow\n\nThis type exists so that a stored configuration the handler refuses can be tested.".to_string(),
                trigger: WorkflowTrigger::Cron {
                    default_schedule: "@daily".to_string(),
                },
                fields: vec![
                    FieldDescriptor::new(
                        crate::config_path!(HalfBrokenConfig: name),
                        "Name",
                        FieldKind::Text {
                            placeholder: Some("Example".into()),
                        },
                    )
                    .required(),
                    FieldDescriptor::new(
                        crate::config_path!(HalfBrokenConfig: url),
                        "URL",
                        FieldKind::Url {
                            placeholder: Some("https://example.com/".into()),
                        },
                    )
                    .required(),
                ],
            }
        }
    }
}
