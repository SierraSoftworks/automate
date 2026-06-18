mod calendar;
mod cron;
mod github_notifications;
mod github_notifications_cleanup;
mod github_releases;
mod oauth_reauthorization;
mod rss;
mod spotify_yearly_playlist;
mod todoist_cleanup;
mod xkcd;
mod ynab_stocks;
mod youtube;

pub use calendar::CalendarWorkflow;
pub use cron::{CronJob, CronJobConfig};
pub use github_notifications::GitHubNotificationsWorkflow;
pub use github_notifications_cleanup::GitHubNotificationsCleanupWorkflow;
pub use github_releases::GitHubReleasesWorkflow;
pub use oauth_reauthorization::{
    OAuth2ReauthorizationRequiredConfig, OAuth2ReauthorizationRequiredWorkflow,
};
pub use rss::RssWorkflow;
pub use xkcd::XkcdWorkflow;
pub use ynab_stocks::YnabStocksWorkflow;
pub use youtube::YouTubeWorkflow;
