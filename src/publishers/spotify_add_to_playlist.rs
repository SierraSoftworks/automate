use crate::prelude::*;

use super::SpotifyClient;

#[derive(Serialize, Deserialize)]
pub struct SpotifyAddToPlaylistPayload {
    pub account_id: String,
    pub name: String,
    pub description: Option<String>,
    pub track_uris: Vec<String>,
    pub access_token: OAuth2RefreshToken,
}

pub struct SpotifyAddToPlaylist;

impl Job for SpotifyAddToPlaylist {
    type JobType = SpotifyAddToPlaylistPayload;

    fn partition() -> &'static str {
        "spotify/add-to-playlist"
    }

    #[instrument(
        "publishers.spotify_add_to_playlist.handle",
        skip(self, job, services),
        err(Display)
    )]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let client = SpotifyClient::new(job.access_token.clone());

        let playlist_id = self.get_playlist_id(job, &services).await?;

        client
            .add_tracks_to_playlist(&playlist_id, job.track_uris.clone())
            .await?;

        Ok(())
    }
}

impl SpotifyAddToPlaylist {
    async fn get_playlist_id(
        &self,
        job: &SpotifyAddToPlaylistPayload,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<String, human_errors::Error> {
        if let Some(playlist_id) = services
            .kv()
            .get(
                "spotify/playlist",
                format!("{}/{}", job.account_id, job.name),
            )
            .await?
        {
            Ok(playlist_id)
        } else {
            let client = SpotifyClient::new(job.access_token.clone());

            if let Some(playlist) = client
                .get_playlists()
                .await?
                .into_iter()
                .find(|p| p.name == job.name)
                .map(|p| p.id)
            {
                services
                    .kv()
                    .set(
                        "spotify/playlist",
                        format!("{}/{}", job.account_id, job.name),
                        playlist.clone(),
                    )
                    .await?;
                Ok(playlist)
            } else {
                let playlist = client
                    .create_playlist(&job.name, false, false, job.description.clone())
                    .await?;
                services
                    .kv()
                    .set(
                        "spotify/playlist",
                        format!("{}/{}", job.account_id, job.name),
                        playlist.id.clone(),
                    )
                    .await?;
                Ok(playlist.id)
            }
        }
    }
}
