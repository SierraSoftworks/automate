//! The workflows a user has configured.
//!
//! A workflow is one instance of a [`crate::workflows::WorkflowType`]: a feed
//! somebody chose to watch, together with the schedule they chose and the
//! account they file tasks into. The type says what such a thing needs; a
//! record here says what one person asked for.
//!
//! # Where a configuration lives
//!
//! Records are stored under the partition their trigger names — `cron`, or
//! `webhooks/github` — mirroring the queue partition that trigger dispatches
//! into. This costs something: finding a workflow by identifier alone means
//! looking in each partition, because the identifier does not say which one it
//! is in.
//!
//! It buys the thing that matters more. The reconciler that keeps schedules in
//! step reads `cron` and gets exactly the workflows it is responsible for, with
//! no filtering and no way to accidentally reschedule a webhook. A single
//! `workflows` partition would make the lookup direct and make every consumer
//! filter, which is the same work moved somewhere it is easier to get wrong.
//! The number of partitions is the number of trigger kinds, so the scan is over
//! a handful of lists rather than a growing one.

use chrono::{DateTime, Utc};

use automate_api::{Workflow, WorkflowId, WorkflowTrigger};

use human_errors::Error;

use crate::db::KeyValueStore;
use crate::prelude::*;
use crate::workflows;

/// How many times to retry a randomly generated identifier before giving up.
///
/// Three words is eight billion identifiers per tenant and a tenant holds
/// perhaps dozens of workflows, so a second attempt is already vanishingly
/// unlikely; this exists so that a bug cannot become an infinite loop.
const ID_ATTEMPTS: usize = 8;

/// A workflow as stored.
///
/// Distinct from [`Workflow`], which is what the API returns. This holds what
/// was written down; that additionally carries what can be worked out — the
/// display name, which its type derives from the configuration, and the next
/// run, which the schedule implies. Deriving them on read means they cannot go
/// stale, and means a workflow type can change how it names itself without a
/// migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowRecord {
    pub id: WorkflowId,

    #[serde(rename = "type")]
    pub type_id: String,

    pub config: serde_json::Value,

    /// The schedule, for workflows triggered by cron.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
}

fn default_enabled() -> bool {
    true
}

/// What a caller supplies to create or replace a workflow.
///
/// Deliberately not [`WorkflowRecord`]: a caller has no business setting the
/// identifier, the timestamps, or when the workflow last ran, and a type that
/// let them would need every write path to remember to overwrite those.
#[derive(Debug, Clone)]
pub struct WorkflowDraft {
    pub type_id: String,
    pub config: serde_json::Value,
    pub schedule: Option<String>,
    pub enabled: bool,
}

/// Reads and writes the workflows belonging to one tenant.
pub struct WorkflowStore<S> {
    services: S,
}

impl<S: Services> WorkflowStore<S> {
    pub fn new(services: S) -> Self {
        Self { services }
    }

    /// Every partition that could hold a workflow, one per distinct trigger.
    fn partitions() -> Vec<String> {
        let mut partitions: Vec<String> = workflows::registry()
            .values()
            .map(|workflow| workflow.descriptor().trigger.partition())
            .collect();
        partitions.sort();
        partitions.dedup();
        partitions
    }

    /// The partition holding workflows of the given type.
    fn partition_for(type_id: &str) -> Result<String, Error> {
        Ok(workflows::lookup(type_id)?.descriptor().trigger.partition())
    }

    /// Checks a draft against its type, and settles its schedule.
    ///
    /// A cron workflow without a schedule takes the type's default rather than
    /// being refused, so that the API can accept "watch this feed" without the
    /// caller having an opinion about polling frequency. A schedule that cannot
    /// be parsed is refused here rather than at the first run, where the failure
    /// would be a log line nobody is reading.
    fn vet(draft: &WorkflowDraft) -> Result<Option<String>, Error> {
        let workflow = workflows::lookup(&draft.type_id)?;
        workflow.validate(&draft.config)?;

        match workflow.descriptor().trigger {
            WorkflowTrigger::Cron { default_schedule } => {
                let schedule = draft.schedule.clone().unwrap_or(default_schedule);

                <croner::Cron as std::str::FromStr>::from_str(&schedule).map_err(|err| {
                    human_errors::user(
                        format!("'{schedule}' is not a schedule we could understand: {err}"),
                        &[
                            "Schedules are cron expressions, such as '0 */6 * * *' for every six hours.",
                            "The shorthands '@hourly', '@daily' and '@weekly' also work.",
                        ],
                    )
                })?;

                Ok(Some(schedule))
            }
            WorkflowTrigger::Webhook { .. } => Ok(None),
        }
    }

