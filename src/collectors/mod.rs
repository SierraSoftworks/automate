mod differential;
mod incremental;

mod github_releases;
mod rss;
mod youtube;
mod xkcd;

pub use differential::DifferentialCollector;
pub use incremental::IncrementalCollector;

pub use github_releases::{GitHubReleasesCollector, GitHubReleaseItem};
pub use rss::RssCollector;
pub use youtube::{YouTubeCollector, YouTubeItem};
pub use xkcd::{XkcdCollector, XkcdItem};

use crate::services::Services;

#[async_trait::async_trait]
pub trait Collector {
    type Item;

    async fn list(&self, services: &(impl Services + Send + Sync + 'static)) -> Result<Vec<Self::Item>, human_errors::Error>;
}