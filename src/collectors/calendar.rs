use serde::{Deserialize, Serialize};

use crate::{
    collectors::{Diff, DifferentialCollector},
    parsers::{Calendar, CalendarEvent},
    prelude::*,
};

pub struct CalendarCollector {
    pub url: String,
}

impl CalendarCollector {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CalendarEventIdentifier {
    uid: String,
    start: chrono::DateTime<chrono::Utc>,
}

#[async_trait::async_trait]
impl Collector for CalendarCollector {
    type Item = CalendarEvent;

    #[instrument("collectors.calendar.list", skip(self, services), err(Display))]
    async fn list(
        &self,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let results = self.diff(services).await?;

        Ok(results
            .into_iter()
            .filter_map(|d| match d {
                Diff::Added(_, item) => Some(item),
                _ => None,
            })
            .collect())
    }
}

impl DifferentialCollector for CalendarCollector {
    type Identifier = CalendarEventIdentifier;

    fn kind(&self) -> &'static str {
        "calendar"
    }

    fn key(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.url.clone())
    }

    fn identifier(&self, item: &Self::Item) -> Self::Identifier {
        CalendarEventIdentifier {
            uid: item.uid.clone(),
            start: item.start,
        }
    }

    #[instrument("collectors.calendar.fetch", skip(self), err(Display))]
    async fn fetch(&self) -> Result<Vec<Self::Item>, human_errors::Error> {
        let client = reqwest::Client::builder()
            .user_agent("SierraSoftworks/automate-rs")
            .build()
            .or_system_err(&["Report this issue to the development team on GitHub."])?;

        let response = client
            .get(&self.url)
            .header("Accept", "text/calendar")
            .send()
            .await
            .wrap_user_err(
                "We were unable to fetch your calendar.",
                &[
                    "Make sure that your network connection is working properly.",
                    "Make sure you provided a valid URL for your calendar.",
                ],
            )?;

        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_FOUND => {
                return Err(human_errors::user(
                    "The calendar URL you provided returned a 404 Not Found response when queried.",
                    &[
                        "Ensure that you have provided a valid URL for your calendar.",
                        "Check that the calendar is publicly accessible.",
                    ],
                ));
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(human_errors::user(
                    "Authorization failed when trying to fetch your calendar.",
                    &["Make sure that your calendar is marked as publicly accessible."],
                ));
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(human_errors::user(
                    "Rate limit exceeded when trying to fetch your calendar.",
                    &["Wait for a while before making more requests to your calendar's URL."],
                ));
            }
            status => {
                return Err(human_errors::user(
                    format!(
                        "Failed to fetch your calendar. Received unexpected status code: {}",
                        status
                    ),
                    &["Make sure that your network connection is working properly."],
                ));
            }
        }

        let content = response.text().await.or_user_err(&[
            "Make sure that you have provided a valid URL for your calendar.",
        ])?;

        let calendar: Calendar = content.parse()?;

        let now = chrono::Utc::now();
        let start = now;
        let end = now + chrono::Duration::days(7);
        let events = calendar.events(start, end)?;

        Ok(events)
    }
}
