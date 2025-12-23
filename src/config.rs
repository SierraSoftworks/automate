use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// Loads environment variables from a .env file if it exists.
    /// Variables from the .env file will override process-level environment variables.
    pub fn load_env_file(path: impl AsRef<Path>) -> Result<(), human_errors::Error> {
        let path = path.as_ref();

        if !path.exists() {
            // It's okay if the file doesn't exist
            return Ok(());
        }

        dotenvy::from_path_override(path).wrap_err_as_user(
            format!(
                "We could not load your environment file '{}'.",
                path.display()
            ),
            &[
                "Ensure the file is in the correct .env format (KEY=value).",
                "Check that you have the necessary permissions to read the file.",
            ],
        )?;

        Ok(())
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, human_errors::Error> {
        let path = path.into();
        let contents = std::fs::read_to_string(&path).wrap_err_as_user(
            format!("We could not read your config file '{}'.", path.display()),
            &[
                "Ensure the file exists and is readable.",
                "Check that you have the necessary permissions to read the file.",
            ],
        )?;

        // Interpolate environment variables before parsing TOML
        let contents = crate::parsers::interpolate(&contents, |expr| {
            let expr = expr.trim();
            if let Some(var_name) = expr.strip_prefix("env.") {
                Ok(std::env::var(var_name).unwrap_or_else(|_| format!("${{{{ {} }}}}", expr)))
            } else {
                Err(human_errors::user(
                    format!("Unknown interpolation expression: '{}'", expr),
                    &[
                        "Currently, only 'env.VARIABLE_NAME' expressions are supported.",
                        "Use '\\${{ ... }}' to escape literal text that looks like an expression.",
                    ],
                ))
            }
        })?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_env_file_with_valid_file() {
        let temp_dir = std::env::temp_dir();
        let env_file = temp_dir.join(format!("test_env_{}.env", uuid::Uuid::new_v4()));

        // Create a test .env file
        let mut file = std::fs::File::create(&env_file).unwrap();
        writeln!(file, "TEST_VAR_1=value1").unwrap();
        writeln!(file, "TEST_VAR_2=value2").unwrap();
        writeln!(file, "# Comment line").unwrap();
        writeln!(file, "TEST_VAR_3=\"value with spaces\"").unwrap();
        drop(file);

        // Load the env file
        Config::load_env_file(&env_file).unwrap();

        // Verify the variables were loaded
        assert_eq!(std::env::var("TEST_VAR_1").unwrap(), "value1");
        assert_eq!(std::env::var("TEST_VAR_2").unwrap(), "value2");
        assert_eq!(std::env::var("TEST_VAR_3").unwrap(), "value with spaces");

        // Cleanup
        std::fs::remove_file(&env_file).ok();
    }

    #[test]
    fn test_load_env_file_nonexistent_file() {
        let temp_dir = std::env::temp_dir();
        let env_file = temp_dir.join("nonexistent.env");

        // Should not error when file doesn't exist
        let result = Config::load_env_file(&env_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_env_interpolation_in_config() {
        let temp_dir = std::env::temp_dir();
        let config_file = temp_dir.join(format!("test_config_{}.toml", uuid::Uuid::new_v4()));
        let env_file = temp_dir.join(format!("test_env_{}.env", uuid::Uuid::new_v4()));

        // Create a test .env file
        let mut file = std::fs::File::create(&env_file).unwrap();
        writeln!(file, "TEST_API_KEY=secret123").unwrap();
        drop(file);

        // Create a test config file with interpolation
        let mut file = std::fs::File::create(&config_file).unwrap();
        writeln!(file, "[connections.github]").unwrap();
        writeln!(file, "api_key = \"${{{{ env.TEST_API_KEY }}}}\"").unwrap();
        drop(file);

        // Load env file and config
        Config::load_env_file(&env_file).unwrap();
        let config = Config::load(&config_file).unwrap();

        // Verify interpolation worked
        assert_eq!(
            config.connections.github.api_key.as_deref(),
            Some("secret123")
        );

        // Cleanup
        std::fs::remove_file(&config_file).ok();
        std::fs::remove_file(&env_file).ok();
    }
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
