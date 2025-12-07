use std::path::PathBuf;

use human_errors::ResultExt;
use serde::{Deserialize};
use tracing_batteries::prelude::info;

use crate::{publishers::TodoistConfig, workflows::Workflow};

#[derive(Clone, Deserialize)]
pub struct Config {
    pub connections: ConnectionConfigs,
    pub workflows: WorkflowConfigs,
}

impl Config {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, human_errors::Error> {
        let path = path.into();
        let contents = std::fs::read_to_string(&path)
            .wrap_err_as_user(
                format!("We could not read your config file '{}'.", path.display()),
                &[
                    "Ensure the file exists and is readable.",
                    "Check that you have the necessary permissions to read the file.",
                ]
            )?;
        let config: Config = toml::from_str(&contents)
            .wrap_err_as_user(
                "Your configuration file could not be loaded.",
                &[
                    "Ensure that the file is valid TOML.",
                    "Make sure that you are using the correct configuration file format."
                ]
            )?;
        Ok(config)
    }
}

#[derive(Default, Clone, Deserialize)]
pub struct ConnectionConfigs {
    pub todoist: TodoistConfig,
    pub github: GitHubConfig,
}

#[derive(Clone, Deserialize)]
pub struct WorkflowConfigs {
    pub github_releases: Vec<crate::workflows::GitHubReleases>,
    pub rss: Vec<crate::workflows::Rss>,
    pub youtube: Vec<crate::workflows::YouTube>,
    pub xkcd: Option<crate::workflows::Xkcd>,
}

impl WorkflowConfigs {
    pub async fn run_all(self, services: impl crate::services::Services + Clone + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        let mut join_handles = vec![];

        for workflow in self.github_releases {
            info!("Registered workflow {}", &workflow);
            join_handles.push(tokio::spawn(workflow.run(services.clone())));
        }

        for workflow in self.rss {
            info!("Registered workflow {}", &workflow);
            join_handles.push(tokio::spawn(workflow.run(services.clone())));
        }

        for workflow in self.youtube {
            info!("Registered workflow {}", &workflow);
            join_handles.push(tokio::spawn(workflow.run(services.clone())));
        }
        
        if let Some(xkcd_workflow) = self.xkcd {
            info!("Registered workflow {}", &xkcd_workflow);
            join_handles.push(tokio::spawn(xkcd_workflow.run(services.clone())));
        }

        for handle in join_handles {
            handle.await.map_err_as_system(&[
                "Please report this failure to the development team on GitHub."
            ])??;
        }

        Ok(())
    }
}

#[derive(Default, Clone, Deserialize)]
pub struct GitHubConfig {
    #[serde(default)]
    pub api_key: Option<String>,
}