use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{CalendarCollector, Diff, DifferentialCollector},
    db::StateKey,
    prelude::*,
    publishers::TodoistTarget,
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct CalendarWorkflowConfig {
    pub name: String,
    pub url: String,

    #[serde(default)]
    pub priority: Option<i32>,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for CalendarWorkflowConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "calendar/{}", self.name)
    }
}

#[derive(Clone)]
pub struct CalendarWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Reads an iCalendar feed and keeps a Todoist task for each event in it, due at
the event's start time and given the event's duration. Unlike the other
scheduled workflows this one is a mirror rather than an inbox: an event that
gets moved has its task moved with it, an event that is cancelled or that stops
matching the filter has its task completed, and re-running does not produce a
second copy of anything.

That makes it useful for the events you want to *act* on — a delivery slot, a
booking, an on-call shift — rather than for a calendar you already look at,
where it will simply duplicate what your calendar app already shows you.

Only the next seven days are looked at. An event further out than that gets no
task until the window reaches it, and an event that has passed drops out of the
window and has its task completed, which is why this is a reasonable thing to
point at a busy calendar without drowning in it.

## Getting the calendar address

**Calendar URL** wants an iCalendar feed: an address ending in `.ics`, served
over HTTP, which returns a `BEGIN:VCALENDAR` document. Most calendar services
publish one, usually described as a "secret address in iCal format", an "ICS
link", or a "subscribe" URL.

- **Google Calendar**: Settings → *Settings for my calendars* → pick the
  calendar → *Integrate calendar* → **Secret address in iCal format**.
- **Outlook / Microsoft 365**: Calendar settings → *Shared calendars* →
  publish the calendar, then copy the **ICS** link rather than the HTML one.
- **Apple iCloud**: share the calendar publicly, then change the `webcal://`
  prefix on the resulting address to `https://`.

Treat this address as a credential. It is unauthenticated, so anybody who has
it can read the calendar; that is also why it is worth using a calendar
dedicated to whatever you are automating rather than your personal one.

## What the options change

**Priority** is the Todoist priority given to every task this creates, from 1
(lowest) to 4 (highest). Leave it unset to take Todoist's default.

The filter decides which events get a task. It can match on `summary`,
`description`, `status`, `busy_status`, `start`, `end` and `duration_minutes`.
`status` carries the event's own state (`confirmed`, `tentative`, `cancelled`)
and `busy_status` carries how it shows on your calendar (`free`, `tentative`,
`busy`, `oof`). Because this workflow mirrors rather than appends, changing the
filter later tidies up after itself: events that no longer match have their
tasks completed on the next run.

```
busy_status == "busy" && duration_minutes >= 30
```

## Scheduling

Calendars change under you — meetings move, invitations arrive — so this
defaults to `@hourly` rather than `@daily`. The feed is cheap to fetch and the
work is skipped entirely when nothing has changed.
"#;

crate::register_job!(CalendarWorkflow);
crate::register_workflow_type!(CalendarWorkflow);

impl crate::workflows::ConfigurableWorkflow for CalendarWorkflow {
    type ConfigType = CalendarWorkflowConfig;

    fn type_id() -> &'static str {
        "calendar"
    }

    fn describe(config: &Self::JobType) -> String {
        config.name.clone()
    }

    /// The snapshot of the calendar this mirrors against. Clearing it makes the
    /// next run treat every event in the window as new, so tasks that were
    /// completed by hand come back.
    fn state(config: &Self::ConfigType) -> Vec<StateKey> {
        vec![CalendarCollector::new(&config.url).state()]
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "Calendar".to_string(),
            description: "Files a task for each event in a calendar feed.".to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@hourly".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(CalendarWorkflowConfig: name),
                    "Name",
                    FieldKind::Text { placeholder: Some("Work".into()) },
                )
                .with_help("Used to label the tasks this creates.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(CalendarWorkflowConfig: url),
                    "Calendar URL",
                    FieldKind::Url { placeholder: Some("https://example.com/calendar.ics".into()) },
                )
                .with_help("The address of the iCalendar feed, which most calendars offer as a secret link.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(CalendarWorkflowConfig: priority),
                    "Priority",
                    FieldKind::Number {
                        min: Some(1.0),
                        max: Some(4.0),
                        step: Some(1.0),
                    },
                )
                .with_help("The Todoist priority to give these tasks, from 1 (lowest) to 4 (highest)."),
                FieldDescriptor::new(
                    crate::config_path!(CalendarWorkflowConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "summary".into(),
                            "description".into(),
                            "status".into(),
                            "busy_status".into(),
                            "start".into(),
                            "end".into(),
                            "duration_minutes".into(),
                        ],
                    },
                )
                .with_help("Only file events matching this. Leave it empty to file every event."),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                CalendarWorkflowConfig,
                project = Some("Work"),
                section = None::<&str>
            ))
            .collect(),
        }
    }
}

impl Job for CalendarWorkflow {
    type JobType = CalendarWorkflowConfig;

    fn partition() -> &'static str {
        "calendar/todoist"
    }

    /// Visibility timeout / retry backoff. Calendar sync is cheap and not
    /// heavily rate limited, so a failed run can be retried promptly.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::minutes(5)
    }

    #[instrument("workflow.calendar.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let collector = CalendarCollector::new(&job.url);

        let items = collector.diff(services).await?;

        for item in items.into_iter() {
            match item {
                Diff::Added(id, item) | Diff::Modified(id, item)
                    if job.filter.matches(&item).unwrap_or_default() =>
                {
                    info!(
                        "Calendar item '{}' matched filter, creating Todoist task",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistUpsertTask::dispatch(
                        crate::publishers::TodoistUpsertTaskPayload {
                            unique_key: identifier_string,
                            title: item.summary,
                            description: item.description,
                            priority: job.priority,
                            due: if item.all_day {
                                crate::publishers::TodoistDueDate::Date(item.start.date_naive())
                            } else {
                                crate::publishers::TodoistDueDate::DateTime(item.start)
                            },
                            duration: Some(item.end - item.start),
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
                Diff::Added(id, item) | Diff::Modified(id, item) => {
                    info!(
                        "Calendar item '{}' did not match filter, skipping Todoist creation",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
                Diff::Removed(id) => {
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }
}
