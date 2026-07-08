//! Generic debounce for flapping health signals.
//!
//! Alerting integrations frequently receive a stream of *healthy* / *unhealthy* notifications for
//! the same entity and want to avoid churning an operator on transient blips. [`Debouncer`]
//! captures that pattern behind a small state machine so each integration only has to describe
//! *what* to do — surface an alert, escalate it, mark it recovering, or forget it — while the timing
//! and per-entity bookkeeping live here.
//!
//! The lifecycle of one incident:
//!
//! * **Unhealthy** — the first failure starts an incident and the caller is told to
//!   [`Schedule`](UnhealthyAction::Schedule) the alert [`DebounceConfig::alert_delay`] into the
//!   future. A recovery received before that elapses suppresses the alert entirely, so a brief blip
//!   never surfaces.
//! * **Healthy (alert surfaced)** — a genuine recovery. The caller is told to
//!   [`Recover`](HealthyAction::Recover), with the total `impact` measured from the *first* failure
//!   so a flapping incident is reported end to end. The caller shows the entity as recovering and
//!   confirms the recovery a full [`DebounceConfig::recovery_window`] later — but only while the
//!   recovery remains newer than the last failure, so any failure in the meantime cancels the
//!   confirmation.
//! * **Healthy (nothing surfaced)** — the incident never reached an operator, so it is
//!   [`Suppress`](HealthyAction::Suppress)ed and forgotten.
//! * **Unhealthy again within the recovery window** — a relapse of the same incident; the caller is
//!   told to [`Escalate`](UnhealthyAction::Escalate) immediately. The first-failure time is
//!   preserved across the relapse so impact still counts from the original outage.
//!
//! The incident-identity cutoff (same incident vs. a brand-new one) is anchored on the **last
//! unhealthy report**, not on the recovery signal, so an incident keeps its identity as long as
//! failures keep arriving within a window of one another — an intervening recovery blip does not
//! time it out prematurely.
//!
//! The debouncer only owns the state and the decision; performing the resulting actions (scheduling
//! work, cancelling it, notifying an operator) and reporting whether an alert has surfaced are left
//! to the caller, keeping the module free of any integration-specific concerns.

use std::borrow::Cow;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::db::KeyValueStore;

/// Timing configuration for a [`Debouncer`].
#[derive(Clone, Copy, Debug)]
pub struct DebounceConfig {
    /// How long a fresh unhealthy signal is held before the caller should surface an alert. A
    /// recovery received within this window cancels it, so a brief blip never surfaces.
    pub alert_delay: Duration,

    /// How long the entity must go without an unhealthy report before its recovery is confirmed and
    /// a subsequent failure counts as a brand-new incident. A relapse inside this window (measured
    /// from the last unhealthy report) re-escalates the existing incident.
    pub recovery_window: Duration,
}

/// Persisted per-entity debounce state.
///
/// This lives in the caller-supplied key/value partition, keyed by the entity's stable identifier.
/// Callers do not normally read it directly, but it is public so it can be inspected (for example in
/// tests or admin tooling).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DebounceState {
    /// When the current incident first went unhealthy. Preserved across relapses inside the recovery
    /// window so impact is measured from the first sign of failure; reset when a new incident
    /// begins.
    pub first_unhealthy_at: DateTime<Utc>,

    /// The most recent unhealthy report for this entity. It anchors the incident-identity cutoff:
    /// the incident stays "alive" until a full window passes with no further unhealthy report.
    pub last_unhealthy_at: DateTime<Utc>,

    /// When the entity entered the *recovering* state — the time of the healthy signal that began
    /// the current recovery — or `None` when it is not currently recovering. A recovery is only
    /// genuine while this is newer than [`Self::last_unhealthy_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovering_since: Option<DateTime<Utc>>,
}

/// What the caller should do in response to an unhealthy signal (see [`Debouncer::on_unhealthy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnhealthyAction {
    /// A fresh or continuing failure: surface the alert after `delay`, cancelling it if a recovery
    /// arrives first. Re-dispatching with a stable idempotency key keeps repeats from stacking up.
    Schedule { delay: Duration },

    /// A relapse inside the recovery window: an operator is already watching, so surface (or
    /// refresh) the alert immediately.
    Escalate,
}

/// What the caller should do in response to a healthy signal (see [`Debouncer::on_healthy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthyAction {
    /// A genuine recovery of a surfaced alert. Show the entity as recovering now and confirm it as
    /// recovered a [`DebounceConfig::recovery_window`] later (cancelling that confirmation on any
    /// further failure); `impact` spans the first failure to now.
    Recover { impact: Duration },

    /// The entity recovered before any alert surfaced (or nothing was being tracked): there is
    /// nothing to show, so the incident is forgotten.
    Suppress,
}

/// A reusable debounce state machine for flapping health signals, backed by a key/value store.
///
/// See the [module documentation](self) for the incident lifecycle. Construct one per integration
/// with its own partition and [`DebounceConfig`]; a debouncer is cheap to create and holds only the
/// store handle and configuration.
pub struct Debouncer<K> {
    kv: K,
    partition: Cow<'static, str>,
    config: DebounceConfig,
}

