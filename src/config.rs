use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::web::*;
use crate::webhooks::*;
use crate::workflows::*;

#[derive(Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub connections: ConnectionConfigs,
    #[serde(default)]
    pub oauth2: HashMap<String, OAuth2Config>,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub webhooks: WebhookConfigs,
    #[serde(default)]
    pub workflows: WorkflowConfigs,
}

pub trait Mergeable {
    fn merge(&self, other: &Self) -> Self;
}

impl Config {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, human_errors::Error> {
        let path = path.into();
        let contents = std::fs::read_to_string(&path).wrap_err_as_user(
            format!("We could not read your config file '{}'.", path.display()),
            &[
                "Ensure the file exists and is readable.",
                "Check that you have the necessary permissions to read the file.",
            ],
        )?;
        let config: Config = toml::from_str(&contents).wrap_err_as_user(
            "Your configuration file could not be loaded.",
            &[
                "Ensure that the file is valid TOML.",
                "Make sure that you are using the correct configuration file format.",
            ],
        )?;
        Ok(config)
    }

    pub fn get_oauth2(&self, kind: &str) -> Result<OAuth2Config, human_errors::Error> {
        self.oauth2.get(kind).cloned().ok_or_else(|| {
            human_errors::user(
                format!("OAuth configuration for kind '{}' not found.", kind),
                &[
                    "Ensure that the OAuth configuration is present in your config file.",
                    "Check that the kind value is correct.",
                ],
            )
        })
    }
}

#[derive(Default, Clone, Deserialize)]
pub struct ConnectionConfigs {
    #[serde(default)]
    pub todoist: TodoistConfig,

    #[serde(default)]
    pub github: GitHubConfig,
}

#[derive(Clone, Deserialize, Default)]
pub struct WebConfig {
    #[serde(default = "default_listen_address")]
    pub address: String,

    #[serde(default)]
    pub admin_acl: Filter,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

fn default_listen_address() -> String {
    "localhost:8080".to_string()
}

#[derive(Clone, Deserialize, Default)]
pub struct WebhookConfigs {
    #[serde(default)]
    pub azure_monitor: AzureMonitorWebhookConfig,

    #[serde(default)]
    pub grafana: GrafanaWebhookConfig,

    #[serde(default)]
    pub honeycomb: HoneycombWebhookConfig,

    #[serde(default)]
    pub sentry: SentryWebhookConfig,

    #[serde(default)]
    pub tailscale: TailscaleWebhookConfig,

    #[serde(default)]
    pub terraform: TerraformWebhookConfig,
}

#[derive(Clone, Deserialize, Default)]
pub struct WorkflowConfigs {
    #[serde(default)]
    pub calendars: Vec<CronJobConfig<CalendarWorkflow>>,
    #[serde(default)]
    pub github_notifications: Vec<CronJobConfig<GitHubNotificationsWorkflow>>,
    #[serde(default)]
    pub github_notifications_cleanup: CronJobConfig<GitHubNotificationsCleanupWorkflow>,
    #[serde(default)]
    pub github_releases: Vec<CronJobConfig<GitHubReleasesWorkflow>>,
    #[serde(default)]
    pub rss: Vec<CronJobConfig<RssWorkflow>>,
    #[serde(default)]
    pub youtube: Vec<CronJobConfig<YouTubeWorkflow>>,
    #[serde(default)]
    pub xkcd: Vec<CronJobConfig<XkcdWorkflow>>,
}

#[derive(Default, Clone, Deserialize)]
pub struct GitHubConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TodoistConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl Mergeable for TodoistConfig {
    fn merge(&self, other: &Self) -> Self {
        TodoistConfig {
            api_key: other.api_key.clone().or_else(|| self.api_key.clone()),
            project: other.project.clone().or_else(|| self.project.clone()),
            section: other.section.clone().or_else(|| self.section.clone()),
        }
    }
}
