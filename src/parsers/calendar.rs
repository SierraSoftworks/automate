use human_errors as errors;
use icalendar::{CalendarDateTime, Component, DatePerhapsTime, Event};

pub enum DateOrTime {
    Date(chrono::NaiveDate),
    Time(chrono::DateTime<chrono::Utc>),
}

impl TryFrom<DatePerhapsTime> for DateOrTime {
    type Error = errors::Error;

    fn try_from(value: DatePerhapsTime) -> Result<Self, Self::Error> {
        match value {
            DatePerhapsTime::Date(date) => Ok(DateOrTime::Date(date)),
            DatePerhapsTime::DateTime(CalendarDateTime::Floating(dt)) => {
                Ok(DateOrTime::Time(dt.and_utc()))
            }
            DatePerhapsTime::DateTime(CalendarDateTime::Utc(dt)) => Ok(DateOrTime::Time(dt)),
            DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid }) => {
                Ok(DateOrTime::Time(date_time.and_utc()))
            }
        }
    }
}

pub struct CalendarEvent<'a> {
    pub uid: &'a str,
    pub start: DateOrTime,
    pub end: DateOrTime,
    pub summary: &'a str,
    pub description: &'a str,
}

impl<'a, 'b> TryFrom<&'a Event> for CalendarEvent<'b>
where
    'a: 'b,
{
    type Error = errors::Error;
    fn try_from(value: &'a Event) -> Result<Self, Self::Error> {
        Ok(Self {
            uid: value.get_uid().ok_or_else(|| {
                errors::user(
                    "Missing UID field for calendar entry.",
                    &["Make sure that the calendar entry has a UID field."],
                )
            })?,
            summary: value.get_description().unwrap_or_default(),
            description: value.get_summary().unwrap_or_default(),
            start: value
                .get_start()
                .ok_or_else(|| {
                    errors::user(
                        "Missing start field for calendar entry.",
                        &["Make sure that the calendar entry has a start field."],
                    )
                })?
                .try_into()?,
            end: value
                .get_end()
                .ok_or_else(|| {
                    errors::user(
                        "Missing end field for calendar entry.",
                        &["Make sure that the calendar entry has an end field."],
                    )
                })?
                .try_into()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use icalendar::{Calendar, CalendarComponent};

    use super::*;
    use crate::testing::get_test_file_contents;

    //#[test]
    fn parse_calendar() {
        let content = get_test_file_contents("calendar_large.ics");
        let calendar: Calendar = content.parse().expect("Failed to parse calendar");

        for component in calendar.components {
            match component {
                CalendarComponent::Event(event) => {
                    let ev = CalendarEvent::try_from(&event).expect("Failed to parse event");
                }
                _ => {}
            }
        }
    }
}
