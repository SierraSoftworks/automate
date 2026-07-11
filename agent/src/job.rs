use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, TimeDelta, Utc};

use crate::db::QueueMessage;
use crate::prelude::*;
use crate::services::AppServices;

/// Contextual information about the job being processed, derived from the
/// original [`QueueMessage`] that triggered it.
///
/// In addition to providing access to the application [`Services`], this
/// carries metadata about the originally enqueued message - most importantly
/// the [`JobContext::scheduled_at`] timestamp. For webhook-backed jobs this is
/// the time at which the request was received, which allows time-sensitive
/// validation (such as webhook signature timestamp checks) to be performed
/// against the original receipt time rather than the current time, so that
/// retries continue to succeed.
pub struct JobContext<S>
where
    S: Services + Send + Sync + 'static,
{
    services: S,
    scheduled_at: DateTime<Utc>,
    #[allow(dead_code)]
    traceparent: Option<String>,
    #[allow(dead_code)]
    tracestate: Option<String>,
}

impl<S> JobContext<S>
where
    S: Services + Send + Sync + 'static,
{
    pub fn new(
        services: S,
        scheduled_at: DateTime<Utc>,
        traceparent: Option<String>,
        tracestate: Option<String>,
    ) -> Self {
        Self {
            services,
            scheduled_at,
            traceparent,
            tracestate,
        }
    }

    /// The application services available to the job.
    pub fn services(&self) -> &S {
        &self.services
    }

    /// Consumes the context, returning ownership of the services. Useful when a
    /// handler needs to pass owned services to a helper.
    pub fn into_services(self) -> S {
        self.services
    }

    /// The time at which the underlying message was originally enqueued.
    ///
    /// For webhook-backed jobs this is the time the request was received, which
    /// makes it the correct reference point for validating time-sensitive
    /// signatures even when a job is retried some time later.
    pub fn scheduled_at(&self) -> DateTime<Utc> {
        self.scheduled_at
    }

    /// The W3C `traceparent` associated with the originally enqueued message, if
    /// any.
    #[allow(dead_code)]
    pub fn traceparent(&self) -> Option<&str> {
        self.traceparent.as_deref()
    }

    /// The W3C `tracestate` associated with the originally enqueued message, if
    /// any.
    #[allow(dead_code)]
    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }
}

pub trait Job {
    type JobType: Serialize + DeserializeOwned + Send + 'static;

    #[instrument("job.dispatch", skip(job, idempotency_key, services), fields(otel.kind=?OpenTelemetrySpanKind::Producer, job.kind = std::any::type_name::<Self::JobType>()), err(Display))]
    async fn dispatch(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        services: &impl Services,
    ) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition());

        queue.enqueue(job, idempotency_key, None).await?;

        Ok(())
    }

    #[instrument("job.dispatch_delayed", skip(job, idempotency_key, delay, services), fields(otel.kind=?OpenTelemetrySpanKind::Producer, job.kind = std::any::type_name::<Self::JobType>()), err(Display))]
    async fn dispatch_delayed(
        job: Self::JobType,
        idempotency_key: Option<Cow<'static, str>>,
        delay: TimeDelta,
        services: &impl Services,
    ) -> Result<(), human_errors::Error> {
        let queue = services.queue().partition(Self::partition());

        queue.enqueue(job, idempotency_key, Some(delay)).await?;

        Ok(())
    }

    fn partition() -> &'static str;

    fn propagate_parent() -> bool {
        true
    }

    fn timeout(&self) -> TimeDelta {
        TimeDelta::minutes(5)
    }

    /// Performs any one-time startup work required by this job, such as
    /// scheduling recurring cron tasks. This is called once for every
    /// registered job when the [`JobConsumer`] starts up, mirroring the
    /// inventory-based registration used for job handlers.
    ///
    /// The default implementation does nothing, so jobs only need to override
    /// it when they require startup wiring.
    fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> impl std::future::Future<Output = Result<(), human_errors::Error>> + Send {
        async move {
            let _ = services;
            Ok(())
        }
    }

    fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> impl std::future::Future<Output = Result<(), human_errors::Error>> + Send;

    fn job_hash(&self, job: &Self::JobType) -> Result<String, human_errors::Error> {
        let serialized = serde_json::to_string(job).wrap_system_err(
            "Failed to serialize job for hashing.",
            &["Please report this issue to the dev team on GitHub."],
        )?;

        Ok(sha256::digest(serialized))
    }
}

