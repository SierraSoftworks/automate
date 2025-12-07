use crate::{services::Services};

mod rss;
mod youtube;
mod xkcd;

pub use rss::Rss;
pub use youtube::YouTube;
pub use xkcd::Xkcd;

pub trait Workflow<S: Services>: std::fmt::Display {
    async fn run(self, services: S) -> Result<(), human_errors::Error>;
}