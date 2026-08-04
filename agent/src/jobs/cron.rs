use std::{fmt::Display, str::FromStr};

use chrono::Utc;

use crate::prelude::*;

/// The partition holding both cron workflow records and their armed schedules.
pub const CRON_PARTITION: &str = "cron";

#[derive(serde::Deserialize, Clone)]
pub struct CronJobConfig<J: Job> {
    #[serde(flatten)]
    pub job: J::JobType,

    pub cron: croner::Cron,
}

impl<J> Default for CronJobConfig<J>
where
    J: Job,
    J::JobType: Default,
{
    fn default() -> Self {
        CronJobConfig {
            job: J::JobType::default(),
            cron: croner::Cron::from_str("@hourly").unwrap(), // Default to hourly
        }
    }
}

/// A schedule, as it sits in the queue waiting for its next fire time.
///
/// # Two shapes, for as long as it takes
///
/// A schedule used to carry the whole configuration it was going to run, which
/// worked while configurations came from a file that only changed on restart.
/// A stored workflow can be edited at any time, so a copy in the queue is a
/// copy that goes stale; `workflow` names the record instead, and the
/// configuration is read at the moment it is needed.
///
/// `task` is what the copied form used, and is still populated for the workflow
/// types that have not been converted to stored records yet. Both are accepted
/// so that an installation upgrades without its existing schedules being
/// dropped on the floor. When the last type is converted, `task` goes.
#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct CronJobTask {
    pub cron: croner::Cron,

    /// The queue partition this dispatches into when it fires.
    pub kind: String,

    pub idempotency_key: Option<String>,

    /// The configuration to run, for schedules that carry their own.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub task: serde_json::Value,

    /// The stored workflow to run, for schedules that name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<automate_api::WorkflowId>,
}

impl Display for CronJobTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl<J: Job> From<&CronJobConfig<J>> for CronJobTask
where
    J::JobType: serde::Serialize + Display,
{
    fn from(config: &CronJobConfig<J>) -> Self {
        CronJobTask {
            cron: config.cron.clone(),
            kind: J::partition().to_string(),
            idempotency_key: Some(format!("{}", config.job)),
            task: serde_json::to_value(&config.job).unwrap(),
            workflow: None,
        }
    }
}

pub struct CronJob;

impl CronJob {
    #[instrument("cron_job.schedule", skip(jobs, services), fields(otel.kind=?OpenTelemetrySpanKind::Producer, job.kind = std::any::type_name::<J::JobType>()))]
    pub async fn schedule<J: Job>(
        jobs: &[CronJobConfig<J>],
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error>
    where
        J::JobType: serde::Serialize + Display,
    {
        for job in jobs.iter() {
            let job: CronJobTask = job.into();
            let idempotency_key = job.idempotency_key.clone().map(|k| k.into());

            Self::dispatch(job, idempotency_key, &services).await?;
        }

        Ok(())
    }
}

/// The most schedules we will look at in one reconciliation pass.
///
/// A tenant with more armed schedules than this has something wrong with it, and
/// silently walking an unbounded list at every startup is how that turns into a
/// slow start nobody can explain.
const MAX_SCHEDULES: usize = 1000;

impl CronJob {
    /// Brings a tenant's armed schedules into line with the workflows it has.
    ///
    /// Configuration used to be a file, so scheduling was a one-way push: read
    /// the file, enqueue what it said. Nothing ever removed a schedule, because
    /// nothing knew a workflow had gone — deleting an entry left it ticking
    /// until somebody purged the row by hand. Records can be created and deleted
    /// while running, so this compares the two and fixes the difference.
    ///
    /// # Why an armed schedule is left alone
    ///
    /// Enqueueing upserts, and an upsert resets the row's fire time to now. So
    /// re-arming everything at startup would fire every workflow immediately,
    /// which is exactly what the old push did — a restart ran the lot. Anything
    /// already armed is therefore skipped, and only the difference is written.
    #[instrument("cron_job.reconcile", skip(services), err(Display))]
    pub async fn reconcile(
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let stored: Vec<(String, crate::workflow_store::WorkflowRecord)> =
            services.kv().list(CRON_PARTITION).await?;

        let armed: Vec<crate::db::PeekedMessage<CronJobTask>> =
            services.queue().peek(CRON_PARTITION, MAX_SCHEDULES).await?;

        let already_armed: std::collections::HashSet<automate_api::WorkflowId> = armed
            .iter()
            .filter_map(|message| message.payload.workflow)
            .collect();

        let known: std::collections::HashSet<automate_api::WorkflowId> =
            stored.iter().map(|(_, record)| record.id).collect();

        let now = Utc::now();
        let (mut added, mut removed) = (0usize, 0usize);

        for (_, record) in &stored {
            if already_armed.contains(&record.id) {
                continue;
            }

            let Some(schedule) = record.schedule.as_deref() else {
                continue;
            };

            let cron = match croner::Cron::from_str(schedule) {
                Ok(cron) => cron,
                Err(err) => {
                    // Refusing to start over one bad row would let a single
                    // unparseable schedule take down everybody else's.
                    warn!(
                        workflow.id = %record.id,
                        workflow.schedule = schedule,
                        "Leaving a workflow unscheduled because its schedule will not parse: {err}",
                    );
                    continue;
                }
            };

            let Ok(next_run) = cron.find_next_occurrence(&now, false) else {
                warn!(
                    workflow.id = %record.id,
                    workflow.schedule = schedule,
                    "Leaving a workflow unscheduled because its schedule will never fire again.",
                );
                continue;
            };

            services
                .queue()
                .enqueue(
                    CRON_PARTITION,
                    CronJobTask {
                        cron,
                        // Carried so the payload still says where it is going if
                        // it is read by something that does not resolve the
                        // workflow, and unused on the path that does.
                        kind: crate::workflows::lookup(&record.type_id)?
                            .partition()
                            .to_string(),
                        idempotency_key: Some(record.id.to_string()),
                        task: serde_json::Value::Null,
                        workflow: Some(record.id),
                    },
                    Some(record.id.to_string().into()),
                    Some(next_run - now),
                )
                .await?;

            added += 1;
        }

        for message in &armed {
            // Only schedules naming a workflow are ours to remove. The ones
            // still carrying their own configuration come from the file, and
            // there is no record to compare them against.
            let Some(id) = message.payload.workflow else {
                continue;
            };

            if known.contains(&id) {
                continue;
            }

            services
                .queue()
                .purge(CRON_PARTITION, message.key.clone())
                .await?;
            removed += 1;
        }

        if added > 0 || removed > 0 {
            info!(
                schedules.added = added,
                schedules.removed = removed,
                "Reconciled workflow schedules.",
            );
        }

        Ok(())
    }
}

crate::register_job!(CronJob);

impl Job for CronJob {
    type JobType = CronJobTask;

