use calcard::{
    Entry,
    icalendar::{ICalendar, ICalendarClassification, ICalendarStatus, ICalendarValue},
};
use chrono::{DateTime, Utc};
use human_errors as errors;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing_batteries::prelude::*;

use crate::filter::Filterable;

pub struct Calendar {
    icalendar: ICalendar,
}

macro_rules! property_value {
    ($value:expr, $prop:ident, $v:ident => $val:expr) => {
        $value
            .property(&calcard::icalendar::ICalendarProperty::$prop)
            .and_then(|v| v.values.first())
            .and_then(|$v| $val)
            .ok_or_else(|| {
                errors::user(
                    concat!("Missing ", stringify!($prop), " field for calendar entry."),
                    &[concat!(
                        "Make sure that the calendar entry has a ",
                        stringify!($prop),
                        " field."
                    )],
                )
            })
    };
    ($value:expr, custom $prop:expr, $v:ident => $val:expr) => {
        match $value.property(&calcard::icalendar::ICalendarProperty::Other($prop.into())) {
            Some(prop) => prop.values.first().map(|$v| $val).ok_or_else(|| {
                errors::user(
                    concat!(
                        "Could not parse the ",
                        stringify!($prop),
                        " field for calendar entry."
                    ),
                    &[concat!(
                        "Make sure that the calendar entry has a valid ",
                        stringify!($prop),
                        " field."
                    )],
                )
            }),
            None => Ok(None),
        }
    };
    ($value:expr, optional $prop:ident, $v:ident => $val:expr) => {
        match $value.property(&calcard::icalendar::ICalendarProperty::$prop) {
            Some(prop) => prop.values.first().map(|$v| $val).ok_or_else(|| {
                errors::user(
                    concat!(
                        "Could not parse the ",
                        stringify!($prop),
                        " field for calendar entry."
                    ),
                    &[concat!(
                        "Make sure that the calendar entry has a valid ",
                        stringify!($prop),
                        " field."
                    )],
                )
            }),
            None => Ok(None),
        }
    };
}

impl Calendar {
    #[instrument("parsers.calendar.events", skip(self), err(Display))]
    pub fn events(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<CalendarEvent>, human_errors::Error> {
        let expanded = self
            .icalendar
            .expand_dates(calcard::common::timezone::Tz::UTC, 10000);
        expanded.events.iter().filter(|event| match event.end {
            calcard::icalendar::dates::TimeOrDelta::Delta(d) => event.start + d,
            calcard::icalendar::dates::TimeOrDelta::Time(t) => t
        } >= start && event.start <= end).map(|event| {
            let start = event.start;
            let end = match event.end {
                calcard::icalendar::dates::TimeOrDelta::Delta(d) => start + d,
                calcard::icalendar::dates::TimeOrDelta::Time(t) => t,
            };

            if let Some(value) = self.icalendar.component_by_id(event.comp_id) {
                Ok(CalendarEvent {
                    uid: property_value!(value, Uid, v => v.as_text())?.to_string(),
                    summary: property_value!(value, Summary, v => v.as_text())?.to_string(),
                    description: property_value!(value, optional Description, v => v.as_text())?.map(|s| s.to_string()),
                    start: start.to_utc(),
                    end: end.to_utc(),
                    private: matches!(
                        property_value!(value, Class, v => Some(v))?,
                        ICalendarValue::Classification(ICalendarClassification::Private | ICalendarClassification::Confidential)),
                    all_day: property_value!(value, custom "X-MICROSOFT-CDO-ALLDAYEVENT", v => v.as_boolean())?.unwrap_or_default(),
                    status: match property_value!(value, Status, v => Some(v))? {
                        ICalendarValue::Status(status) => status.clone(),
                        _ => ICalendarStatus::Tentative,
                    },
                    busy_status: property_value!(value, custom "X-MICROSOFT-CDO-BUSYSTATUS", v => v.as_text())?.map(|s| s.into()).unwrap_or(BusyStatus::Busy),
                    intended_status: property_value!(value, custom "X-MICROSOFT-CDO-INTENDEDSTATUS", v => v.as_text())?.map(|s| s.into()).unwrap_or(BusyStatus::Busy),
                })
            } else {
                unreachable!("Event component with ID {} not found", event.comp_id);
            }
        }).collect::<Result<Vec<_>, _>>()
    }
}

impl FromStr for Calendar {
    type Err = human_errors::Error;