    /// Stores a new workflow, choosing an identifier for it.
    pub async fn create(&self, draft: WorkflowDraft) -> Result<Workflow, Error> {
        let schedule = Self::vet(&draft)?;
        let partition = Self::partition_for(&draft.type_id)?;
        let now = Utc::now();

        for _ in 0..ID_ATTEMPTS {
            let record = WorkflowRecord {
                id: WorkflowId::from_entropy(rand::random()),
                type_id: draft.type_id.clone(),
                config: draft.config.clone(),
                schedule: schedule.clone(),
                enabled: draft.enabled,
                created_at: now,
                updated_at: now,
                last_run: None,
            };

            if self
                .services
                .kv()
                .insert(partition.clone(), record.id.to_string(), record.clone())
                .await?
            {
                return self.present(record);
            }
        }

        Err(human_errors::system(
            "We could not find an unused identifier for this workflow.",
            &["Please try again, and report this if it keeps happening."],
        ))
    }

    /// Every workflow this tenant owns, most recently created first.
    pub async fn list(&self) -> Result<Vec<Workflow>, Error> {
        let mut workflows = Vec::new();

        for partition in Self::partitions() {
            let records: Vec<(String, WorkflowRecord)> = self.services.kv().list(partition).await?;
            for (_, record) in records {
                // A record whose type is no longer registered is reported rather
                // than hidden: its owner needs to know why it stopped, and a
                // silent disappearance looks like data loss.
                match self.present(record.clone()) {
                    Ok(workflow) => workflows.push(workflow),
                    Err(err) => {
                        warn!(
                            workflow.id = %record.id,
                            workflow.type = %record.type_id,
                            error = %err,
                            "Skipping a stored workflow whose type is no longer available: {err}",
                        );
                    }
                }
            }
        }

        workflows.sort_by_key(|workflow| std::cmp::Reverse(workflow.created_at));
        Ok(workflows)
    }