/// An object-safe, type-erased view over a [`Job`] implementation.
///
/// This is what the [`JobConsumer`] dispatches to: it deserializes the raw
/// queue payload into the job's concrete `JobType` before delegating to
/// [`Job::handle`]. A blanket implementation is provided for every [`Job`], so
/// jobs only need to implement [`Job`] (and register themselves via
/// [`register_job!`]) to participate in dynamic dispatch.
#[async_trait::async_trait]
pub trait JobRunnable: Send + Sync {
    /// The queue partition this job consumes from. Also used as the routing key
    /// for dynamic dispatch.
    fn partition(&self) -> &'static str;

    /// Whether the parent trace context should be propagated into the job span.
    fn propagate_parent(&self) -> bool;

    /// How long a dequeued message should remain reserved while this job runs.
    fn timeout(&self) -> TimeDelta;

    /// Run any one-time startup work for the underlying [`Job::setup`].
    async fn setup(&self, services: AppServices) -> Result<(), human_errors::Error>;

    /// Deserialize the raw queue payload and run the underlying [`Job::handle`].
    async fn handle(
        &self,
        ctx: JobContext<AppServices>,
        payload: &serde_json::Value,
    ) -> Result<(), human_errors::Error>;
}

#[async_trait::async_trait]
impl<J> JobRunnable for J
where
    J: Job + Send + Sync + 'static,
{
    fn partition(&self) -> &'static str {
        <J as Job>::partition()
    }

    fn propagate_parent(&self) -> bool {
        <J as Job>::propagate_parent()
    }

    fn timeout(&self) -> TimeDelta {
        Job::timeout(self)
    }

    async fn setup(&self, services: AppServices) -> Result<(), human_errors::Error> {
        Job::setup(self, services).await
    }

    async fn handle(
        &self,
        ctx: JobContext<AppServices>,
        payload: &serde_json::Value,
    ) -> Result<(), human_errors::Error> {
        let job = <J::JobType as serde::Deserialize>::deserialize(payload).wrap_user_err(
            "Failed to deserialize the job payload into the type expected by its handler.",
            &[
                "This usually indicates a mismatch between the enqueued job and its registered handler.",
                "Please report this issue to the dev team on GitHub.",
            ],
        )?;

        Job::handle(self, ctx, &job).await
    }
}

/// A registration entry for a [`JobRunnable`], collected automatically via the
/// [`inventory`] crate. Use [`register_job!`] to submit one for a job.
pub struct JobRegistration(&'static dyn JobRunnable);

impl JobRegistration {
    pub const fn new<T: JobRunnable>(job: &'static T) -> Self {
        Self(job)
    }

    pub fn handler(&self) -> &'static dyn JobRunnable {
        self.0
    }
}

inventory::collect!(JobRegistration);

/// Registers a [`Job`] implementation so that it is automatically picked up by
/// the [`JobConsumer`]. The argument is a value of a unit job struct, e.g.
/// `register_job!(AzureMonitorWebhook);`.
#[macro_export]
macro_rules! register_job {
    ($job:expr) => {
        inventory::submit! { $crate::job::JobRegistration::new(&$job) }
    };
}

/// The single queue consumer responsible for processing every registered job.
///
/// It dequeues messages from any partition, looks up the matching handler in
/// the registry built from [`inventory`], and dispatches the work onto a
/// background task so that multiple jobs can run concurrently.
pub struct JobHost;

