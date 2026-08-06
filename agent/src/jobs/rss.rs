use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{IncrementalCollector, RssCollector},
    db::StateKey,
    prelude::*,
    publishers::TodoistTarget,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct RssConfig {
    pub name: String,
    pub homepage: String,
    pub url: String,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

fn default_todoist_config() -> TodoistTarget {
    TodoistTarget {
        project: Some("Hobbies".into()),
        section: Some("Reading".into()),
        ..Default::default()
    }
}

impl Display for RssConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rss/{}", self.name)
    }
}

#[derive(Clone)]
pub struct RssWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Every time this runs it fetches the feed, looks at the entries published since
the last run, and files a Todoist task for each one it has not seen before. The
task's title links to the entry and its body is the entry's summary converted
from HTML to Markdown, so most of the time you can decide whether to read
something without leaving your task list.

Nothing is filed twice: the publication date of the newest entry seen is
remembered between runs, and only entries newer than that are considered. The
first run is the exception — it files everything currently in the feed, which
for a feed that keeps fifty entries means fifty tasks. Save it, let it run
once, and clear out the backlog if you did not want it.

That watermark is kept against the feed's address rather than against the
workflow, so two workflows watching the same feed will not each get a copy of
every entry; whichever runs first takes them. If you want the same feed sorted
into two places, use one workflow and a filter rather than two workflows.

## Getting the feed address

The **Feed URL** is the address of the RSS or Atom document, not the address of
the site it describes. Those are usually different: `https://example.com/blog/`
is a page, `https://example.com/blog/feed.xml` is the feed. Most sites either
link theirs from the footer or advertise it in the page's `<head>` as a
`<link rel="alternate" type="application/rss+xml">` element, which is what
feed readers look for.

The **Homepage** is the site's own address. It is used to turn the relative
links many feeds put inside their entries (`/posts/2024/thing`) into ones you
can actually click, so getting it wrong shows up as broken links in your tasks
rather than as an error.

## Choosing which entries to file

The filter runs against each entry, and an entry is only filed if it matches.
It can match on `title`, `description` and `link`, all lower-cased before
comparison. Leaving it empty files every entry.

```
title contains "release" || link contains "/security/"
```

## Polling, not pushing

This is a scheduled workflow: it asks the feed for new entries rather than
being told about them. The schedule decides how quickly something shows up, so
a feed you want to see promptly is worth putting on `@hourly`, and a feed that
publishes weekly is not.
"#;

crate::register_job!(RssWorkflow);
crate::register_workflow_type!(RssWorkflow);

impl crate::workflows::ConfigurableWorkflow for RssWorkflow {
    type ConfigType = RssConfig;

    fn type_id() -> &'static str {
        "rss"
    }

    fn describe(config: &Self::JobType) -> String {
        config.name.clone()
    }

    /// The feed's watermark, which is kept against the feed's address rather
    /// than this workflow — so resetting here re-files the backlog for anything
    /// else watching the same feed too.
    fn state(config: &Self::ConfigType) -> Vec<StateKey> {
        vec![RssCollector::new(&config.url).state()]
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "RSS Feed".to_string(),
            description: "Watches a feed and files a task for each new entry.".to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@daily".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(RssConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Citation Needed".into()),
                    },
                )
                .with_help(
                    "Used to label the tasks this creates, so you can tell them apart at a glance.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(RssConfig: url),
                    "Feed URL",
                    FieldKind::Url {
                        placeholder: Some("https://example.com/rss/".into()),
                    },
                )
                .with_help("The address of the RSS or Atom feed itself, not the page it describes.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(RssConfig: homepage),
                    "Homepage",
                    FieldKind::Url {
                        placeholder: Some("https://example.com/".into()),
                    },
                )
                .with_help(
                    "Used to resolve relative links inside entries, which many feeds rely on.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(RssConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["title".into(), "description".into(), "link".into()],
                    },
                )
                .with_help("Only file entries matching this. Leave it empty to file every entry."),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                RssConfig,
                project = Some("Hobbies"),
                section = Some("Reading")
            ))
            .collect(),
        }
    }
}

impl Job for RssWorkflow {
    type JobType = RssConfig;

    fn partition() -> &'static str {
        "rss/todoist"
    }

    /// Visibility timeout / retry backoff. Polls third-party RSS feeds that can
    /// rate limit or throttle, so a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument("workflow.rss.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let base_url: reqwest::Url = job.homepage.parse().wrap_user_err(
            format!("The feed URL you provided could not be parsed as a valid URL ({}).", &job.homepage),
            &[
                "Ensure that the feed URL is correctly formatted, it should be a fully qualified URL (including the scheme, e.g., https://).",
            ])?;

        let collector = RssCollector::new(&job.url);

        let items = collector.list(services).await?;

        for item in items.into_iter() {
            match job.filter.matches(&RssEntryFilter(&item)) {
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
                        &job.name,
                        &item.links[0].href,
                        item.title
                            .as_ref()
                            .map(|t| t.content.as_str())
                            .unwrap_or("New article")
                    ),
                    description: item
                        .summary
                        .as_ref()
                        .map(|s| html_escape::decode_html_entities(&s.content))
                        .map(|html| {
                            crate::parsers::html_to_markdown(
                                &html,
                                item.links[0]
                                    .href
                                    .parse()
                                    .unwrap_or_else(|_| base_url.clone()),
                            )
                        }),
                    due: TodoistDueDate::Today,
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

struct RssEntryFilter<'a>(&'a feed_rs::model::Entry);

impl<'a> Filterable for RssEntryFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "title" => self
                .0
                .title
                .as_ref()
                .map(|t| t.content.to_lowercase())
                .unwrap_or_default()
                .into(),
            "description" => self
                .0
                .summary
                .as_ref()
                .map(|s| s.content.to_lowercase())
                .unwrap_or_default()
                .into(),
            "link" => self
                .0
                .links
                .first()
                .map(|l| l.href.to_lowercase())
                .unwrap_or_default()
                .into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}