impl<K: KeyValueStore> Debouncer<K> {
    /// Creates a debouncer that persists its state in `partition` of `kv`.
    pub fn new(kv: K, partition: impl Into<Cow<'static, str>>, config: DebounceConfig) -> Self {
        Self {
            kv,
            partition: partition.into(),
            config,
        }
    }

    /// Records an unhealthy signal for `key` observed at `now`, updates the persisted state, and
    /// returns the [`UnhealthyAction`] the caller should take.
    ///
    /// Recording an unhealthy signal makes the last failure newer than any prior recovery, so the
    /// caller should also cancel any pending recovery confirmation — a recovery is only ever
    /// confirmed while it remains newer than the last failure.
    pub async fn on_unhealthy(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<UnhealthyAction, human_errors::Error> {
        let state = self.load(key).await?;

        // The incident-identity cutoff runs from the last unhealthy report, so an incident keeps its
        // identity (and its first-failure time) as long as failures keep arriving within a window of
        // one another, rather than being reset off the back of an intervening recovery signal.
        let within_window = state
            .as_ref()
            .is_some_and(|s| now - s.last_unhealthy_at < self.config.recovery_window);

        let first_unhealthy_at = match &state {
            Some(prev) if within_window => prev.first_unhealthy_at,
            _ => now,
        };

        // Escalate immediately when we are showing the entity as recovering and it relapses inside
        // the window; otherwise this is a fresh or continuing failure to debounce.
        let recovering = state.as_ref().is_some_and(|s| s.recovering_since.is_some());
        let action = if within_window && recovering {
            UnhealthyAction::Escalate
        } else {
            UnhealthyAction::Schedule {
                delay: self.config.alert_delay,
            }
        };

        self.store(
            key,
            DebounceState {
                first_unhealthy_at,
                last_unhealthy_at: now,
                recovering_since: None,
            },
        )
        .await?;

        Ok(action)
    }

    /// Records a healthy signal for `key` observed at `now` and returns the [`HealthyAction`] to
    /// take.
    ///
    /// `alert_surfaced` tells the debouncer whether an alert actually reached an operator for this
    /// incident (for example, whether the delayed alert already ran). When it did, the entity is
    /// marked as recovering; when it did not, the incident is forgotten. Any still-pending alert is
    /// the caller's to cancel — the debouncer does not know how the caller surfaces alerts.
    pub async fn on_healthy(
        &self,
        key: &str,
        now: DateTime<Utc>,
        alert_surfaced: bool,
    ) -> Result<HealthyAction, human_errors::Error> {
        let state = self.load(key).await?;

        if !alert_surfaced {
            self.kv
                .remove(self.partition.clone(), key.to_string())
                .await?;
            return Ok(HealthyAction::Suppress);
        }

        let first_unhealthy_at = state.as_ref().map(|s| s.first_unhealthy_at).unwrap_or(now);
        let last_unhealthy_at = state.as_ref().map(|s| s.last_unhealthy_at).unwrap_or(now);

        let impact = (now - first_unhealthy_at).max(Duration::zero());

        // Record the recovery. It is newer than the last failure, so it is a genuine recovery until
        // (and unless) a subsequent unhealthy signal supersedes it.
        self.store(
            key,
            DebounceState {
                first_unhealthy_at,
                last_unhealthy_at,
                recovering_since: Some(now),
            },
        )
        .await?;

        Ok(HealthyAction::Recover { impact })
    }

    async fn load(&self, key: &str) -> Result<Option<DebounceState>, human_errors::Error> {
        self.kv
            .get::<DebounceState>(self.partition.clone(), key.to_string())
            .await
    }

    async fn store(&self, key: &str, state: DebounceState) -> Result<(), human_errors::Error> {
        self.kv
            .set(self.partition.clone(), key.to_string(), state)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SqliteDatabase;

    fn config() -> DebounceConfig {
        DebounceConfig {
            alert_delay: Duration::minutes(5),
            recovery_window: Duration::hours(1),
        }
    }

    /// Parses an RFC 3339 timestamp into a UTC instant. The explicit return type pins the otherwise
    /// ambiguous `FromStr` impl (chrono has one per timezone).
    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    async fn debouncer() -> Debouncer<SqliteDatabase> {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        Debouncer::new(db, "test/debounce", config())
    }

    async fn state(d: &Debouncer<SqliteDatabase>, key: &str) -> Option<DebounceState> {
        d.load(key).await.unwrap()
    }

    const SCHEDULE: UnhealthyAction = UnhealthyAction::Schedule {
        delay: Duration::minutes(5),
    };

    #[tokio::test]
    async fn fresh_failure_schedules_and_records_first_failure() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");

        assert_eq!(d.on_unhealthy("probe", t0).await.unwrap(), SCHEDULE);

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_unhealthy_at, t0);
        assert_eq!(s.last_unhealthy_at, t0);
        assert!(s.recovering_since.is_none());
    }

    #[tokio::test]
    async fn continuing_failure_preserves_first_but_advances_last() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:02:00Z");