    fn partition() -> &'static str {
        CRON_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    #[instrument("workflow.cron.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let now = Utc::now();

        // Everything is resolved before the schedule is re-armed, because the
        // schedule itself may have been edited since this was queued. Re-arming
        // first would mean using the copy in the payload and applying the edit
        // one run later than it was made.
        let record = match job.workflow {
            Some(id) => {
                let store = crate::workflow_store::WorkflowStore::new(&services);

                let Some(record) = store.find(id).await? else {
                    // Returning without re-enqueueing is what removes the
                    // schedule: the consumer completes this message on success,
                    // so a deleted workflow stops ticking of its own accord
                    // rather than needing anything to go and tidy up.
                    info!(
                        workflow.id = %id,
                        "The workflow this schedule ran has been deleted, so the schedule ends here.",
                    );
                    return Ok(());
                };

                Some(record)
            }
            None => None,
        };

        let cron = match &record {
            Some(record) => {
                let Some(schedule) = record.schedule.as_deref() else {
                    warn!(
                        workflow.id = %record.id,
                        "A scheduled workflow has no schedule, so it will not be run again.",
                    );
                    return Ok(());
                };

                croner::Cron::from_str(schedule).wrap_user_err(
                    format!("'{schedule}' is not a schedule we could understand."),
                    &["Edit this workflow and give it a schedule such as '@daily'."],
                )?
            }
            None => job.cron.clone(),
        };

        let next_run = cron.find_next_occurrence(&now, false).wrap_user_err(
            "We could not determine the next time at which this cron job should be dispatched.",
            &["Please ensure the cron schedule is valid."],
        )?;

        // Re-arm before dispatching. A schedule that is armed and whose run then
        // fails is a run that gets retried; a run that succeeds and then fails to
        // re-arm is a workflow that silently stops.
        services
            .queue()
            .enqueue(
                "cron",
                CronJobTask {
                    cron: cron.clone(),
                    ..job.clone()
                },
                job.idempotency_key.as_ref().map(|k| k.clone().into()),
                Some(next_run - now),
            )
            .await?;

        let (partition, config) = match record {
            Some(record) => {
                if !record.enabled {
                    // The schedule stays armed so that unpausing takes effect on
                    // its own, without waiting for a restart to notice.
                    debug!(workflow.id = %record.id, "Skipping a paused workflow.");
                    return Ok(());
                }

                let store = crate::workflow_store::WorkflowStore::new(&services);
                store.mark_run(record.id, now).await?;

                (
                    crate::workflows::lookup(&record.type_id)?
                        .partition()
                        .to_string(),
                    record.config,
                )
            }
            None => (job.kind.clone(), job.task.clone()),
        };