    #[instrument("parsers.calendar.parse", skip(s), err(Display))]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let icalendar = ICalendar::parse(s).map_err(|e| match e {
            Entry::InvalidLine(line) => errors::user(
                format!("Failed to parse calendar entry line: '{}'.", line),
                &[
                    "Make sure that the calendar data is correctly formatted.",
                    "Check for any invalid or unsupported lines in the calendar data.",
                ],
            ),
            Entry::UnterminatedComponent(component) => errors::user(
                format!("Calendar component '{}' is unterminated.", component),
                &[
                    "Ensure that all calendar components are properly closed.",
                    "Check the calendar data for any missing 'END' statements.",
                ],
            ),
            Entry::UnexpectedComponentEnd { expected, found } => errors::user(
                format!(
                    "Expected end of component '{:?}', but found end of component '{:?}'.",
                    expected, found
                ),
                &[
                    "Ensure that all calendar components are properly nested and closed.",
                    "Check the calendar data for any mismatched 'END' statements.",
                ],
            ),
            _ => errors::user(
                "Failed to parse calendar data.",
                &[
                    "Ensure that the calendar data is correctly formatted.",
                    "Check for any invalid or unsupported entries in the calendar data.",
                ],
            ),
        })?;

        Ok(Self { icalendar })
    }
}

pub struct CalendarEvent {
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,

    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,

    pub status: ICalendarStatus,
    pub busy_status: BusyStatus,
    pub intended_status: BusyStatus,
    pub all_day: bool,
    pub private: bool,
}

impl Filterable for CalendarEvent {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "summary" => self.summary.clone().into(),
            "description" => self.description.clone().into(),

            "start" => self.start.to_rfc3339().into(),
            "end" => self.end.to_rfc3339().into(),
            "duration_minutes" => (self.end - self.start).num_minutes().into(),

            "status" => match self.status {
                ICalendarStatus::Confirmed => "confirmed".into(),
                ICalendarStatus::Tentative => "tentative".into(),
                ICalendarStatus::Cancelled => "cancelled".into(),
                ICalendarStatus::Completed => "completed".into(),
                ICalendarStatus::InProcess => "in-process".into(),
                ICalendarStatus::Pending => "pending".into(),
                ICalendarStatus::NeedsAction => "needs-action".into(),
                ICalendarStatus::Draft => "draft".into(),
                ICalendarStatus::Final => "final".into(),
                ICalendarStatus::Failed => "failed".into(),
            },

            "busy_status" => match self.busy_status {
                BusyStatus::Free => "free".into(),
                BusyStatus::Tentative => "tentative".into(),
                BusyStatus::Busy => "busy".into(),
                BusyStatus::OutOfOffice => "oof".into(),
            },

            "intended_status" => match self.intended_status {
                BusyStatus::Free => "free".into(),
                BusyStatus::Tentative => "tentative".into(),
                BusyStatus::Busy => "busy".into(),
                BusyStatus::OutOfOffice => "oof".into(),
            },

            "is_private" => self.private.into(),
            "is_all_day" => self.all_day.into(),

            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusyStatus {
    Free,
    Tentative,
    Busy,
    OutOfOffice,
}

impl From<&str> for BusyStatus {
    fn from(value: &str) -> Self {
        match value {
            "FREE" => BusyStatus::Free,
            "TENTATIVE" => BusyStatus::Tentative,
            "BUSY" => BusyStatus::Busy,
            "OOF" => BusyStatus::OutOfOffice,
            _ => BusyStatus::Busy, // Default to busy if unrecognized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::get_test_file_contents;

    #[test]
    fn parse_calendar() {
        let content = get_test_file_contents("calendar_large.ics");
        let calendar: Calendar = content.parse().expect("Failed to parse calendar");

        let mut events = 0;
        for event in calendar.events(
            DateTime::from_str("2023-07-01T00:00:00Z").expect("Failed to parse date"),
            DateTime::from_str("2023-07-31T23:59:59Z").expect("Failed to parse date"),
        ).expect("Failed to get events") {
            println!(
                "Event: {} - {} (private: {})",
                event.uid, event.summary, event.private
            );
            events += 1;
        }
        assert_eq!(events, 193);
    }
}