        d.on_unhealthy("probe", t0).await.unwrap();
        // A second failure while not recovering is a continuing failure: still scheduled, first
        // preserved, last advanced.
        assert_eq!(d.on_unhealthy("probe", t1).await.unwrap(), SCHEDULE);

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_unhealthy_at, t0);
        assert_eq!(s.last_unhealthy_at, t1);
    }

    #[tokio::test]
    async fn recovery_after_surfaced_alert_reports_impact_from_first_failure() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z");

        d.on_unhealthy("probe", t0).await.unwrap();
        let action = d.on_healthy("probe", t1, true).await.unwrap();

        // Impact is first -> recovery (15m); the recovery is recorded as newer than the last failure.
        assert_eq!(
            action,
            HealthyAction::Recover {
                impact: Duration::minutes(15),
            }
        );
        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_unhealthy_at, t0);
        assert_eq!(s.last_unhealthy_at, t0);
        assert_eq!(s.recovering_since, Some(t1));
    }

    #[tokio::test]
    async fn recovery_without_surfaced_alert_is_suppressed_and_forgotten() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:01:00Z");

        d.on_unhealthy("probe", t0).await.unwrap();
        assert_eq!(
            d.on_healthy("probe", t1, false).await.unwrap(),
            HealthyAction::Suppress
        );
        assert!(state(&d, "probe").await.is_none());
    }

    #[tokio::test]
    async fn relapse_within_window_escalates_and_preserves_first_failure() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:45:00Z"); // relapse, 45m after the last failure (t0)

        d.on_unhealthy("probe", t0).await.unwrap();
        d.on_healthy("probe", t1, true).await.unwrap();
        assert_eq!(
            d.on_unhealthy("probe", t2).await.unwrap(),
            UnhealthyAction::Escalate
        );

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_unhealthy_at, t0); // preserved across the relapse
        assert_eq!(s.last_unhealthy_at, t2);
        // The last failure is now newer than the recovery, so any pending confirmation is void.
        assert!(s.recovering_since.is_none());
    }

    #[tokio::test]
    async fn impact_spans_the_whole_flapping_incident() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:45:00Z"); // relapse within window (45m after t0)
        let t3 = dt("2026-01-01T00:55:00Z"); // recover again

        d.on_unhealthy("probe", t0).await.unwrap();
        d.on_healthy("probe", t1, true).await.unwrap();
        d.on_unhealthy("probe", t2).await.unwrap();
        let action = d.on_healthy("probe", t3, true).await.unwrap();

        // Impact runs t0 -> t3 (55m), not t2 -> t3.
        assert_eq!(
            action,
            HealthyAction::Recover {
                impact: Duration::minutes(55),
            }
        );
    }

    #[tokio::test]
    async fn window_is_measured_from_the_last_failure_not_the_recovery() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:50:00Z"); // recover after a long failure
        let t2 = dt("2026-01-01T01:10:00Z"); // fail again: 20m after recovery, but 70m after t0

        d.on_unhealthy("probe", t0).await.unwrap();
        d.on_healthy("probe", t1, true).await.unwrap();
        // 70m > the 1h window since the last failure, so this is a NEW incident (scheduled, not
        // escalated) even though it is only 20m after the recovery signal.
        assert_eq!(d.on_unhealthy("probe", t2).await.unwrap(), SCHEDULE);
        assert_eq!(state(&d, "probe").await.unwrap().first_unhealthy_at, t2);
    }

    #[tokio::test]
    async fn boundary_at_exactly_the_window_is_a_new_incident() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T01:00:00Z"); // exactly 1h after the last failure (t0)

        d.on_unhealthy("probe", t0).await.unwrap();
        d.on_healthy("probe", t1, true).await.unwrap();
        // The window is half-open (`< recovery_window`), so exactly 1h after the last failure is a
        // new incident.
        assert_eq!(d.on_unhealthy("probe", t2).await.unwrap(), SCHEDULE);
        assert_eq!(state(&d, "probe").await.unwrap().first_unhealthy_at, t2);
    }

    #[tokio::test]
    async fn recovery_with_no_prior_state_uses_now_as_the_baseline() {
        let d = debouncer().await;
        let t = dt("2026-01-01T00:00:00Z");

        // A healthy signal for a surfaced alert we have no record of (e.g. one created before the
        // debouncer existed) yields a zero-impact recovery.
        assert_eq!(
            d.on_healthy("probe", t, true).await.unwrap(),
            HealthyAction::Recover {
                impact: Duration::zero(),
            }
        );
    }

    #[tokio::test]
    async fn a_failure_supersedes_a_recovery_so_its_confirmation_is_void() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:30:00Z"); // relapse

        d.on_unhealthy("probe", t0).await.unwrap();
        d.on_healthy("probe", t1, true).await.unwrap();
        assert_eq!(state(&d, "probe").await.unwrap().recovering_since, Some(t1));

        // Once a failure lands after the recovery, the recovery is cleared — it is no longer newer
        // than the last failure, so the caller must not go on to confirm it.
        d.on_unhealthy("probe", t2).await.unwrap();
        let s = state(&d, "probe").await.unwrap();
        assert!(s.recovering_since.is_none());
        assert_eq!(s.last_unhealthy_at, t2);
    }
}
