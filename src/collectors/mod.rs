mod differential;
mod incremental;

mod github_releases;
mod rss;
mod xkcd;
mod youtube;

#[allow(dead_code)]
pub use differential::DifferentialCollector;
pub use incremental::IncrementalCollector;

pub use github_releases::GitHubReleasesCollector;
pub use rss::RssCollector;
pub use xkcd::XkcdCollector;
pub use youtube::YouTubeCollector;

use crate::services::Services;

#[async_trait::async_trait]
pub trait Collector {
    type Item;

    async fn list(
        &self,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error>;
}
