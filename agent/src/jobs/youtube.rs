use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::YouTubeCollector,
    prelude::*,
    publishers::TodoistTarget,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct YouTubeConfig {
    pub name: String,
    pub channel_id: String,

    #[serde(default)]
    filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for YouTubeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "youtube/{}", self.name)
    }
}

#[derive(Clone)]
pub struct YouTubeWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r##"## What this does

Every time this runs it reads the channel's public feed and files a Todoist
task for each video published since the last run. The task's title links
straight to the video, so the task list doubles as a watch-later queue you
already look at.

YouTube's feed only carries the most recent handful of uploads, so a channel
that posts more often than this workflow runs will lose the ones in between.
A daily schedule is fine for most channels; a channel that uploads several
times a day wants `@hourly`.

The first run files everything the feed currently carries rather than only what
arrives afterwards, so expect a small burst of tasks immediately after saving.
Every run after that only sees what is new.

## Finding the channel id

The **Channel ID** is the channel's own identifier and always starts with
`UC` — for example `UCy0tKL1T7wFoYcxCe0xjN6Q`. It is not the `@handle` and not
the vanity name, both of which YouTube lets a creator change; the id does not
change, which is why we ask for it instead.

Where to find it:

- Open the channel and look at the address. A URL of the form
  `youtube.com/channel/UC...` contains the id directly.
- If the address uses a handle (`youtube.com/@SomeChannel`) instead, open the
  channel's **About** panel and use **Share channel → Copy channel ID**.
- Failing both, view the page source and search for `channelId`.

Given an id, the feed this reads is
`https://www.youtube.com/feeds/videos.xml?channel_id=UC...`, which is worth
opening once to confirm you have the right channel before you save.

## Choosing which videos to file

The filter runs against each video and can match on `channel`, `title` and
`link`. Leaving it empty files every video, which is usually what you want for
a channel you subscribed to deliberately.

```
!(title contains "#shorts")
```
"##;

crate::register_job!(YouTubeWorkflow);
crate::register_workflow_type!(YouTubeWorkflow);

impl crate::workflows::ConfigurableWorkflow for YouTubeWorkflow {
    type ConfigType = YouTubeConfig;

    fn type_id() -> &'static str {
        "youtube"
    }

    fn describe(config: &Self::JobType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "YouTube Channel".to_string(),
            description: "Files a task for each new video on a channel.".to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@daily".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(YouTubeConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Technology Connections".into()),
                    },
                )
                .with_help("Used to label the tasks this creates.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(YouTubeConfig: channel_id),
                    "Channel ID",
                    FieldKind::Text {
                        placeholder: Some("UCy0tKL1T7wFoYcxCe0xjN6Q".into()),
                    },
                )
                .with_help("The channel's identifier, which appears in its URL and starts with UC.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(YouTubeConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["channel".into(), "title".into(), "link".into()],
                    },
                )
                .with_help("Only file videos matching this. Leave it empty to file every video."),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                YouTubeConfig,
                project = Some("Hobbies"),
                section = Some("Watching")
            ))
            .collect(),
        }
    }
}

impl Job for YouTubeWorkflow {
    type JobType = YouTubeConfig;

    fn partition() -> &'static str {
        "youtube/todoist"
    }

    /// Visibility timeout / retry backoff. Polls YouTube feeds that can rate
    /// limit, so a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument("workflow.youtube.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let collector = YouTubeCollector::new(&job.channel_id);

        let items = collector.list(services).await?;

        for item in items.into_iter() {
            match job.filter.matches(&item) {
                Ok(false) => continue,
                Err(err) => {
                    return Err(err);
                }
                _ => {}
            }

            TodoistCreateTask::dispatch(
                TodoistCreateTaskPayload {
                    title: format!(
                        "[{}]({}): {}",
                        if item.channel.is_empty() {
                            &job.name
                        } else {
                            &item.channel
                        },
                        item.link,
                        item.title
                    ),
                    due: crate::publishers::TodoistDueDate::Today,
                    config: job.todoist.clone(),
                    ..Default::default()
                },
                None,
                services,
            )
            .await?;
        }

        Ok(())
    }
}
