use std::fmt::Display;

use serde::{Deserialize};

use crate::{collectors::{Collector, GitHubReleasesCollector, GitHubReleaseItem}, filter::{Filter, Filterable}, publishers::{Publisher, PublisherConfig, TodoistConfig, TodoistPublisher}, services::Services, workflows::Workflow};

#[derive(Clone, Deserialize)]
pub struct GitHubReleases {
    pub repository: String,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_cron")]
    pub cron: croner::Cron,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

fn default_cron() -> croner::Cron {
    "@hourly".parse().unwrap()
}

fn default_todoist_config() -> TodoistConfig {
    TodoistConfig {
        project: Some("Hobbies".into()),
        section: Some("Open Source".into()),
        priority: Some(2),
        ..Default::default()
    }
}

impl Display for GitHubReleases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-releases/{}", self.repository)
    }
}

impl<S: Services + Clone + Send + Sync + 'static> Workflow<S> for GitHubReleases {
    async fn run(self, services: S) -> Result<(), human_errors::Error> {
        let GitHubReleases{ repository, filter, cron, todoist } = self.clone();

        let todoist = services.connections().todoist.merge(&default_todoist_config()).merge(&todoist);

        crate::engines::cron(format!("{}", &self), cron, services, async move |services| {
            let collector = GitHubReleasesCollector::new(&repository);
            let publisher = TodoistPublisher;

            let items = collector.list(&services).await?;

            for item in items.into_iter() {
                match filter.matches(&GitHubReleaseEntryFilter(&item)) {
                    Ok(false) => continue,
                    Err(err) => {
                        return Err(err);
                    }
                    _ => {}
                }

                publisher.publish(todoist_api::CreateTaskArgs {
                    content: format!("[github:{}]({}): Released {} ({})", &repository, &item.html_url, item.name, item.tag_name),
                    description: item.body.map(|body| crate::parsers::html_to_markdown(&body, "https://github.com/".parse().unwrap())),
                    due_string: Some("today".into()),
                    ..Default::default()
                }, todoist.clone(), &services).await?;
            }

            Ok(())
        }).await
    }
}

struct GitHubReleaseEntryFilter<'a>(&'a GitHubReleaseItem);

impl<'a> Filterable for GitHubReleaseEntryFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "tag" => self.0.tag_name.clone().into(),
            "name" => self.0.name.clone().into(),
            "published" => self.0.published_at.to_rfc3339().into(),
            "link" => self.0.html_url.clone().into(),
            "draft" => self.0.draft.into(),
            "prerelease" => self.0.prerelease.into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}