        // Enqueue the actual task to be run immediately
        services
            .queue()
            .enqueue(
                partition,
                config,
                job.idempotency_key.clone().map(|k| k.into()),
                None,
            )
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    fn draft() -> WorkflowDraft {
        WorkflowDraft {
            type_id: "rss".into(),
            config: serde_json::json!({
                "name": "Citation Needed",
                "url": "https://example.com/rss/",
                "homepage": "https://example.com/",
            }),
            schedule: Some("@daily".into()),
            enabled: true,
        }
    }

    async fn armed(
        services: &(impl Services + Send + Sync + 'static),
    ) -> Vec<crate::db::PeekedMessage<CronJobTask>> {
        services.queue().peek(CRON_PARTITION, 100).await.unwrap()
    }

    #[tokio::test]
    async fn a_stored_workflow_gets_a_schedule() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = WorkflowStore::new(&services).create(draft()).await.unwrap();

        CronJob::reconcile(&services).await.unwrap();

        let armed = armed(&services).await;
        assert_eq!(armed.len(), 1);
        assert_eq!(armed[0].payload.workflow, Some(workflow.id));
        assert_eq!(
            armed[0].key,
            workflow.id.to_string(),
            "a schedule is keyed by the workflow it runs, which is what lets it be found and removed",
        );
    }

    #[tokio::test]
    async fn reconciling_twice_does_not_bring_a_schedule_forward() {
        // Enqueueing upserts, and an upsert resets the fire time to now. If
        // reconciliation re-armed what is already armed, every restart would run
        // every workflow immediately, which is the behaviour this replaced.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        WorkflowStore::new(&services).create(draft()).await.unwrap();

        CronJob::reconcile(&services).await.unwrap();
        let first = armed(&services).await[0].hidden_until;

        CronJob::reconcile(&services).await.unwrap();
        let second = armed(&services).await;

        assert_eq!(second.len(), 1, "reconciling twice should not arm it twice");
        assert_eq!(
            first, second[0].hidden_until,
            "an armed schedule should be left exactly as it was found",
        );
    }

    #[tokio::test]
    async fn a_schedule_is_not_armed_to_fire_immediately() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        WorkflowStore::new(&services).create(draft()).await.unwrap();

        CronJob::reconcile(&services).await.unwrap();

        assert!(
            armed(&services).await[0].hidden_until > Utc::now(),
            "a workflow should first run at its next scheduled time, not because the agent restarted",
        );
    }

    #[tokio::test]
    async fn the_schedule_of_a_deleted_workflow_is_removed() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();
        assert_eq!(armed(&services).await.len(), 1);

