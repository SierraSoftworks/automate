use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::RssCollector,
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

crate::register_job!(RssWorkflow);
crate::register_workflow_type!(RssWorkflow);

impl crate::workflows::ConfigurableWorkflow for RssWorkflow {
    fn type_id() -> &'static str {
        "rss"
    }

    fn describe(config: &Self::JobType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "RSS Feed".to_string(),
            description: "Watches a feed and files a task for each new entry.".to_string(),
            trigger: WorkflowTrigger::Cron,
            fields: vec![
                FieldDescriptor::new(
                    "name",
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
                    "url",
                    "Feed URL",
                    FieldKind::Url {
                        placeholder: Some("https://example.com/rss/".into()),
                    },
                )
                .with_help("The address of the RSS or Atom feed itself, not the page it describes.")
                .required(),
                FieldDescriptor::new(
                    "homepage",
                    "Homepage",
                    FieldKind::Url {
                        placeholder: Some("https://example.com/".into()),
                    },
                )
                .with_help(
                    "Used to resolve relative links inside entries, which many feeds rely on.",
                )
                .required(),
                FieldDescriptor::new("cron", "Schedule", FieldKind::Cron)
                    .with_help("How often to check the feed for new entries.")
                    .with_default("@daily")
                    .required(),
                FieldDescriptor::new(
                    "filter",
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["title".into(), "description".into(), "link".into()],
                    },
                )
                .with_help("Only file entries matching this. Leave it empty to file every entry."),
                FieldDescriptor::new(
                    "todoist.connection",
                    "Todoist account",
                    FieldKind::Connection {
                        provider: crate::publishers::TODOIST_PROVIDER.to_string(),
                    },
                )
                .with_help("Which linked account the tasks are created in.")
                .required(),
                FieldDescriptor::new(
                    "todoist.project",
                    "Project",
                    FieldKind::Options {
                        source: "projects".into(),
                        depends_on: "todoist.connection".into(),
                    },
                )
                .with_default("Hobbies"),
                FieldDescriptor::new(
                    "todoist.section",
                    "Section",
                    FieldKind::Options {
                        source: "sections".into(),
                        depends_on: "todoist.connection".into(),
                    },
                )
                .with_default("Reading"),
            ],
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

    #[instrument("workflow.rss.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.rss, services).await
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
