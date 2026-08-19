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
    /// Up to [`COVER_URL_CANDIDATES`] distinct track covers, in track order,
    /// derived the last time the playlist was browsed. Spotify's rootlist
    /// returns no artwork at all for most playlists, so this is what the
    /// surfaces that only ever see the library payload — the sidebar, the home
    /// grid — mosaic instead of dropping to a monogram tile. Empty until a
    /// browse fills it in; a real `cover_url` still outranks it.
    pub cover_urls: Vec<String>,
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
            // The rootlist reference carries neither the collaborative flag,
            // nor a revision, nor any track — so the cover candidates have to
            // be carried over from a browse (see `carry_browse_fields`).
            collaborative: false,
            tracks_total: reference.track_count.unwrap_or(0),
            cover_urls: Vec::new(),
            snapshot_id: String::new(),
        }
    }
}

/// How many distinct track covers a playlist keeps as mosaic candidates: the
/// 2x2 grid the frontend paints for a playlist with no cover of its own.
pub const COVER_URL_CANDIDATES: usize = 4;

/// The first [`COVER_URL_CANDIDATES`] distinct non-empty track covers, in
/// track order.
///
/// Distinctness is the point: a playlist that is one album top to bottom would
/// otherwise mosaic four copies of the same square, which reads as a rendering
/// bug rather than as a cover. Scanning the kept urls beats a set — there are
/// never more than four of them, and the loop stops at the fourth.
pub fn cover_urls_from_tracks(tracks: &[Track]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::with_capacity(COVER_URL_CANDIDATES);
    for track in tracks {
        if track.cover_url.is_empty() || urls.iter().any(|url| url == &track.cover_url) {
            continue;
        }
        urls.push(track.cover_url.clone());
        if urls.len() == COVER_URL_CANDIDATES {
            break;
        }
    }
    urls
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
        let tracks: Vec<Track> = browse.tracks.into_iter().map(Track::from).collect();
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
                // Derived here rather than at the call sites so the candidates
                // and the revision they came from are always written as a pair:
                // a revision bump can replace exactly the tracks they were
                // taken from, so candidates outliving their revision are stale.
                cover_urls: cover_urls_from_tracks(&tracks),
                collaborative: false,
                tracks_total: tracks.len() as u32,
                snapshot_id: revision,
            },
            tracks,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn track(cover_url: &str) -> Track {
        Track {
            cover_url: cover_url.to_owned(),
            ..Track::default()
        }
    }

    fn browse(revision: &str, covers: &[&str]) -> PlaylistBrowse {
        PlaylistBrowse {
            id: "p1".to_owned(),
            uri: "spotify:playlist:p1".to_owned(),
            name: "Mixtape".to_owned(),
            revision: Some(revision.to_owned()),
            owner_id: "me".to_owned(),
            owner_name: String::new(),
            cover_url: None,
            tracks: covers.iter().map(|url| track(url).into()).collect(),
        }
    }

    #[test]
    fn cover_candidates_take_the_first_four_distinct_covers() {
        let covers = ["a", "a", "b", "c", "b", "d", "e"];
        let tracks: Vec<Track> = covers.iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn cover_candidates_skip_tracks_without_a_cover() {
        let tracks: Vec<Track> = ["", "a", "", "b"].iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b"]);
        // Fewer than four distinct covers is normal (short playlists, one
        // album); the frontend renders whatever it is given.
        assert!(cover_urls_from_tracks(&[]).is_empty());
        assert!(cover_urls_from_tracks(&[track("")]).is_empty());
    }

    #[test]
    fn browsing_derives_the_candidates_alongside_the_revision() {
        let detail = PlaylistDetail::from(browse("rev-a", &["a", "b"]));
        assert_eq!(detail.playlist.cover_urls, vec!["a", "b"]);
        assert_eq!(detail.playlist.snapshot_id, "rev-a");
        assert_eq!(detail.playlist.tracks_total, 2);

        // A revision bump can replace exactly the tracks the old candidates
        // came from, so a browse never reuses them.
        let next = PlaylistDetail::from(browse("rev-b", &["c", "d"]));
        assert_eq!(next.playlist.cover_urls, vec!["c", "d"]);
        assert_eq!(next.playlist.snapshot_id, "rev-b");
    }

    #[test]
    fn a_missing_cover_urls_field_deserializes_to_no_candidates() {
        // Caches written before the field existed must still load.
        let playlist: Playlist =
            serde_json::from_str(r#"{"id":"p1","name":"Mixtape","cover_url":""}"#).unwrap();
        assert_eq!(playlist.id, "p1");
        assert!(playlist.cover_urls.is_empty());
    }
}
