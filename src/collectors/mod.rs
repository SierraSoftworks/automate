mod differential;
mod incremental;

mod rss;
mod youtube;
mod xkcd;

pub use differential::DifferentialCollector;
pub use incremental::IncrementalCollector;

pub use rss::RssCollector;
pub use youtube::YouTubeCollector;
pub use xkcd::{XkcdCollector, XkcdItem};

