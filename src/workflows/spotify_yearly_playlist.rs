use chrono::{Datelike, TimeDelta};

use crate::{prelude::*, publishers::SpotifyClient};

#[derive(Clone)]
pub struct SpotifyYearlyPlaylistWorkflow;

impl Job for SpotifyYearlyPlaylistWorkflow {
    type JobType = OAuth2RefreshToken;

    fn partition() -> &'static str {
        "workflow/spotify-yearly-playlist"
    }

    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let token = SpotifyClient::renew_access_token(job, &services).await?;

        let client = SpotifyClient::new(token.clone());
        let user = client.get_current_user().await?;

        let collector =
            crate::collectors::SpotifyLikedTracksCollector::new(user.id.clone(), token.clone());

        let new_tracks = collector.list(&services).await?;

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
                    &services,
                )
                .await?;
            }
        }

        Self::dispatch_delayed(token, Some(user.id.into()), TimeDelta::hours(1), &services).await?;

        Ok(())
    }
}
