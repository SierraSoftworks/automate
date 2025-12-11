use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::RssCollector,
    config::TodoistConfig,
    prelude::*,
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
    pub todoist: TodoistConfig,
}

fn default_todoist_config() -> TodoistConfig {
    TodoistConfig {
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

impl Job for RssWorkflow {
    type JobType = RssConfig;

    fn partition() -> &'static str {
        "workflow/rss-todoist"
    }

    #[instrument("workflow.rss.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let base_url: reqwest::Url = job.homepage.parse().wrap_err_as_user(
            format!("The feed URL you provided could not be parsed as a valid URL ({}).", &job.homepage),
            &[
                "Ensure that the feed URL is correctly formatted, it should be a fully qualified URL (including the scheme, e.g., https://).",
            ])?;

        let collector = RssCollector::new(&job.url);

        let items = collector.list(&services).await?;

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
                        urlencoding::encode(&item.links[0].href),
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
                &services,
            )
            .await?;
        }

        Ok(())
    }
}

struct RssEntryFilter<'a>(&'a feed_rs::model::Entry);

impl<'a> Filterable for RssEntryFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
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
                .links.first()
                .map(|l| l.href.to_lowercase())
                .unwrap_or_default()
                .into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}