        store.delete(workflow.id).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        assert!(
            armed(&services).await.is_empty(),
            "deleting a workflow should stop it running, which the old push never did",
        );
    }

    #[tokio::test]
    async fn a_paused_workflow_keeps_its_schedule() {
        // So that unpausing takes effect on its own rather than waiting for a
        // restart to notice.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        store
            .update(
                workflow.id,
                WorkflowDraft {
                    enabled: false,
                    ..draft()
                },
            )
            .await
            .unwrap();
        CronJob::reconcile(&services).await.unwrap();

        assert_eq!(armed(&services).await.len(), 1);
    }

    /// Fires an armed schedule the way the consumer would, and returns what is
    /// armed afterwards.
    async fn run_once(
        services: &(impl Services + Send + Sync + Clone + 'static),
    ) -> Vec<crate::db::PeekedMessage<CronJobTask>> {
        let pending = armed(services).await;
        let task = pending[0].payload.clone();

        run_task(services, &task).await;

        armed(services).await
    }

    /// Runs one schedule the way the consumer would.
    async fn run_task(
        services: &(impl Services + Send + Sync + Clone + 'static),
        task: &CronJobTask,
    ) {
        CronJob
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                task,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn editing_a_schedule_takes_effect_on_the_next_run_not_the_one_after() {
        // The armed message carries a copy of the schedule. If a run re-armed
        // itself from that copy rather than from the record, an edit would be
        // skipped over exactly once, which is the kind of thing that gets
        // written off as "it needed a restart".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        store
            .update(
                workflow.id,
                WorkflowDraft {
                    schedule: Some("@hourly".into()),
                    ..draft()
                },
            )
            .await
            .unwrap();

        let armed = run_once(&services).await;

        assert_eq!(armed.len(), 1);
        assert!(
            armed[0].hidden_until < Utc::now() + chrono::TimeDelta::hours(2),
            "the run should have re-armed itself hourly, as the workflow now says, rather than daily",
        );
    }

    #[tokio::test]
    async fn a_run_records_when_it_happened() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();
        run_once(&services).await;

        assert!(
            store.get(workflow.id).await.unwrap().last_run.is_some(),
            "a workflow should be able to say when it last ran",
        );
    }

    #[tokio::test]
    async fn a_run_dispatches_the_configuration_as_it_stands_now() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        store
            .update(
                workflow.id,
                WorkflowDraft {
                    config: serde_json::json!({
                        "name": "Renamed Since Scheduling",
                        "url": "https://example.com/rss/",
                        "homepage": "https://example.com/",
                    }),
                    ..draft()
                },
            )
            .await
            .unwrap();

        run_once(&services).await;

        let dispatched: Vec<crate::db::PeekedMessage<serde_json::Value>> =
            services.queue().peek("rss/todoist", 10).await.unwrap();

        assert_eq!(dispatched.len(), 1);
        assert_eq!(
            dispatched[0].payload["name"], "Renamed Since Scheduling",
            "the run should use the workflow as it stands, not the copy taken when it was scheduled",
        );
    }

    #[tokio::test]
    async fn a_workflow_deleted_while_its_run_was_pending_stops_there() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        let pending = armed(&services).await;
        let (task, armed_until) = (pending[0].payload.clone(), pending[0].hidden_until);
        store.delete(workflow.id).await.unwrap();

        run_task(&services, &task).await;

        // The consumer completes the message it handed over once the run reports
        // success, and completing removes the row. So the schedule ends by this
        // run declining to write a new one; the row still present here is the
        // one being handled, which the consumer is about to take away.
        let still_armed = armed(&services).await;
        assert_eq!(
            still_armed[0].hidden_until, armed_until,
            "a deleted workflow should not have re-armed itself for another run",
        );

        let dispatched: Vec<crate::db::PeekedMessage<serde_json::Value>> =
            services.queue().peek("rss/todoist", 10).await.unwrap();
        assert!(
            dispatched.is_empty(),
            "a deleted workflow should not have run"
        );
    }

    #[tokio::test]
    async fn a_paused_workflow_stays_armed_but_does_not_run() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);

        let workflow = store.create(draft()).await.unwrap();
        CronJob::reconcile(&services).await.unwrap();

        store
            .update(
                workflow.id,
                WorkflowDraft {
                    enabled: false,
                    ..draft()
                },
            )
            .await
            .unwrap();

        let before = armed(&services).await[0].hidden_until;
        let after = run_once(&services).await;

        assert_eq!(after.len(), 1, "a paused workflow keeps its schedule");
        assert!(
            after[0].hidden_until > before,
            "a paused workflow should re-arm, so that unpausing takes effect without a restart",
        );

        let dispatched: Vec<crate::db::PeekedMessage<serde_json::Value>> =
            services.queue().peek("rss/todoist", 10).await.unwrap();
        assert!(
            dispatched.is_empty(),
            "a paused workflow should not have run"
        );
    }

    #[tokio::test]
    async fn a_schedule_carrying_its_own_configuration_is_left_alone() {
        // Workflows that still come from the configuration file have no record
        // to compare against, so reconciliation must not treat them as orphans.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();

        services
            .queue()
            .enqueue(
                CRON_PARTITION,
                CronJobTask {
                    cron: croner::Cron::from_str("@daily").unwrap(),
                    kind: "rss/todoist".into(),
                    idempotency_key: Some("rss/From The File".into()),
                    task: serde_json::json!({ "name": "From The File" }),
                    workflow: None,
                },
                Some("rss/From The File".into()),
                None,
            )
            .await
            .unwrap();

        CronJob::reconcile(&services).await.unwrap();

        let armed = armed(&services).await;
        assert_eq!(armed.len(), 1);
        assert!(armed[0].payload.workflow.is_none());
    }

    #[tokio::test]
    async fn one_unparseable_schedule_does_not_stop_the_others_being_armed() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store = WorkflowStore::new(&services);
        let good = store.create(draft()).await.unwrap();

        // Written directly, because the store refuses a schedule like this; it
        // is what an older or hand-edited record could hold.
        let mut broken = store.get(good.id).await.unwrap();
        broken.id = automate_api::WorkflowId::from_entropy(9);
        broken.schedule = Some("every other tuesday".into());
        services
            .kv()
            .set(CRON_PARTITION, broken.id.to_string(), broken.clone())
            .await
            .unwrap();

        CronJob::reconcile(&services).await.unwrap();

        let armed = armed(&services).await;
        assert_eq!(
            armed.len(),
            1,
            "the workflow with a good schedule should still have been armed",
        );
        assert_eq!(armed[0].payload.workflow, Some(good.id));
    }
}
