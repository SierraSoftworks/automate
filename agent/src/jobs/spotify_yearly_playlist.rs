use automate_api::{ConnectionId, ConnectionStatus};
use chrono::Datelike;

use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::db::StateKey;
use crate::{prelude::*, publishers::SpotifyClient};

/// The provider identifier Spotify connections are stored under.
pub const SPOTIFY_PROVIDER: &str = "spotify";

/// Which linked Spotify account to file liked songs for, and what to call the
/// playlists they are filed into.
///
/// The credential itself is deliberately not carried here. It lives encrypted in
/// the connection, so a stored workflow names the account rather than holding
/// the means to use it - which also means a refreshed token is written back to
/// one place instead of riding along in every copy of the configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct SpotifyYearlyPlaylistConfig {
    pub connection: ConnectionId,

    #[serde(default = "default_playlist_name")]
    pub playlist_name: String,

    #[serde(default = "default_playlist_description")]
    pub playlist_description: String,
}

fn default_playlist_name() -> String {
    "{year} Liked Songs".to_string()
}

fn default_playlist_description() -> String {
    "A yearly playlist of all my liked songs from {year}.".to_string()
}

/// Substitutes the one placeholder these templates support.
fn render(template: &str, year: i32) -> String {
    template.replace("{year}", &year.to_string())
}

impl std::fmt::Display for SpotifyYearlyPlaylistConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "spotify-yearly-playlist/{}", self.connection)
    }
}

#[derive(Clone)]
pub struct SpotifyYearlyPlaylistWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Watches the songs you like on Spotify and files each one into a playlist for the
year you liked it in, creating that playlist the first time a song needs it.

Songs are grouped by *when you liked them*, not when they were released, so a
1974 record you found last week lands in this year's playlist.

## Setting one up

Link the Spotify account under Connections first, then choose it here. That
authorisation is what lets us read your liked songs and create playlists on your
behalf; playlists this creates are private.

The first run files every song you have ever liked, which for a well-used account
is several playlists at once. Every run after that only sees what you have liked
since, so the burst happens once.

Songs you unlike are not removed from the playlists they were already filed into.
A yearly playlist is a record of what you liked that year, and unliking a song
does not change that you did.

## Naming the playlists

**Playlist name** and **Description** may both contain `{year}`, which is
replaced with the year being filed — so `{year} Liked Songs` produces
`2026 Liked Songs`.

Changing these does not rename the playlists already created. The new name is
looked for among your existing playlists and one is created if nothing matches,
so a rename here — or a rename in Spotify itself — starts a fresh playlist beside
the old one.

## Scheduling

This polls the Spotify API, which is rate limited, so a failed run backs off for
an hour before it tries again. `@hourly` keeps the playlists close to current
without asking much of the API; `@daily` is plenty if you are not in a hurry.
"#;

crate::register_job!(SpotifyYearlyPlaylistWorkflow);
crate::register_workflow_type!(SpotifyYearlyPlaylistWorkflow);

impl crate::workflows::ConfigurableWorkflow for SpotifyYearlyPlaylistWorkflow {
    type ConfigType = SpotifyYearlyPlaylistConfig;

    fn type_id() -> &'static str {
        "spotify-yearly-playlist"
    }

    /// Nothing to forget. The watermark is keyed by the Spotify account this
    /// discovers at run time rather than by anything the configuration holds,
    /// and clearing it would re-file every liked song into playlists that
    /// already contain them.
    fn state(_config: &Self::ConfigType) -> Vec<StateKey> {
        Vec::new()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{
            ConnectionKind, FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor,
        };

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "Spotify Yearly Playlists".to_string(),
            description: "Files each song you like into a playlist for the year you liked it."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@hourly".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    crate::config_path!(SpotifyYearlyPlaylistConfig: connection),
                    "Spotify account",
                    FieldKind::Connection {
                        provider: SPOTIFY_PROVIDER.to_string(),
                        connection_kind: Some(ConnectionKind::OAuth2),
                    },
                )
                .with_help("Whose liked songs are filed, and whose playlists are written to.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(SpotifyYearlyPlaylistConfig: playlist_name),
                    "Playlist name",
                    FieldKind::Text {
                        placeholder: Some(default_playlist_name()),
                    },
                )
                .with_help("The name of each year's playlist. `{year}` becomes the year.")
                .with_default(default_playlist_name()),
                FieldDescriptor::new(
                    crate::config_path!(SpotifyYearlyPlaylistConfig: playlist_description),
                    "Description",
                    FieldKind::Text {
                        placeholder: Some(default_playlist_description()),
                    },
                )
                .with_help("Set on a playlist when it is created. `{year}` becomes the year.")
                .with_default(default_playlist_description()),
            ],
        }
    }
}

