use std::borrow::Cow;

use crate::services::Services;

mod todoist;

pub use todoist::{TodoistPublisher, TodoistConfig};

pub trait Publisher {
    type Item;
    type Config: PublisherConfig;

    fn kind(&self) -> Cow<'static, str>;

    fn partition<S: Into<Cow<'static, str>>>(&self, namespace: Option<S>) -> String {
        if let Some(ns) = namespace {
            format!("publisher::{}::{}", self.kind(), ns.into())
        } else {
            format!("publisher::{}", self.kind())
        }
    }

    fn key(&self) -> Cow<'static, str>;

    async fn publish(&self, item: Self::Item, config: Self::Config, services: &impl Services) -> Result<(), human_errors::Error>;
}

pub trait PublisherConfig: Default {
    fn merge(&self, other: &Self) -> Self;
}