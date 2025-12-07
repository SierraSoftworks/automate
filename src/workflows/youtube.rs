use std::fmt::Display;

use serde::Deserialize;

use super::{Workflow, Services};
use crate::collectors::*;
use crate::filter::{Filter, Filterable};
use crate::publishers::*;

#[derive(Clone, Deserialize)]
pub struct YouTube {
    pub name: String,
    pub channel_id: String,

    #[serde(default = "default_cron")]
    pub cron: croner::Cron,

    #[serde(default)]
    filter: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

fn default_cron() -> croner::Cron {
    "@hourly".parse().unwrap()
}


fn default_todoist_config() -> TodoistConfig {
    TodoistConfig {
        project: Some("Hobbies".into()),
        section: Some("Movies and Series".into()),
        priority: Some(2),
        ..Default::default()
    }
}

impl Display for YouTube {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "youtube/{}", self.name)
    }
}

impl<S: Services + Clone + Send + Sync + 'static> Workflow<S> for YouTube {
    async fn run(self, services: S) -> Result<(), human_errors::Error> {
        let YouTube{ name, channel_id, cron, filter, todoist } = self.clone();
        let todoist = services.connections().todoist.merge(&default_todoist_config()).merge(&todoist);

        crate::engines::cron(format!("{}", &self), cron, services, async move |services| {
            let collector = YouTubeCollector::new(&channel_id);
            let publisher = TodoistPublisher;

            let items = collector.list(&services).await?;

            for item in items.into_iter() {
                match filter.matches(&YouTubeEntryFilter(&item)) {
                    Ok(false) => continue,
                    Err(err) => {
                        return Err(err);
                    }
                    _ => {}
                }

                publisher.publish(todoist_api::CreateTaskArgs {
                    content: format!("[{}]({}): {}", if item.channel.is_empty() { &name } else { &item.channel }, item.link, item.title),
                    due_string: Some("today".into()),
                    ..Default::default()
                }, todoist.clone(), &services).await?;
            }

            Ok(())
        }).await
    }
}


struct YouTubeEntryFilter<'a>(&'a YouTubeItem);

impl<'a> Filterable for YouTubeEntryFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "channel" => self.0.channel.clone().into(),
            "title" => self.0.title.clone().into(),
            "link" => self.0.link.clone().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}