//! Generic debounce for flapping health signals.
//!
//! Alerting integrations frequently receive a stream of *triggered* (unhealthy) / *recovered*
//! (healthy) notifications for the same entity and want to avoid churning an operator on transient
//! blips. [`Debouncer`] is a small detector over that stream: it tracks per-entity state and
//! classifies each observation as one of the [`Detection`] variants, leaving the caller to perform
//! the resulting actions.
//!
//! The two detections:
//!
//! * [`Triggered`](Detection::Triggered) — the entity is unhealthy. Carries the time the ongoing
//!   incident *first* triggered. When that equals the observation time the incident is brand new and
//!   the caller should debounce for [`DebounceConfig::alert_delay`] before surfacing an alert (a
//!   recovery within that window suppresses it, so a brief blip never surfaces); when it is earlier
//!   the entity has flapped back to unhealthy while recovering, so the caller can re-alert
//!   immediately with the original context rather than treating it as a brand-new incident.
//! * [`Recovering`](Detection::Recovering) — the entity has recovered after being triggered. Carries
//!   how long it was triggered (first trigger → recovery), i.e. the incident's total impact.
//!
//! Incident identity is anchored on the **last trigger**, not on the recovery signal, so an incident
//! keeps its identity (and its first-trigger time, for the impact measurement) as long as triggers
//! keep arriving within a window of one another — an intervening recovery blip does not time it out
//! prematurely. A recovery is only ever genuine while it stays newer than the last trigger, so the
//! caller should cancel any pending recovery follow-up whenever a new trigger arrives.
//!
//! The debouncer only owns the state and the classification; performing the resulting actions
//! (scheduling work, cancelling it, notifying an operator) and reporting whether an alert has
//! surfaced are left to the caller, keeping the module free of any integration-specific concerns.

use std::borrow::Cow;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::db::KeyValueStore;

/// Timing configuration for a [`Debouncer`].
#[derive(Clone, Copy, Debug)]
pub struct DebounceConfig {
    /// How long the entity must go without a trigger before its recovery is settled and a subsequent
    /// trigger counts as a brand-new incident. A trigger inside this window (measured from the last
    /// trigger) is treated as flapping and re-escalates the existing incident.
    pub window: Duration,
}

/// Persisted per-entity debounce state.
///
/// This lives in the caller-supplied key/value partition, keyed by the entity's stable identifier.
/// Callers do not normally read it directly, but it is public so it can be inspected (for example in
/// tests or admin tooling).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DebounceState {
    /// When the current incident first triggered. Preserved across flaps inside the recovery window
    /// so impact is measured from the first sign of trouble; reset when a new incident begins.
    pub first_triggered_at: DateTime<Utc>,

    /// The most recent trigger for this entity. It anchors the incident-identity cutoff: the incident
    /// stays "alive" until a full window passes with no further trigger.
    pub last_triggered_at: DateTime<Utc>,

    /// When the entity entered the *recovering* state — the time of the recovery that began it — or
    /// `None` when it is not currently recovering. A recovery is only genuine while this is newer
    /// than [`Self::last_triggered_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovering_since: Option<DateTime<Utc>>,
}

/// The classification the debouncer assigns to an observation (see [`Debouncer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detection {
    /// The entity is unhealthy. `first_triggered_at` is the time the ongoing incident first
    /// triggered: it equals the observation time for a brand-new incident, and is earlier when the
    /// entity has flapped back to unhealthy while recovering, letting the caller re-alert immediately
    /// with the original context rather than treating it as brand new.
    Triggered { first_triggered_at: DateTime<Utc> },

    /// The entity has recovered after being triggered. Carries how long it was triggered (first
    /// trigger → this recovery) — the incident's total impact.
    Recovering { triggered_for: Duration },
}

