use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    config::TodoistConfig,
    prelude::*,
    publishers::{TodoistDueDate, TodoistUpsertTask, TodoistUpsertTaskPayload},
};

/// Configuration for a re-authorization reminder. `provider` identifies the
/// OAuth2 provider (the key under `[oauth2.*]` in the configuration) whose
/// refresh token has expired.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct OAuth2ReauthorizationRequiredConfig {
    pub provider: String,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Display for OAuth2ReauthorizationRequiredConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "oauth-reauthorization/{}", self.provider)
    }
}

/// Raises a Todoist reminder telling the user to re-authorize an OAuth2 provider
/// whose refresh token has expired or been revoked. Dispatched by
/// [`crate::web::refresh_or_notify`] when a refresh fails permanently.
#[derive(Clone)]
pub struct OAuth2ReauthorizationRequiredWorkflow;

crate::register_job!(OAuth2ReauthorizationRequiredWorkflow);

impl Job for OAuth2ReauthorizationRequiredWorkflow {
    type JobType = OAuth2ReauthorizationRequiredConfig;

    fn partition() -> &'static str {
        "oauth/reauthorization"
    }

    /// Visibility timeout / retry backoff. Dispatches to the rate-limited
    /// Todoist API, so a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument(
        "oauth.reauthorization.handle",
        skip(self, ctx, job),
        fields(job = %job),
        err(Display)
    )]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let config = services.config().get_oauth2(&job.provider)?;

        let base_url = services.config().web.base_url.clone();
        let authorize_url = reauthorization_url(base_url.as_deref(), &job.provider);

        let mut description = format!(
            "The refresh token for {name} has expired or been revoked, so automate can no longer access this account.\n\n[Re-authorize {name}]({url})",
            name = config.name,
            url = authorize_url,
        );
        if base_url.is_none() {
            // Without a configured base URL the link above is only a path, so
            // point the user at how to make it a clickable absolute link.
            description.push_str(
                "\n\nSet `web.base_url` in your automate configuration so this link is absolute; until then, open the path above on your automate host.",
            );
        }

        // Upsert (keyed by provider) so that repeated detections update the one
        // reminder rather than creating duplicates.
        TodoistUpsertTask::dispatch(
            TodoistUpsertTaskPayload {
                unique_key: format!("oauth-reauth/{}", job.provider),
                title: format!("**automate**: Re-authorize {} access", config.name),
                description: Some(description),
                priority: Some(4),
                due: TodoistDueDate::Today,
                duration: None,
                config: job.todoist.clone(),
            },
            Some(format!("oauth-reauth/{}", job.provider).into()),
            services,
        )
        .await
    }
}

/// Builds the URL a user visits to re-authorize an OAuth2 provider. Prefers an
/// absolute URL built from the configured `web.base_url`; falls back to a
/// relative path when no base URL is configured (a background job has no request
/// context to reconstruct the host from).
fn reauthorization_url(base_url: Option<&str>, provider: &str) -> String {
    match base_url {
        Some(base) => format!("{}/oauth/{}/", base.trim_end_matches('/'), provider),
        None => format!("/oauth/{}/", provider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publishers::TodoistUpsertTaskPayload;
    use crate::web::OAuth2Config;

    #[test]
    fn partition_is_namespaced() {
        assert_eq!(
            OAuth2ReauthorizationRequiredWorkflow::partition(),
            "oauth/reauthorization"
        );
    }

    #[test]
    fn display_doubles_as_idempotency_key() {
        // The Display impl is used as the dispatch idempotency key, so it must
        // remain stable and provider-scoped.
        let config = OAuth2ReauthorizationRequiredConfig {
            provider: "spotify".into(),
            todoist: Default::default(),
        };
        assert_eq!(config.to_string(), "oauth-reauthorization/spotify");
    }

    #[test]
    fn reauthorization_url_is_absolute_when_base_url_is_set() {
        // A trailing slash on the configured base URL must not produce a double
        // slash in the resulting link.
        assert_eq!(
            reauthorization_url(Some("https://automate.example.com/"), "spotify"),
            "https://automate.example.com/oauth/spotify/"
        );
        assert_eq!(
            reauthorization_url(Some("https://automate.example.com"), "spotify"),
            "https://automate.example.com/oauth/spotify/"
        );
    }

    #[test]
    fn reauthorization_url_falls_back_to_relative_path() {
        assert_eq!(
            reauthorization_url(None, "spotify"),
            "/oauth/spotify/"
        );
    }

    fn spotify_oauth_config() -> OAuth2Config {
        OAuth2Config {
            name: "Spotify".into(),
            jobs: Vec::new(),
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
            auth_url: "https://accounts.spotify.com/authorize".into(),
            token_url: "https://accounts.spotify.com/api/token".into(),
            scopes: Vec::new(),
            todoist: Default::default(),
        }
    }

    #[tokio::test]
    async fn handle_enqueues_an_upsert_reminder_with_the_authorize_link() {
        let database = crate::db::SqliteDatabase::open_in_memory().await.unwrap();
        let mut config = crate::config::Config::default();
        config
            .oauth2
            .insert("spotify".into(), spotify_oauth_config());
        config.web.base_url = Some("https://automate.example.com".into());

        let services = crate::services::ServicesContainer::new(config, database);

        let ctx = JobContext::new(services.clone(), chrono::Utc::now(), None, None);
        OAuth2ReauthorizationRequiredWorkflow
            .handle(
                ctx,
                &OAuth2ReauthorizationRequiredConfig {
                    provider: "spotify".into(),
                    todoist: Default::default(),
                },
            )
            .await
            .unwrap();

        let message = services
            .queue()
            .dequeue_any(chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(message.partition, "todoist/upsert-task");

        let payload: TodoistUpsertTaskPayload = serde_json::from_value(message.payload).unwrap();
        assert_eq!(payload.unique_key, "oauth-reauth/spotify");
        assert_eq!(payload.title, "**automate**: Re-authorize Spotify access");
        assert!(
            payload
                .description
                .unwrap()
                .contains("https://automate.example.com/oauth/spotify/"),
            "the reminder should contain the absolute authorization link"
        );
    }

    #[tokio::test]
    async fn handle_carries_the_configured_todoist_settings() {
        // The Todoist configuration attached to the reminder (sourced from the
        // provider's `[oauth2.*].todoist`) must be passed through to the upsert
        // task so it lands in the configured project/section.
        let database = crate::db::SqliteDatabase::open_in_memory().await.unwrap();
        let mut config = crate::config::Config::default();
        config
            .oauth2
            .insert("spotify".into(), spotify_oauth_config());

        let services = crate::services::ServicesContainer::new(config, database);

        let ctx = JobContext::new(services.clone(), chrono::Utc::now(), None, None);
        OAuth2ReauthorizationRequiredWorkflow
            .handle(
                ctx,
                &OAuth2ReauthorizationRequiredConfig {
                    provider: "spotify".into(),
                    todoist: TodoistConfig {
                        project: Some("Accounts".into()),
                        section: Some("Re-authorize".into()),
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();

        let message = services
            .queue()
            .dequeue_any(chrono::Duration::seconds(60))
            .await
            .unwrap();
        let payload: TodoistUpsertTaskPayload = serde_json::from_value(message.payload).unwrap();
        assert_eq!(payload.config.project.as_deref(), Some("Accounts"));
        assert_eq!(payload.config.section.as_deref(), Some("Re-authorize"));
    }
}
