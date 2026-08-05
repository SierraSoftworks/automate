//! Durations on the wire, written the way a person would write them.
//!
//! # Why this module exists
//!
//! [`chrono`] ships a family of serde adapters for putting a timestamp on the
//! wire in whatever unit the other end expects — [`chrono::serde::ts_seconds`],
//! [`ts_milliseconds`](chrono::serde::ts_milliseconds),
//! [`ts_microseconds`](chrono::serde::ts_microseconds),
//! [`ts_nanoseconds`](chrono::serde::ts_nanoseconds), and an `_option` variant
//! of each. Every one of them is for a *date-time*: `chrono::serde` is nothing
//! but a re-export of `chrono`'s `datetime::serde` module. There is no
//! equivalent anywhere in the crate for [`chrono::Duration`] (a.k.a.
//! `TimeDelta`), so a duration that needs a human-readable encoding has to
//! bring its own.
//!
//! What `chrono` does give `Duration` is a derived-by-hand `Serialize` /
//! `Deserialize` pair that writes it as a `(seconds, nanoseconds)` **tuple** —
//! JSON `[300, 0]` for five minutes. That is a perfectly reasonable format for
//! one program talking to another, and an impossible thing to put in front of a
//! person: no number input, no form control, and no configuration file anybody
//! would write by hand produces a two-element array. Every duration we store is
//! an answer to "how long should we wait", which everybody answers in minutes,
//! so minutes is what goes on the wire and minutes is what the form collects.
//!
//! # Naming
//!
//! The modules here are named to mirror `chrono`'s own convention, so
//! `#[serde(with = "crate::serde_duration::minutes")]` reads like the
//! `ts_seconds` adapters it sits alongside, and [`minutes_option`] relates to
//! [`minutes`] exactly as [`chrono::serde::ts_seconds_option`] relates to
//! [`chrono::serde::ts_seconds`].
//!
//! # Precision: minutes in, minutes out
//!
//! Storing minutes means sub-minute precision does not survive the trip, and
//! that is a deliberate trade rather than an oversight:
//!
//! * **Serializing truncates.** A 90-second duration is written as `1`, because
//!   [`chrono::Duration::num_minutes`] truncates towards zero. Refusing instead
//!   would turn a harmless rounding into a hard failure at the one place that
//!   currently produces a sub-minute value (the length of a calendar entry,
//!   which the Todoist publisher already reduces to whole minutes before it is
//!   sent anywhere), so nothing downstream can observe the difference.
//! * **Deserializing does not round.** The wire form is a whole number of
//!   minutes; a fractional value such as `1.5` is refused by the integer
//!   deserializer rather than being silently rounded, because a configuration
//!   that cannot be stored as written should say so instead of quietly becoming
//!   a different configuration.
//!
//! # Negative durations are refused at both ends
//!
//! These are waiting periods. "Wait minus five minutes" is not something we
//! could do, and treating it as zero would alert immediately on a monitor
//! somebody thought they had told us to be patient about — so a negative value
//! is a mistake to report, not a value to honour.
//!
//! The refusal applies when *writing* as well as when reading. That is the only
//! way to guarantee that anything we put on the wire can be read back: these
//! adapters are used on queue payloads which are persisted as JSON and may sit
//! in the database for as long as their delay, so a value we were willing to
//! write but would refuse to read is a message that can never be delivered and
//! retries forever. Failing at the point the bad value is created reports the
//! actual bug, at the site that caused it.

/// The refusal shown for a negative duration, at both ends of the wire. Phrased
/// for the person editing the configuration rather than for the code that
/// caught it, since that is who has to act on it.
const NEGATIVE: &str = "A waiting period cannot be negative; give a number of minutes to wait, or zero to act straight away.";

/// The refusal shown when a stored number of minutes is too large to be a
/// [`chrono::Duration`] at all.
const TOO_LARGE: &str = "That is longer than we could ever wait; give a smaller number of minutes.";

/// Reduces a duration to the whole number of minutes that goes on the wire,
/// refusing the negative values documented at the module level.
fn to_minutes<E: serde::ser::Error>(value: chrono::Duration) -> Result<i64, E> {
    if value < chrono::Duration::zero() {
        return Err(E::custom(NEGATIVE));
    }

    Ok(value.num_minutes())
}

