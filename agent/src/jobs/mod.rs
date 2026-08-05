mod calendar;
mod cron;
mod github_attention;
mod github_auto_merge;
mod github_notifications;
mod github_notifications_cleanup;
mod github_notifications_refresh;
mod github_releases;
mod oauth_reauthorization;
mod rss;
mod spotify_yearly_playlist;
mod todoist_cleanup;
mod webhook_todoist;
mod xkcd;
mod ynab_stocks;
mod youtube;

pub use calendar::CalendarWorkflow;
pub use cron::{CRON_PARTITION, CronJob, CronJobConfig, CronJobTask};
pub use github_attention::{
    DEFAULT_ASSIGNMENT_FILTER, DEFAULT_COMMENT_FILTER, DEFAULT_SECURITY_ALERT_FILTER,
    GitHubAttentionConfig, GitHubAttentionTask, GitHubAttentionWorkflow, subject_key,
};
pub use github_auto_merge::{
    DEFAULT_APPROVAL_MESSAGE, DEFAULT_AUTO_MERGE_FILTER, GitHubAutoMergeConfig,
    GitHubAutoMergeTask, GitHubAutoMergeWorkflow,
};
pub use github_notifications::GitHubNotificationsWorkflow;
pub use github_notifications_cleanup::GitHubNotificationsCleanupWorkflow;
pub use github_notifications_refresh::GitHubNotificationsRefreshWorkflow;
pub use github_releases::{GitHubReleasesConfig, GitHubReleasesWorkflow};
pub use oauth_reauthorization::{
    OAuth2ReauthorizationRequiredConfig, OAuth2ReauthorizationRequiredWorkflow,
};
pub use rss::RssWorkflow;
pub use spotify_yearly_playlist::{SpotifyYearlyPlaylistTask, SpotifyYearlyPlaylistWorkflow};
pub use xkcd::XkcdWorkflow;
pub use ynab_stocks::YnabStocksWorkflow;
pub use youtube::YouTubeWorkflow;