/// A reusable debounce detector for flapping health signals, backed by a key/value store.
///
/// See the [module documentation](self) for the detections. Construct one per integration with its
/// own partition and [`DebounceConfig`]; a debouncer is cheap to create and holds only the store
/// handle and configuration.
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

    /// Records a trigger for `key` observed at `now`, updates the persisted state, and returns a
    /// [`Detection::Triggered`] whose `first_triggered_at` equals `now` for a brand-new incident and
    /// is the ongoing incident's original trigger time when the entity has flapped back to unhealthy
    /// while recovering.
    ///
    /// A trigger always makes the last trigger newer than any prior recovery, so the caller should
    /// also cancel any pending recovery follow-up — a recovery is only ever settled while it remains
    /// newer than the last trigger.
    pub async fn on_triggered(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Detection, human_errors::Error> {
        let new_state = if let Some(state) = self.load(key).await? {
            if now - state.last_triggered_at < self.config.window {
                DebounceState {
                    last_triggered_at: now,
                    recovering_since: None,
                    ..state
                }
            } else {
                DebounceState {
                    first_triggered_at: now,
                    last_triggered_at: now,
                    recovering_since: None,
                }
            }
        } else {
            DebounceState {
                first_triggered_at: now,
                last_triggered_at: now,
                recovering_since: None,
            }
        };

        let first_triggered = new_state.first_triggered_at;
        self.store(key, new_state).await?;

        Ok(Detection::Triggered {
            first_triggered_at: first_triggered,
        })
    }

    /// Records a recovery for `key` observed at `now`.
    ///
    /// `alert_surfaced` tells the debouncer whether an alert actually reached an operator for this
    /// incident (for example, whether the delayed alert already ran). When it did, this returns
    /// [`Detection::Recovering`] with the total triggered duration and marks the entity as
    /// recovering; when it did not, the incident never mattered, so the state is dropped and `None`
    /// is returned. Any still-pending alert is the caller's to cancel.
    pub async fn on_recovered(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Detection>, human_errors::Error> {
        if let Some(state) = self.load(key).await? {
            let triggered_for = if let Some(recovering) = state.recovering_since &&
                recovering > state.last_triggered_at
            {
                // The entity is already recovering, so the caller has already been notified and the
                // recovery duration has already been reported. Don't adjust the
                // recovery timestamp.
                return Ok(Some(Detection::Recovering { triggered_for: (recovering - state.first_triggered_at).max(Duration::zero()) }));
            } else {
                (now - state.first_triggered_at).max(Duration::zero())
            };

            self.store(
                key,
                DebounceState {
                    recovering_since: Some(now),
                    ..state
                },
            )
            .await?;

            Ok(Some(Detection::Recovering { triggered_for }))
        } else {
            Ok(None)
        }
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
            window: Duration::hours(1),
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

    #[tokio::test]
    async fn fresh_trigger_is_new_and_records_first_trigger() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");

        assert_eq!(
            d.on_triggered("probe", t0).await.unwrap(),
            Detection::Triggered {
                first_triggered_at: t0
            }
        );

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_triggered_at, t0);
        assert_eq!(s.last_triggered_at, t0);
        assert!(s.recovering_since.is_none());
    }

    #[tokio::test]
    async fn continuing_trigger_preserves_first_but_advances_last() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:02:00Z");

        d.on_triggered("probe", t0).await.unwrap();
        // A second trigger within the window belongs to the same ongoing incident, so it reports the
        // incident's original first-trigger time (not `now`) while advancing the last trigger.
        assert_eq!(
            d.on_triggered("probe", t1).await.unwrap(),
            Detection::Triggered {
                first_triggered_at: t0
            }
        );

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_triggered_at, t0);
        assert_eq!(s.last_triggered_at, t1);
    }

    #[tokio::test]
    async fn recovery_after_surfaced_alert_reports_triggered_duration() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z");

        d.on_triggered("probe", t0).await.unwrap();
        let detection = d.on_recovered("probe", t1).await.unwrap();

        // Triggered for first -> recovery (15m); the recovery is recorded as newer than the last
        // trigger.
        assert_eq!(
            detection,
            Some(Detection::Recovering {
                triggered_for: Duration::minutes(15)
            })
        );
        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_triggered_at, t0);
        assert_eq!(s.last_triggered_at, t0);
        assert_eq!(s.recovering_since, Some(t1));
    }

    #[tokio::test]
    async fn repeated_recovery_is_idempotent() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:20:00Z"); // a second recovery signal

        d.on_triggered("probe", t0).await.unwrap();
        assert_eq!(
            d.on_recovered("probe", t1).await.unwrap(),
            Some(Detection::Recovering {
                triggered_for: Duration::minutes(15)
            })
        );

        // A second recovery while already recovering re-reports the same impact and does not move the
        // recovery timestamp forward — recovery began at t1, not t2.
        assert_eq!(
            d.on_recovered("probe", t2).await.unwrap(),
            Some(Detection::Recovering {
                triggered_for: Duration::minutes(15)
            })
        );
        assert_eq!(state(&d, "probe").await.unwrap().recovering_since, Some(t1));
    }

    #[tokio::test]
    async fn flap_within_window_carries_the_first_trigger_time() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:45:00Z"); // flap, 45m after the last trigger (t0)

        d.on_triggered("probe", t0).await.unwrap();
        d.on_recovered("probe", t1).await.unwrap();
        assert_eq!(
            d.on_triggered("probe", t2).await.unwrap(),
            Detection::Triggered {
                first_triggered_at: t0
            }
        );

        let s = state(&d, "probe").await.unwrap();
        assert_eq!(s.first_triggered_at, t0); // preserved across the flap
        assert_eq!(s.last_triggered_at, t2);
        // The last trigger is now newer than the recovery, so any pending recovery follow-up is void.
        assert!(s.recovering_since.is_none());
    }

    #[tokio::test]
    async fn triggered_duration_spans_the_whole_flapping_incident() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:45:00Z"); // flap within window (45m after t0)
        let t3 = dt("2026-01-01T00:55:00Z"); // recover again

        d.on_triggered("probe", t0).await.unwrap();
        d.on_recovered("probe", t1).await.unwrap();
        d.on_triggered("probe", t2).await.unwrap();
        let detection = d.on_recovered("probe", t3).await.unwrap();

        // Triggered for t0 -> t3 (55m), not t2 -> t3.
        assert_eq!(
            detection,
            Some(Detection::Recovering {
                triggered_for: Duration::minutes(55)
            })
        );
    }

    #[tokio::test]
    async fn window_is_measured_from_the_last_trigger_not_the_recovery() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:50:00Z"); // recover after a long trigger
        let t2 = dt("2026-01-01T01:10:00Z"); // trigger again: 20m after recovery, but 70m after t0

        d.on_triggered("probe", t0).await.unwrap();
        d.on_recovered("probe", t1).await.unwrap();
        // 70m > the 1h window since the last trigger, so this is a NEW incident even though it is
        // only 20m after the recovery signal.
        assert_eq!(
            d.on_triggered("probe", t2).await.unwrap(),
            Detection::Triggered {
                first_triggered_at: t2
            }
        );
        assert_eq!(state(&d, "probe").await.unwrap().first_triggered_at, t2);
    }

    #[tokio::test]
    async fn boundary_at_exactly_the_window_is_a_new_incident() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T01:00:00Z"); // exactly 1h after the last trigger (t0)

        d.on_triggered("probe", t0).await.unwrap();
        d.on_recovered("probe", t1).await.unwrap();
        // The window is half-open (`< recovery_window`), so exactly 1h after the last trigger is a
        // new incident.
        assert_eq!(
            d.on_triggered("probe", t2).await.unwrap(),
            Detection::Triggered {
                first_triggered_at: t2
            }
        );
        assert_eq!(state(&d, "probe").await.unwrap().first_triggered_at, t2);
    }

    #[tokio::test]
    async fn recovery_with_no_prior_state_is_ignored() {
        let d = debouncer().await;
        let t = dt("2026-01-01T00:00:00Z");

        // A recovery for an entity we have no record of is a no-op: there was no incident to settle,
        // so nothing is reported and no state is created.
        assert_eq!(d.on_recovered("probe", t).await.unwrap(), None);
        assert!(state(&d, "probe").await.is_none());
    }

    #[tokio::test]
    async fn a_trigger_supersedes_a_recovery() {
        let d = debouncer().await;
        let t0 = dt("2026-01-01T00:00:00Z");
        let t1 = dt("2026-01-01T00:15:00Z"); // recover
        let t2 = dt("2026-01-01T00:30:00Z"); // flap

        d.on_triggered("probe", t0).await.unwrap();
        d.on_recovered("probe", t1).await.unwrap();
        assert_eq!(state(&d, "probe").await.unwrap().recovering_since, Some(t1));

        // Once a trigger lands after the recovery, the recovery is cleared — it is no longer newer
        // than the last trigger, so the caller must not go on to settle it.
        d.on_triggered("probe", t2).await.unwrap();
        let s = state(&d, "probe").await.unwrap();
        assert!(s.recovering_since.is_none());
        assert_eq!(s.last_triggered_at, t2);
    }
}