/// Rebuilds a duration from the whole number of minutes on the wire, refusing
/// both a negative span and one too large for [`chrono::Duration`] to hold.
fn from_minutes<E: serde::de::Error>(minutes: i64) -> Result<chrono::Duration, E> {
    if minutes < 0 {
        return Err(E::custom(NEGATIVE));
    }

    chrono::Duration::try_minutes(minutes).ok_or_else(|| E::custom(TOO_LARGE))
}

/// A [`chrono::Duration`] as a whole number of minutes.
///
/// ```ignore
/// #[serde(with = "crate::serde_duration::minutes")]
/// pub alert_delay: chrono::Duration,
/// ```
///
/// See the [module documentation](self) for why minutes, what happens to
/// sub-minute precision, and why a negative span is refused.
pub mod minutes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &chrono::Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(super::to_minutes(*value)?)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<chrono::Duration, D::Error> {
        super::from_minutes(i64::deserialize(deserializer)?)
    }
}

/// An `Option<`[`chrono::Duration`]`>` as a whole number of minutes, or `null`.
///
/// The `Option` counterpart to [`minutes`], in the same way that
/// [`chrono::serde::ts_seconds_option`] is the counterpart to
/// [`chrono::serde::ts_seconds`].
///
/// Note that `#[serde(with = ...)]` makes a field mandatory even when its type
/// is an `Option` — serde's usual "a missing `Option` field is `None`" shortcut
/// only applies to fields it deserializes itself. Pair this with
/// `#[serde(default)]` on any field that is allowed to be absent:
///
/// ```ignore
/// #[serde(default, with = "crate::serde_duration::minutes_option")]
/// pub duration: Option<chrono::Duration>,
/// ```
pub mod minutes_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<chrono::Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => serializer.serialize_some(&super::to_minutes(*duration)?),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<chrono::Duration>, D::Error> {
        Option::<super::Stored>::deserialize(deserializer)?
            .map(super::Stored::into_duration)
            .transpose()
    }
}

/// A duration as it may be found on the wire.
///
/// Whole minutes is what we write. The pair is what `chrono` wrote before this
/// module existed, and messages queued then are still in the queue now — a
/// consumer that could not read one would not merely fail that message, it
/// would fail every peek of the partition holding it, taking an unrelated job
/// down with it. Reading both costs a few lines; not reading both costs an
/// upgrade that has to be timed against an empty queue.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Stored {
    Minutes(i64),
    /// `(seconds, nanoseconds)`, as `chrono::Duration` serializes itself. The
    /// nanoseconds are named so the shape matches what has to be read, and
    /// ignored because nothing that wrote one carried sub-second precision
    /// anything downstream could observe.
    SecondsAndNanos(i64, #[allow(dead_code)] i32),
}