impl Job for SpotifyYearlyPlaylistWorkflow {
    type JobType = SpotifyYearlyPlaylistConfig;

    fn partition() -> &'static str {
        "spotify/yearly-playlist"
    }

    /// Visibility timeout / retry backoff. Calls the rate-limited Spotify API,
    /// so a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    fn propagate_parent() -> bool {
        false
    }

    #[instrument("workflow.spotify_yearly_playlist.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let connections = ConnectionStore::for_services(services);

        let Some(connection) = connections.get(job.connection).await? else {
            return Err(human_errors::user(
                format!(
                    "The selected Spotify connection ('{}') no longer exists.",
                    job.connection
                ),
                &["Link the account again, or select another connection for this workflow."],
            ));
        };

        let ConnectionSecret::OAuth2 {
            access_token,
            refresh_token,
            expires_at,
        } = connections.open(&connection)?
        else {
            return Err(human_errors::user(
                "This Spotify connection does not hold an authorization grant.",
                &["Reconnect your Spotify account."],
            ));
        };

        let stored = OAuth2RefreshToken::new(access_token, refresh_token, expires_at);

        let token = match crate::web::refresh_or_notify(SPOTIFY_PROVIDER, &stored, services).await?
        {
            Some(token) => token,
            // The refresh token has expired or been revoked: a re-authorization
            // reminder has been raised. Marking the connection and completing
            // here (rather than erroring) stops us retrying against a dead
            // account on every schedule until somebody notices.
            None => {
                connections
                    .set_status(job.connection, ConnectionStatus::NeedsReauthorization)
                    .await?;
                return Ok(());
            }
        };

        // Write the renewed grant back, so the next run starts from it rather
        // than repeating the refresh.
        if token.access_token() != stored.access_token() {
            connections
                .update_secret(
                    job.connection,
                    ConnectionSecret::OAuth2 {
                        access_token: token.access_token().to_string(),
                        refresh_token: token.refresh_token().to_string(),
                        expires_at: token.expires_at(),
                    },
                )
                .await?;
        }

        let client = SpotifyClient::new(token.clone(), services.http_client());
        let user = client.get_current_user().await?;

        let collector =
            crate::collectors::SpotifyLikedTracksCollector::new(user.id.clone(), token.clone());

        let new_tracks = collector.list(services).await?;

        let year_groups = new_tracks.iter().fold(
            std::collections::HashMap::<
                i32,
                Vec<&crate::publishers::spotify::SpotifySavedTrack>,
            >::new(),
            |mut acc, track| {
                acc.entry(track.added_at.year()).or_default().push(track);
                acc
            },
        );

        for (year, tracks) in year_groups {
            let description = render(&job.playlist_description, year);

            crate::publishers::SpotifyAddToPlaylist::dispatch(
                crate::publishers::SpotifyAddToPlaylistPayload {
                    account_id: user.id.clone(),
                    name: render(&job.playlist_name, year),
                    description: (!description.is_empty()).then_some(description),
                    access_token: token.clone(),
                    track_uris: tracks.iter().map(|t| t.track.uri.clone()).collect(),
                },
                None,
                services,
            )
            .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults reproduce the names this workflow used before it was
    /// configurable, so an upgraded installation keeps filing into the playlists
    /// it already created rather than starting a second set beside them.
    #[test]
    fn the_default_templates_produce_the_playlist_names_already_out_there() {
        assert_eq!(render(&default_playlist_name(), 2024), "2024 Liked Songs");
        assert_eq!(
            render(&default_playlist_description(), 2024),
            "A yearly playlist of all my liked songs from 2024."
        );
    }

    /// What every stored configuration looked like before the templates existed.
    #[test]
    fn a_configuration_naming_only_a_connection_takes_the_defaults() {
        let config: SpotifyYearlyPlaylistConfig = serde_json::from_value(serde_json::json!({
            "connection": ConnectionId::from_entropy(0).to_string(),
        }))
        .unwrap();

        assert_eq!(config.playlist_name, default_playlist_name());
        assert_eq!(config.playlist_description, default_playlist_description());
    }
}
