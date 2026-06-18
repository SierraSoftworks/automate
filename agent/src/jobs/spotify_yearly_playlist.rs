use chrono::{Datelike, TimeDelta};

use crate::{prelude::*, publishers::SpotifyClient};

#[derive(Clone)]
pub struct SpotifyYearlyPlaylistWorkflow;

crate::register_job!(SpotifyYearlyPlaylistWorkflow);

impl Job for SpotifyYearlyPlaylistWorkflow {
    type JobType = OAuth2RefreshToken;

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
        let token = match crate::web::refresh_or_notify("spotify", job, services).await? {
            Some(token) => token,
            // The refresh token has expired or been revoked: a re-authorization
            // reminder has been raised. Completing here (rather than erroring)
            // removes this queued message, and deliberately skipping the delayed
            // re-enqueue below stops us from using the dead account.
            None => return Ok(()),
        };

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

        Self::dispatch_delayed(token, Some(user.id.into()), TimeDelta::hours(1), services).await?;

        Ok(())
    }
}
