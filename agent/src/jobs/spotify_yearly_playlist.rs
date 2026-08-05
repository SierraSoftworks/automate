use automate_api::{ConnectionId, ConnectionStatus};
use chrono::{Datelike, TimeDelta};

use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::{prelude::*, publishers::SpotifyClient};

/// Identifies which linked Spotify account a run is for.
///
/// The credential itself is deliberately not carried here. It lives encrypted in
/// the connection, so a queue message names the account rather than holding the
/// means to use it - which also means a refreshed token is written back to one
/// place instead of riding along in every subsequent message.
#[derive(Clone, Serialize, Deserialize)]
pub struct SpotifyYearlyPlaylistTask {
    pub connection: ConnectionId,
}

impl std::fmt::Display for SpotifyYearlyPlaylistTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.connection)
    }
}

#[derive(Clone)]
pub struct SpotifyYearlyPlaylistWorkflow;

crate::register_job!(SpotifyYearlyPlaylistWorkflow);

impl Job for SpotifyYearlyPlaylistWorkflow {
    type JobType = SpotifyYearlyPlaylistTask;

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

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let connections = ConnectionStore::for_services(services);

        let Some(connection) = connections.get(job.connection).await? else {
            // The account was unlinked while this run was queued. Completing
            // here stops the loop, which is what unlinking is meant to do.
            info!(
                connection.id = %job.connection,
                "Skipping a run for a connection that no longer exists."
            );
            return Ok(());
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

        let token = match crate::web::refresh_or_notify("spotify", &stored, services).await? {
            Some(token) => token,
            // The refresh token has expired or been revoked: a re-authorization
            // reminder has been raised. Marking the connection and completing
            // here (rather than erroring) removes this queued message, and
            // deliberately skipping the delayed re-enqueue stops us from using
            // the dead account.
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

        if !new_tracks.is_empty() {
            let year_groups =
                new_tracks.iter().fold(
                    std::collections::HashMap::<
                        i32,
                        Vec<&crate::publishers::spotify::SpotifySavedTrack>,
                    >::new(),
                    |mut acc, track| {
                        let year = track.added_at.year();
                        acc.entry(year).or_default().push(track);
                        acc
                    },
                );

            for (year, tracks) in year_groups {
                crate::publishers::SpotifyAddToPlaylist::dispatch(
                    crate::publishers::SpotifyAddToPlaylistPayload {
                        account_id: user.id.clone(),
                        name: format!("{} Liked Songs", year),
                        description: Some(format!(
                            "A yearly playlist of all my liked songs from {}.",
                            year
                        )),
                        access_token: token.clone(),
                        track_uris: tracks.iter().map(|t| t.track.uri.clone()).collect(),
                    },
                    None,
                    services,
                )
                .await?;
            }
        }

        // Keyed by the connection, so a linked account has exactly one run
        // pending at a time no matter how often it is re-authorised.
        Self::dispatch_delayed(
            job.clone(),
            Some(job.connection.to_string().into()),
            TimeDelta::hours(1),
            services,
        )
        .await?;

        Ok(())
    }
}