    /// Finds a workflow by identifier, wherever it is stored.
    pub async fn find(&self, id: WorkflowId) -> Result<Option<WorkflowRecord>, Error> {
        for partition in Self::partitions() {
            if let Some(record) = self
                .services
                .kv()
                .get::<WorkflowRecord>(partition, id.to_string())
                .await?
            {
                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    /// Finds a workflow, or reports that there is no such thing.
    pub async fn get(&self, id: WorkflowId) -> Result<WorkflowRecord, Error> {
        self.find(id).await?.ok_or_else(|| {
            human_errors::user(
                format!("There is no workflow called '{id}'."),
                &[
                    "Check the identifier, or list your workflows to see what you have.",
                    "It may have been deleted.",
                ],
            )
        })
    }

    /// Replaces a workflow's configuration.
    ///
    /// The type is fixed at creation: changing it would keep the identifier
    /// while replacing everything the identifier referred to, which is a new
    /// workflow wearing an old name.
    pub async fn update(&self, id: WorkflowId, draft: WorkflowDraft) -> Result<Workflow, Error> {
        let existing = self.get(id).await?;

        if existing.type_id != draft.type_id {
            return Err(human_errors::user(
                format!(
                    "This workflow is a '{}' and cannot be changed into a '{}'.",
                    existing.type_id, draft.type_id
                ),
                &["Delete this workflow and create one of the type you want instead."],
            ));
        }

        let schedule = Self::vet(&draft)?;

        let record = WorkflowRecord {
            config: draft.config,
            schedule,
            enabled: draft.enabled,
            updated_at: Utc::now(),
            ..existing
        };

        self.services
            .kv()
            .set(
                Self::partition_for(&record.type_id)?,
                record.id.to_string(),
                record.clone(),
            )
            .await?;

        self.present(record)
    }

    /// Records that a workflow just ran.
    ///
    /// Missing records are ignored rather than reported: a run that finished
    /// after its workflow was deleted is a race, not a fault, and failing the
    /// run would retry work that has nowhere to be recorded.
    pub async fn mark_run(&self, id: WorkflowId, at: DateTime<Utc>) -> Result<(), Error> {
        let Some(existing) = self.find(id).await? else {
            return Ok(());
        };

        let partition = Self::partition_for(&existing.type_id)?;
        let record = WorkflowRecord {
            last_run: Some(at),
            ..existing
        };

        self.services
            .kv()
            .set(partition, id.to_string(), record)
            .await
    }

    /// Removes a workflow.
    ///
    /// This leaves its pending schedule behind; the reconciler is what clears
    /// that up, so that a deletion and a restart converge on the same state
    /// rather than deletion having its own half of the logic to get wrong.
    pub async fn delete(&self, id: WorkflowId) -> Result<(), Error> {
        let existing = self.get(id).await?;
        self.services
            .kv()
            .remove(Self::partition_for(&existing.type_id)?, id.to_string())
            .await
    }

    /// Turns a stored record into what the API returns, for the callers that
    /// already hold one.
    pub fn present_record(&self, record: WorkflowRecord) -> Result<Workflow, Error> {
        self.present(record)
    }

    /// Turns a stored record into what the API returns, working out the parts
    /// that are derived rather than written down.
    fn present(&self, record: WorkflowRecord) -> Result<Workflow, Error> {
        let workflow = workflows::lookup(&record.type_id)?;

        let next_run = record
            .enabled
            .then_some(record.schedule.as_deref())
            .flatten()
            .and_then(next_occurrence);

        Ok(Workflow {
            id: record.id,
            name: workflow.describe(&record.config)?,
            type_id: record.type_id,
            enabled: record.enabled,
            config: record.config,
            schedule: record.schedule,
            created_at: record.created_at,
            updated_at: record.updated_at,
            last_run: record.last_run,
            next_run,
        })
    }
}

/// When a schedule next fires, or `None` if it never will again.
///
/// A schedule is checked when it is saved, so one that will not parse here has
/// been edited underneath us; that is worth a line in the log but not worth
/// refusing to show the workflow.
pub fn next_occurrence(schedule: &str) -> Option<DateTime<Utc>> {
    match <croner::Cron as std::str::FromStr>::from_str(schedule) {
        Ok(cron) => cron.find_next_occurrence(&Utc::now(), false).ok(),
        Err(err) => {
            warn!(
                workflow.schedule = schedule,
                "Ignoring a stored schedule which no longer parses: {err}",
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration the RSS workflow accepts, for the tests that are about
    /// the store rather than about validation.
    fn valid_config() -> serde_json::Value {
        serde_json::json!({
            "name": "Citation Needed",
            "url": "https://example.com/rss/",
            "homepage": "https://example.com/",
        })
    }

    fn draft(schedule: Option<&str>) -> WorkflowDraft {
        WorkflowDraft {
            type_id: "rss".into(),
            config: valid_config(),
            schedule: schedule.map(|s| s.to_string()),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn a_created_workflow_is_named_by_its_type_from_its_own_configuration() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft(Some("@daily"))).await.unwrap();

        assert_eq!(workflow.name, "Citation Needed");
        assert_eq!(workflow.type_id, "rss");
        assert!(workflow.enabled);
    }

    #[tokio::test]
    async fn a_cron_workflow_saved_without_a_schedule_takes_its_types_default() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft(None)).await.unwrap();

        assert_eq!(workflow.schedule.as_deref(), Some("@daily"));
        assert!(
            workflow.next_run.is_some(),
            "a scheduled workflow should be able to say when it next runs",
        );
    }

    #[tokio::test]
    async fn a_schedule_nobody_could_run_is_refused_when_it_is_saved() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let Err(err) = store.create(draft(Some("every other tuesday"))).await else {
            panic!("a schedule that cannot be parsed should not be stored");
        };

        assert!(
            format!("{err}").contains("every other tuesday"),
            "the error should quote the schedule that was rejected: {err}",
        );
    }

    #[tokio::test]
    async fn a_configuration_the_handler_could_not_read_is_refused_when_it_is_saved() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let broken = WorkflowDraft {
            config: serde_json::json!({ "name": "Missing its feed" }),
            ..draft(None)
        };

        assert!(
            store.create(broken).await.is_err(),
            "a workflow whose configuration will not load should not be stored",
        );
    }

    #[tokio::test]
    async fn a_paused_workflow_keeps_its_configuration_but_stops_expecting_to_run() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let created = store.create(draft(Some("@daily"))).await.unwrap();
        let paused = store
            .update(
                created.id,
                WorkflowDraft {
                    enabled: false,
                    ..draft(Some("@daily"))
                },
            )
            .await
            .unwrap();

        assert!(!paused.enabled);
        assert_eq!(paused.config, valid_config());
        assert!(
            paused.next_run.is_none(),
            "a paused workflow has no next run to report",
        );
    }

    #[tokio::test]
    async fn a_workflow_cannot_be_turned_into_a_different_type() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let created = store.create(draft(None)).await.unwrap();

        let Err(err) = store
            .update(
                created.id,
                WorkflowDraft {
                    type_id: "something-else".into(),
                    ..draft(None)
                },
            )
            .await
        else {
            panic!("changing a workflow's type keeps the name while replacing what it names");
        };

        assert!(format!("{err}").contains("rss"), "{err}");
    }

