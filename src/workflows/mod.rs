mod calendar;
mod cron;
mod github_notifications;
mod github_releases;
mod rss;
mod xkcd;
mod youtube;

pub use calendar::CalendarWorkflow;
pub use cron::{CronJob, CronJobConfig};
pub use github_notifications::GitHubNotificationsWorkflow;
pub use github_releases::GitHubReleasesWorkflow;
pub use rss::RssWorkflow;
pub use xkcd::XkcdWorkflow;
pub use youtube::YouTubeWorkflow;
