use std::fmt::Display;

use serde::{Deserialize};

use crate::{collectors::{Collector, XkcdCollector, XkcdItem}, filter::{Filter, Filterable}, publishers::{Publisher, PublisherConfig, TodoistConfig, TodoistPublisher}, services::Services, workflows::Workflow};

#[derive(Clone, Deserialize)]
pub struct Xkcd {
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
        section: Some("Reading".into()),
        priority: Some(2),
        ..Default::default()
    }
}

impl Display for Xkcd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xkcd")
    }
}

impl<S: Services + Clone + Send + Sync + 'static> Workflow<S> for Xkcd {
    async fn run(self, services: S) -> Result<(), human_errors::Error> {
        let Xkcd{ filter, cron, todoist } = self.clone();

        let todoist = services.connections().todoist.merge(&default_todoist_config()).merge(&todoist);

        crate::engines::cron(format!("{}", &self), cron, services, async move |services| {
            let collector = XkcdCollector::new();
            let publisher = TodoistPublisher;

            let items = collector.list(&services).await?;

            for item in items.into_iter() {
                match filter.matches(&XkcdEntryFilter(&item)) {
                    Ok(false) => continue,
                    Err(err) => {
                        return Err(err);
                    }
                    _ => {}
                }

                publisher.publish(todoist_api::CreateTaskArgs {
                    content: format!("[XKCD]({}): {}", &item.url, item.title),
                    description: item.image_url.map(|url| format!("![XKCD]({})\n\n*{}*", url, item.image_alt.unwrap_or_default())),
                    due_string: Some("today".into()),
                    ..Default::default()
                }, todoist.clone(), &services).await?;
            }

            Ok(())
        }).await
    }
}

struct XkcdEntryFilter<'a>(&'a XkcdItem);

impl<'a> Filterable for XkcdEntryFilter<'a> {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "title" => self.0.title.clone().into(),
            "published" => self.0.published.to_rfc3339().into(),
            "link" => self.0.url.clone().into(),
            "has_image" => self.0.image_url.is_some().into(),       
            _ => crate::filter::FilterValue::Null,
        }
    }
}