impl Stored {
    fn into_duration<E: serde::de::Error>(self) -> Result<chrono::Duration, E> {
        match self {
            Self::Minutes(minutes) => from_minutes(minutes),
            Self::SecondsAndNanos(seconds, _) => {
                // The nanoseconds are dropped rather than rounded: nothing that
                // wrote one of these carried sub-second precision that anything
                // downstream could observe.
                Ok(chrono::Duration::seconds(seconds))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    /// Stands in for a configuration struct holding a mandatory waiting period.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Waiting {
        #[serde(with = "super::minutes")]
        delay: chrono::Duration,
    }

    /// Stands in for a queue payload holding an optional span, which is allowed
    /// to be absent entirely as well as explicitly `null`.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct MaybeWaiting {
        #[serde(default, with = "super::minutes_option")]
        delay: Option<chrono::Duration>,
    }

    #[test]
    fn a_waiting_period_is_written_to_the_wire_as_a_bare_number_of_minutes() {
        // The whole point of this module is the shape of the encoded value, not
        // merely that it survives a round trip: a round trip would still pass if
        // we wrote chrono's `[seconds, nanos]` pair, and that is precisely the
        // format no number input can produce. So assert on the JSON itself.
        let encoded = serde_json::to_value(Waiting {
            delay: chrono::Duration::minutes(15),
        })
        .unwrap();

        assert_eq!(encoded, serde_json::json!({ "delay": 15 }));
        assert!(
            encoded["delay"].is_number(),
            "a form's number input has to be able to produce this value, got {:?}",
            encoded["delay"],
        );
    }

    #[test]
    fn a_waiting_period_survives_the_trip_through_the_wire_form() {
        let original = Waiting {
            delay: chrono::Duration::hours(2),
        };

        let decoded: Waiting =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();

        // Two hours is a whole number of minutes, so it comes back exactly as it
        // went in — the lossiness documented on this module is confined to spans
        // finer than the unit we store.
        assert_eq!(decoded, original);
        assert_eq!(decoded.delay, chrono::Duration::minutes(120));
    }

    #[test]
    fn an_optional_waiting_period_survives_the_trip_in_both_of_its_states() {
        for original in [
            MaybeWaiting {
                delay: Some(chrono::Duration::minutes(30)),
            },
            MaybeWaiting { delay: None },
        ] {
            let encoded = serde_json::to_string(&original).unwrap();
            let decoded: MaybeWaiting = serde_json::from_str(&encoded).unwrap();

            // `None` has to round-trip as faithfully as a real span does: these
            // adapters sit on queue payloads where "no duration was given" is a
            // meaningful state and must not decay into "zero minutes".
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn an_absent_optional_waiting_period_reads_as_nothing_at_all() {
        // `#[serde(with = ...)]` makes a field mandatory even when it is an
        // `Option`, so the `#[serde(default)]` that restores the usual behaviour
        // is load-bearing. Without it, every payload written before a duration
        // field existed would stop deserializing.
        let decoded: MaybeWaiting = serde_json::from_str("{}").unwrap();

        assert_eq!(decoded.delay, None);
    }

    #[test]
    fn nothing_at_all_is_written_as_null_rather_than_as_a_number() {
        // Zero is a legitimate waiting period meaning "act straight away", so an
        // absent duration must not be encoded as `0` — the two say different
        // things and the reader has no way to tell them apart afterwards.
        let encoded = serde_json::to_value(MaybeWaiting { delay: None }).unwrap();

        assert_eq!(encoded, serde_json::json!({ "delay": null }));
    }

    #[test]
    fn a_negative_waiting_period_is_refused_when_read_and_says_what_is_wrong() {
        // Silently clamping to zero would alert instantly on a monitor somebody
        // thought they had told us to be patient about, so this has to fail, and
        // it has to fail in a way that names the problem to whoever typed it.
        let Err(err) = serde_json::from_str::<Waiting>(r#"{"delay":-5}"#) else {
            panic!("a negative waiting period should not load");
        };

        let message = err.to_string();
        assert!(message.contains("negative"), "{message}");
        assert!(
            message.contains("zero to act straight away"),
            "the refusal should say what to do instead, got {message}",
        );
    }

    #[test]
    fn a_negative_optional_waiting_period_is_refused_too() {
        // The `Option` variant delegates to the same check; a negative span
        // wrapped in a `Some` is no more meaningful than a bare one.
        let Err(err) = serde_json::from_str::<MaybeWaiting>(r#"{"delay":-1}"#) else {
            panic!("a negative waiting period should not load");
        };

        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn a_negative_waiting_period_is_refused_when_written_as_well_as_when_read() {
        // Refusing on read alone would let us persist a queue payload that we
        // then decline to parse. Because those payloads outlive the process that
        // wrote them, that message would fail on every delivery attempt forever.
        // Failing here instead reports the bug at the site that caused it.
        let Err(err) = serde_json::to_string(&Waiting {
            delay: chrono::Duration::minutes(-5),
        }) else {
            panic!("a negative waiting period should not be written");
        };

        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn anything_we_are_willing_to_write_can_be_read_back() {
        // The property the write-side refusal exists to protect, stated directly:
        // for every duration that encodes at all, decoding the result succeeds.
        for minutes in [0, 1, 5, 60, 1440, 525_600] {
            let original = Waiting {
                delay: chrono::Duration::minutes(minutes),
            };

            let encoded = serde_json::to_string(&original)
                .unwrap_or_else(|e| panic!("{minutes} minutes should encode: {e}"));
            let decoded: Waiting = serde_json::from_str(&encoded)
                .unwrap_or_else(|e| panic!("{minutes} minutes should decode: {e}"));

            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn sub_minute_precision_is_dropped_on_the_way_out_rather_than_refused() {
        // Minutes is the storage unit, so a span finer than a minute cannot be
        // represented. We truncate rather than reject because the only producer
        // of such a span (the length of a calendar entry) is already reduced to
        // whole minutes by the publisher that consumes it, so refusing would
        // fail a job over a difference nothing downstream can observe.
        let encoded = serde_json::to_value(Waiting {
            delay: chrono::Duration::seconds(90),
        })
        .unwrap();

        assert_eq!(encoded, serde_json::json!({ "delay": 1 }));

        // Stated as the round-trip property it breaks, so the loss is recorded
        // as a known consequence rather than discovered as a surprise.
        let decoded: Waiting = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.delay, chrono::Duration::minutes(1));
        assert_ne!(decoded.delay, chrono::Duration::seconds(90));
    }

    #[test]
    fn a_span_shorter_than_a_minute_is_written_as_zero_not_lost_entirely() {
        // Truncation takes a 30-second span to `0`, which reads back as "act
        // straight away". That is the honest answer at this resolution, and it
        // is still a value rather than an error or a `null`.
        let encoded = serde_json::to_value(Waiting {
            delay: chrono::Duration::seconds(30),
        })
        .unwrap();

        assert_eq!(encoded, serde_json::json!({ "delay": 0 }));
    }

    #[test]
    fn a_fractional_number_of_minutes_is_refused_rather_than_rounded() {
        // A configuration that cannot be stored as written should say so. Half a
        // minute silently becoming zero (or one) is a configuration that does
        // something other than what its author asked for.
        assert!(
            serde_json::from_str::<Waiting>(r#"{"delay":1.5}"#).is_err(),
            "a fractional number of minutes should not load",
        );
    }

    #[test]
    fn a_number_of_minutes_too_large_to_hold_is_refused_by_name() {
        // `i64` minutes overflows `chrono::Duration` long before it overflows
        // itself, so the bound has to be checked rather than assumed.
        let Err(err) = serde_json::from_str::<Waiting>(&format!(r#"{{"delay":{}}}"#, i64::MAX))
        else {
            panic!("an unrepresentable waiting period should not load");
        };

        assert!(
            err.to_string().contains("smaller number of minutes"),
            "{err}"
        );
    }

    #[test]
    fn chronos_own_encoding_is_the_two_element_array_this_module_exists_to_avoid() {
        // Pins the premise of this module, and doubles as the record of what a
        // payload written before these adapters were adopted looks like: any
        // such value is a JSON array and will not parse as a number of minutes.
        let native = serde_json::to_value(chrono::Duration::minutes(5)).unwrap();

        assert_eq!(native, serde_json::json!([300, 0]));
        assert!(
            native.is_array(),
            "chrono writes a (seconds, nanos) pair, which is why we do not use it",
        );

        // And it is still read, because messages written in it are still in the
        // queue. A consumer that refused one would not just fail that message:
        // peeking a partition decodes into the concrete payload type, so a
        // single unreadable row fails the whole peek and takes an unrelated job
        // down with it.
        let legacy: MaybeWaiting = serde_json::from_str(r#"{"delay":[300,0]}"#)
            .expect("a payload queued before these adapters existed is still readable");

        assert_eq!(legacy.delay, Some(chrono::Duration::minutes(5)));
    }

    #[test]
    fn a_duration_queued_in_the_old_form_survives_the_upgrade() {
        // The one producer of a non-null duration is the calendar workflow, which
        // dispatches an event's length into `todoist/upsert-task`. Anything of
        // its already in the queue when this shipped has to still run.
        let payload: crate::publishers::TodoistUpsertTaskPayload = serde_json::from_str(
            r#"{"unique_key":"k","title":"t","due":"None","duration":[1800,0],"config":{}}"#,
        )
        .expect("a task queued before these adapters existed is still readable");

        assert_eq!(payload.duration, Some(chrono::Duration::minutes(30)));
    }
}
