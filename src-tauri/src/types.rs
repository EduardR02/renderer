//! Shared serde types for the Tauri <-> frontend contract.
//!
//! Field names are the exact JSON keys the frontend consumes; they mirror
//! the engine's protocol payloads (see `spotify_playback_engine::protocol`)
//! so the engine's line-JSON state and browse messages deserialize straight
//! into these types.

use serde::{Deserialize, Serialize};

use spotify_playback_engine::protocol::{
    AlbumBrowse, AlbumRef, ArtistBrowse, ArtistRef, PlaylistBrowse, PlaylistRef, SearchBrowse,
    TrackRef,
};

/// One playable track. Field-for-field identical to the engine's `TrackRef`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    pub artist_id: String,
    pub album_id: String,
    pub album_name: String,
    pub cover_url: String,
    pub duration_ms: u32,
}

impl From<TrackRef> for Track {
    fn from(track: TrackRef) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
        }
    }
}

impl From<&TrackRef> for Track {
    fn from(track: &TrackRef) -> Self {
        Self {
            id: track.id.clone(),
            uri: track.uri.clone(),
            name: track.name.clone(),
            artist_names: track.artist_names.clone(),
            artist_id: track.artist_id.clone(),
            album_id: track.album_id.clone(),
            album_name: track.album_name.clone(),
            cover_url: track.cover_url.clone(),
            duration_ms: track.duration_ms,
        }
    }
}

impl From<Track> for TrackRef {
    fn from(track: Track) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
        }
    }
}

/// A playlist in the user's library (rootlist).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// Display name of the owning user (falls back to the username).
    pub owner: String,
    pub owner_id: String,
    pub cover_url: String,
    pub collaborative: bool,
    pub tracks_total: u32,
    /// Playlist4 revision hex (the Web API "snapshot id"); empty when the
    /// library listing did not carry a revision.
    pub snapshot_id: String,
}

impl From<&PlaylistRef> for Playlist {
    fn from(reference: &PlaylistRef) -> Self {
        Self {
            id: reference.id.clone(),
            uri: reference.uri.clone(),
            name: reference.name.clone(),
            owner: if reference.owner_name.is_empty() {
                reference.owner_id.clone()
            } else {
                reference.owner_name.clone()
            },
            owner_id: reference.owner_id.clone(),
            cover_url: reference.cover_url.clone().unwrap_or_default(),
            // The rootlist reference does not carry the collaborative flag.
            collaborative: false,
            tracks_total: reference.track_count.unwrap_or(0),
            snapshot_id: String::new(),
        }
    }
}

/// A playlist opened for browsing: playlist metadata plus its tracks.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistDetail {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

impl From<PlaylistBrowse> for PlaylistDetail {
    fn from(browse: PlaylistBrowse) -> Self {
        let revision = browse.revision.unwrap_or_default();
        Self {
            playlist: Playlist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                owner: if browse.owner_name.is_empty() {
                    browse.owner_id.clone()
                } else {
                    browse.owner_name
                },
                owner_id: browse.owner_id,
                cover_url: browse.cover_url.unwrap_or_default(),
                collaborative: false,
                tracks_total: browse.tracks.len() as u32,
                snapshot_id: revision,
            },
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    pub cover_url: String,
}

impl From<AlbumRef> for Album {
    fn from(reference: AlbumRef) -> Self {
        Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            artist_names: reference.artist_names,
            cover_url: reference.cover_url.unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: Album,
    pub tracks: Vec<Track>,
}

impl From<AlbumBrowse> for AlbumDetail {
    fn from(browse: AlbumBrowse) -> Self {
        Self {
            album: Album {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                artist_names: browse.artist_names,
                cover_url: browse.cover_url.unwrap_or_default(),
            },
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Artist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub cover_url: String,
}

impl From<ArtistRef> for Artist {
    fn from(reference: ArtistRef) -> Self {
        Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            cover_url: reference.portrait_url.unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistDetail {
    #[serde(flatten)]
    pub artist: Artist,
    pub top_tracks: Vec<Track>,
    pub albums: Vec<Album>,
}

impl From<ArtistBrowse> for ArtistDetail {
    fn from(browse: ArtistBrowse) -> Self {
        Self {
            artist: Artist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                cover_url: browse.portrait_url.unwrap_or_default(),
            },
            top_tracks: browse.top_tracks.into_iter().map(Track::from).collect(),
            albums: browse.albums.into_iter().map(Album::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchResult {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
}

impl From<SearchBrowse> for SearchResult {
    fn from(browse: SearchBrowse) -> Self {
        Self {
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
            albums: browse.albums.into_iter().map(Album::from).collect(),
            artists: browse.artists.into_iter().map(Artist::from).collect(),
        }
    }
}

/// Deserializes a nullable/missing string field into an owned `String`
/// (the engine serializes some optional state fields as `null`).
fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Mirror of the engine's `state` line, projected locally between the
/// engine's 2-second heartbeats.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaybackState {
    pub ready: bool,
    /// One of `authenticating`, `needs_login`, `ready`, `error`.
    pub auth_state: String,
    #[serde(deserialize_with = "string_or_default")]
    pub auth_url: String,
    pub playing: bool,
    #[serde(deserialize_with = "string_or_default")]
    pub username: String,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    /// One of `off`, `context`, `track`.
    pub repeat: String,
    pub current_index: Option<usize>,
    #[serde(deserialize_with = "string_or_default")]
    pub current_uri: String,
    pub queue: Vec<Track>,
    #[serde(deserialize_with = "string_or_default")]
    pub error: String,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            ready: false,
            auth_state: "needs_login".to_owned(),
            auth_url: String::new(),
            playing: false,
            username: String::new(),
            position_ms: 0,
            duration_ms: 0,
            // The engine starts at 50% volume; mirror it so the first paint
            // matches the first heartbeat.
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            current_index: None,
            current_uri: String::new(),
            queue: Vec::new(),
            error: String::new(),
        }
    }
}

/// Full snapshot served to the frontend for the initial render.
#[derive(Clone, Debug, Serialize)]
pub struct AppState {
    pub playback: PlaybackState,
    pub playlists: Vec<Playlist>,
    pub me_id: String,
}