    #[tokio::test]
    async fn each_created_workflow_gets_its_own_identifier() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let first = store.create(draft(None)).await.unwrap();
        let second = store.create(draft(None)).await.unwrap();

        assert_ne!(
            first.id, second.id,
            "two workflows sharing an identifier would overwrite one another",
        );

        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[tokio::test]
    async fn a_deleted_workflow_is_gone_and_saying_so_twice_is_an_error() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let created = store.create(draft(None)).await.unwrap();
        store.delete(created.id).await.unwrap();

        assert!(store.find(created.id).await.unwrap().is_none());
        assert!(
            store.delete(created.id).await.is_err(),
            "deleting something that is not there should say so rather than succeed quietly",
        );
    }

    #[tokio::test]
    async fn a_run_that_finishes_after_its_workflow_was_deleted_is_not_an_error() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let created = store.create(draft(None)).await.unwrap();
        store.delete(created.id).await.unwrap();

        // The run was already in flight when the workflow was removed. Failing
        // here would retry work that has nowhere left to be recorded.
        store.mark_run(created.id, Utc::now()).await.unwrap();
    }

    #[tokio::test]
    async fn a_workflow_remembers_when_it_last_ran() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let created = store.create(draft(None)).await.unwrap();
        let ran_at = Utc::now();
        store.mark_run(created.id, ran_at).await.unwrap();

        let stored = store.get(created.id).await.unwrap();
        assert_eq!(
            stored.last_run.map(|at| at.timestamp()),
            Some(ran_at.timestamp()),
        );
    }

    #[tokio::test]
    async fn a_stored_workflow_whose_type_is_gone_is_left_out_rather_than_failing_the_list() {
        let services = crate::testing::mock_services().await.unwrap();
        let store = WorkflowStore::new(&services);

        let good = store.create(draft(None)).await.unwrap();

        // Written directly, as an upgrade which removed a workflow type would
        // leave behind. One unreadable record must not make the others
        // unreachable.
        services
            .kv()
            .set(
                "cron",
                "orphan",
                WorkflowRecord {
                    id: WorkflowId::from_entropy(7),
                    type_id: "a-type-that-was-removed".into(),
                    config: serde_json::json!({}),
                    schedule: Some("@daily".into()),
                    enabled: true,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    last_run: None,
                },
            )
            .await
            .unwrap();

        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, good.id);
    }
}