impl JobHost {
    #[instrument("job.host.run", skip(services), fields(otel.kind=?OpenTelemetrySpanKind::Consumer), err(Display))]
    pub async fn run(services: AppServices) -> Result<(), human_errors::Error> {
        let mut registry: HashMap<&'static str, &'static dyn JobRunnable> = HashMap::new();
        for registration in inventory::iter::<JobRegistration> {
            let handler = registration.handler();
            let partition = handler.partition();
            if registry.insert(partition, handler).is_some() {
                return Err(human_errors::user(
                    format!(
                        "Multiple job handlers are registered for the queue partition '{partition}'."
                    ),
                    &[
                        "Each job must use a unique partition.",
                        "Please report this issue to the dev team on GitHub.",
                    ],
                ));
            }
        }

        info!(
            "Job host started with {} registered handler(s).",
            registry.len()
        );

        // Run one-time startup wiring for every registered job (for example,
        // scheduling recurring cron tasks) before we begin processing work.
        for handler in registry.values() {
            handler.setup(services.clone()).await?;
        }

        // Reserve dequeued messages for at least as long as the slowest job may
        // take, so that a message is never released back to the queue while it
        // is still being processed. Once a message is dequeued and its handler
        // is known, `process` narrows this to the handler's own timeout.
        let reserve_for = registry
            .values()
            .map(|handler| handler.timeout())
            .max()
            .unwrap_or_else(|| TimeDelta::minutes(5));

        let queue = services.queue();
        let root_span = tracing::Span::current();

        // Track spawned job tasks so their lifetimes are bounded by the job
        // host rather than being detached. When the host is dropped (for
        // example because the web server shut down first), the set is dropped
        // and any in-flight jobs are aborted, releasing the `Services` clones
        // - and with them the `Arc<Session>` clones - they were holding. This
        // is what lets `main` reclaim sole ownership of the session to flush
        // telemetry on the way out.
        let mut tasks = tokio::task::JoinSet::new();

        loop {
            // Reap completed job tasks so the set does not grow without bound.
            while tasks.try_join_next().is_some() {}

            match queue.dequeue_any(reserve_for).await {
                Ok(item) => {
                    let Some(&handler) = registry.get(item.partition.as_str()) else {
                        warn!(
                            job.payload = %item.payload,
                            "No job handler is registered for partition '{}'; dropping the message. Payload: {}",
                            item.partition,
                            item.payload
                        );
                        services.session().record_event(
                            "job::missing-handler",
                            [
                                ("partition".to_string(), item.partition.clone()),
                            ].into()
                        );
                        if let Err(err) = queue.complete(item.partition.clone(), item).await {
                            error!(error = %err, "Failed to drop unhandled job message: {err}");
                            services.session().record_human_error(&err);
                        }
                        continue;
                    };

                    tasks.spawn(Self::process(
                        handler,
                        item,
                        services.clone(),
                        root_span.clone(),
                    ));
                }
                Err(err) => {
                    error!(error = %err, "An error occurred while fetching a job from the queue: {err}");
                    services.session().record_human_error(&err);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn process(
        handler: &'static dyn JobRunnable,
        item: QueueMessage<serde_json::Value>,
        services: AppServices,
        root_span: tracing::Span,
    ) {
        let queue = services.queue();
        let name = handler.partition();
        let delay = Utc::now() - item.scheduled_at;
        let session = services.session();

        // Narrow the generous dequeue reservation down to this job's own timeout.
        // The reservation window doubles as the retry backoff: if the job fails
        // (or the process dies) the message stays hidden for this long before it
        // becomes available again, which keeps rate-limited jobs from retrying
        // too aggressively.
        if let Err(err) = queue
            .reserve(
                item.partition.clone(),
                item.key.clone(),
                item.reservation_id.clone(),
                handler.timeout(),
            )
            .await
        {
            warn!(error = %err, "Failed to set the reservation window for job '{name}': {err}");
            session.record_human_error(&err);
        }

        let span = info_span!(
            parent: None,
            "job.run",
            job.name = name,
            job.delay = delay.num_milliseconds(),
            otel.kind = ?OpenTelemetrySpanKind::Consumer
        );
        span.follows_from(&root_span);

        let traceparent = item
            .traceparent
            .clone()
            .unwrap_or_else(|| "none".to_string());

        if item.traceparent.is_some() {
            let context = get_text_map_propagator(|p| p.extract(&item));

            if handler.propagate_parent() {
                if let Err(err) = span.set_parent(context) {
                    warn!(error = %err, "Failed to set trace context for job '{name}' (traceparent: {traceparent}): {err}");
                }
            } else {
                span.add_link(context.span().span_context().clone());
            }
        }

        debug!("Processing job '{name}' (traceparent: {traceparent}).");

        let ctx = JobContext::new(
            services.clone(),
            item.scheduled_at,
            item.traceparent.clone(),
            item.tracestate.clone(),
        );

        match handler
            .handle(ctx, &item.payload)
            .instrument(span.clone())
            .await
        {
            Ok(()) => {
                info!("Job '{name}' completed successfully (traceparent: {traceparent}).");
                if let Err(err) = queue.complete(name.to_string(), item).await {
                    error!(error = %err, "Failed to mark job '{name}' as completed (traceparent: {traceparent}): {err}");
                    session.record_human_error(&err);
                }
            }
            Err(err) => {
                if err.is(human_errors::Kind::System) {
                    session.record_human_error(&err);
                }

                // Record the failure against the job's own span (rather than the
                // long-lived consumer span) so that it is exported as part of the
                // trace, including an OpenTelemetry `exception` event.
                Self::record_job_failure(&span, &err);

                error!(error = %err, "An error occurred while processing job '{name}' (traceparent: {traceparent}): {err}");
            }
        }
    }

    /// Records a job failure against the supplied span following OpenTelemetry
    /// semantic conventions, attaching an `exception` event and marking the span
    /// status as an error so the failure is visible in exported traces.
    fn record_job_failure(span: &tracing::Span, err: &human_errors::Error) {
        let exception_type = if err.is(human_errors::Kind::System) {
            "SystemFailure"
        } else {
            "UserError"
        };

        span.add_event(
            "exception",
            vec![
                opentelemetry::KeyValue::new("exception.type", exception_type),
                opentelemetry::KeyValue::new("exception.message", err.to_string()),
                opentelemetry::KeyValue::new("exception.escaped", true),
            ],
        );
        span.set_status(opentelemetry::trace::Status::error(err.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::services::ServicesContainer;

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct TestPayload {
        id: String,
        value: String,
    }

    struct TestJob;

    impl Job for TestJob {
        type JobType = TestPayload;

        fn partition() -> &'static str {
            "test/dynamic-dispatch"
        }

        async fn setup(
            &self,
            services: impl Services + Send + Sync + 'static,
        ) -> Result<(), human_errors::Error> {
            services
                .kv()
                .set("test/dynamic-dispatch", "setup", "done".to_string())
                .await?;
            Ok(())
        }

        async fn handle(
            &self,
            ctx: JobContext<impl Services + Send + Sync + 'static>,
            job: &Self::JobType,
        ) -> Result<(), human_errors::Error> {
            ctx.services()
                .kv()
                .set("test/dynamic-dispatch", job.id.clone(), job.value.clone())
                .await?;
            Ok(())
        }
    }

    /// A job that always fails, with a timeout in the past so a failed message
    /// becomes available again immediately. This lets tests assert that the
    /// consumer applies the job's own reservation window without waiting.
    struct FailingJob;

    impl Job for FailingJob {
        type JobType = TestPayload;

        fn partition() -> &'static str {
            "test/failing-retry"
        }

        fn timeout(&self) -> TimeDelta {
            TimeDelta::seconds(-1)
        }

        async fn handle(
            &self,
            _ctx: JobContext<impl Services + Send + Sync + 'static>,
            _job: &Self::JobType,
        ) -> Result<(), human_errors::Error> {
            Err(human_errors::user(
                "The job failed.".to_string(),
                &["This failure is expected in tests."],
            ))
        }
    }

    #[test]
    fn test_registry_has_unique_partitions() {
        let mut seen = HashSet::new();
        let mut partitions = Vec::new();

        for registration in inventory::iter::<JobRegistration> {
            let partition = registration.handler().partition();
            assert!(
                seen.insert(partition),
                "duplicate job partition registered: {partition}"
            );
            partitions.push(partition);
        }

        // Every job that ships with the application should be registered.
        assert!(partitions.contains(&"cron"));
        assert!(partitions.contains(&"webhooks/azure-monitor"));
        assert!(
            partitions.len() >= 19,
            "expected at least 19 registered jobs, found {}",
            partitions.len()
        );
    }

    #[tokio::test]
    async fn test_job_runnable_deserializes_and_dispatches() {
        let services = ServicesContainer::new_mock().await.unwrap();

        let payload = serde_json::json!({ "id": "k1", "value": "v1" });
        JobRunnable::handle(
            &TestJob,
            JobContext::new(services.clone(), Utc::now(), None, None),
            &payload,
        )
        .await
        .unwrap();

        let stored: Option<String> = services
            .kv()
            .get("test/dynamic-dispatch", "k1")
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn test_job_runnable_runs_setup() {
        let services = ServicesContainer::new_mock().await.unwrap();

        // The type-erased setup should delegate to the job's `Job::setup`.
        JobRunnable::setup(&TestJob, services.clone())
            .await
            .unwrap();

        let stored: Option<String> = services
            .kv()
            .get("test/dynamic-dispatch", "setup")
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn test_consumer_processes_and_completes_message() {
        static TEST_JOB: TestJob = TestJob;

        let services = ServicesContainer::new_mock().await.unwrap();

        services
            .queue()
            .enqueue(
                "test/dynamic-dispatch",
                TestPayload {
                    id: "k2".into(),
                    value: "v2".into(),
                },
                None,
                None,
            )
            .await
            .unwrap();

        let item = services
            .queue()
            .dequeue_any(chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(item.partition, "test/dynamic-dispatch");

        JobHost::process(&TEST_JOB, item, services.clone(), tracing::Span::none()).await;

        // The job handler should have run.
        let stored: Option<String> = services
            .kv()
            .get("test/dynamic-dispatch", "k2")
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("v2"));

        // The message should have been completed (removed) from the queue, so a
        // subsequent dequeue finds nothing and blocks until the timeout elapses.
        let next = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            services.queue().dequeue_any(chrono::Duration::seconds(60)),
        )
        .await;
        assert!(
            next.is_err(),
            "expected the queue to be empty after the job completed"
        );
    }

    #[tokio::test]
    async fn test_consumer_applies_job_timeout_on_failure() {
        static FAILING: FailingJob = FailingJob;

        let services = ServicesContainer::new_mock().await.unwrap();

        services
            .queue()
            .enqueue(
                "test/failing-retry",
                TestPayload {
                    id: "k3".into(),
                    value: "v3".into(),
                },
                None,
                None,
            )
            .await
            .unwrap();

        // Dequeue with a long reservation. If `process` did not narrow this to
        // the job's own timeout, the failed message would stay hidden for a
        // minute instead of becoming immediately retriable.
        let item = services
            .queue()
            .dequeue_any(chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(item.partition, "test/failing-retry");

        JobHost::process(&FAILING, item, services.clone(), tracing::Span::none()).await;

        // The job failed, so the message must remain on the queue and become
        // available again immediately because its timeout released the
        // reservation.
        let retried = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            services.queue().dequeue_any(chrono::Duration::seconds(60)),
        )
        .await
        .expect("a failed job whose timeout has elapsed should be retriable immediately")
        .unwrap();
        assert_eq!(retried.partition, "test/failing-retry");
    }
}
