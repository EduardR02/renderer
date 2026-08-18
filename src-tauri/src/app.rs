//! Application state and on-disk caches.
//!
//! The disk formats are frozen: `playlist_list.json` and
//! `playlist_tracks_cache.json` under `%LOCALAPPDATA%\SpotifyRenderer` reuse
//! the old app's layout so existing user data migrates unchanged. Covers are
//! raw image bytes keyed by `sha1(url)`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::types::{PlaybackState, Playlist, PlaylistDetail, Track};

/// Number of playlists requested from the engine's rootlist browse.
pub const LIBRARY_LENGTH: usize = 100;

/// Most-recent-first cap for the playlist tracks cache.
const TRACKS_CACHE_MAX: usize = 25;

/// Managed application state. Only the contract fields are serialized; the
/// cache bookkeeping and the data dir are internal.
#[derive(Debug, Serialize)]
pub struct AppState {
    pub playback: PlaybackState,
    pub playlists: Vec<Playlist>,
    pub me_id: String,
    #[serde(skip)]
    pub playlists_fetched_at: Option<i64>,
    /// Most-recently-opened first; limited to [`TRACKS_CACHE_MAX`] entries.
    #[serde(skip)]
    pub tracks_cache: Vec<PlaylistTracksEntry>,
    #[serde(skip)]
    pub data_dir: PathBuf,
    /// True while a background library refresh (with retries) is running.
    #[serde(skip)]
    pub library_fetching: bool,
    /// Set when a trigger arrives mid-chain; the running chain re-runs once
    /// more instead of the trigger being dropped.
    #[serde(skip)]
    pub library_refresh_queued: bool,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            playback: PlaybackState::default(),
            playlists: Vec::new(),
            me_id: String::new(),
            playlists_fetched_at: None,
            tracks_cache: Vec::new(),
            data_dir,
            library_fetching: false,
            library_refresh_queued: false,
        }
    }
}

/// One cached playlist tracks payload, matching the on-disk entry shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistTracksEntry {
    pub id: String,
    pub fetched_at: Option<i64>,
    /// Playlist4 revision hex; the Web API snapshot id.
    pub revision: String,
    pub tracks: Vec<Track>,
}

// ---------------------------------------------------------------------------
// Data directory
// ---------------------------------------------------------------------------

/// `%LOCALAPPDATA%\SpotifyRenderer` (with a sane fallback when the variable
/// is unset). All caches and the engine state dir live under here.
pub fn data_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("SpotifyRenderer");
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join("SpotifyRenderer");
    }
    std::env::temp_dir().join("SpotifyRenderer")
}

/// Diagnostic logs: `%LOCALAPPDATA%\SpotifyRenderer\logs` — the app's own
/// `spotify_renderer.log` and the engine's `playback_engine.log`.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Engine `--state-dir`: `%LOCALAPPDATA%\SpotifyRenderer\engine`, overridable
/// via `SPOTIFY_STATE_DIR`.
pub fn engine_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPOTIFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    data_dir().join("engine")
}

// ---------------------------------------------------------------------------
// playlist_list.json
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistListCache {
    pub version: u32,
    pub fetched_at: Option<i64>,
    pub me_id: String,
    pub playlists: Vec<Playlist>,
}

pub fn load_playlist_list(dir: &Path) -> Option<PlaylistListCache> {
    let bytes = std::fs::read(dir.join("playlist_list.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_playlist_list(dir: &Path, cache: &PlaylistListCache) {
    write_json_atomic(dir.join("playlist_list.json"), cache);
}

// ---------------------------------------------------------------------------
// playlist_tracks_cache.json
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PlaylistTracksCache {
    pub version: u32,
    pub saved_at: Option<i64>,
    /// Most-recently-opened first.
    pub playlists: Vec<PlaylistTracksEntry>,
}

pub fn load_tracks_cache(dir: &Path) -> Vec<PlaylistTracksEntry> {
    let bytes = match std::fs::read(dir.join("playlist_tracks_cache.json")) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<PlaylistTracksCache>(&bytes) {
        Ok(cache) => cache.playlists,
        Err(_) => Vec::new(),
    }
}

pub fn save_tracks_cache(dir: &Path, playlists: &[PlaylistTracksEntry]) {
    let cache = PlaylistTracksCache {
        version: 1,
        saved_at: Some(now_secs()),
        playlists: playlists.to_vec(),
    };
    write_json_atomic(dir.join("playlist_tracks_cache.json"), &cache);
}

/// Inserts (or refreshes) one playlist's tracks at the front of the cache,
/// dropping the oldest entries beyond the cap.
pub fn upsert_tracks_cache(entries: &mut Vec<PlaylistTracksEntry>, entry: PlaylistTracksEntry) {
    entries.retain(|existing| existing.id != entry.id);
    entries.insert(0, entry);
    entries.truncate(TRACKS_CACHE_MAX);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Combines cached tracks with the library playlist metadata into the full
/// detail payload the UI renders.
pub fn playlist_detail_from_cache(state: &AppState, entry: &PlaylistTracksEntry) -> PlaylistDetail {
    let meta = state.playlists.iter().find(|playlist| playlist.id == entry.id);
    let mut playlist = meta.cloned().unwrap_or_else(|| Playlist {
        id: entry.id.clone(),
        uri: format!("spotify:playlist:{}", entry.id),
        ..Playlist::default()
    });
    playlist.tracks_total = entry.tracks.len() as u32;
    playlist.snapshot_id = entry.revision.clone();
    PlaylistDetail {
        playlist,
        tracks: entry.tracks.clone(),
    }
}

/// Upserts a playlist into the in-memory library list.
pub fn upsert_playlist(playlists: &mut Vec<Playlist>, playlist: Playlist) {
    if let Some(existing) = playlists.iter_mut().find(|entry| entry.id == playlist.id) {
        *existing = playlist;
    } else {
        playlists.push(playlist);
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Writes via a temp file + rename so a crash mid-write cannot corrupt the
/// cache.
fn write_json_atomic<T: Serialize>(path: PathBuf, value: &T) {
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("SpotifyRenderer: could not serialize cache {path:?}: {error}");
            return;
        }
    };
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, &bytes).is_err() {
        return;
    }
    let _ = std::fs::rename(&temp, &path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            ..Track::default()
        }
    }

    #[test]
    fn tracks_cache_keeps_most_recent_first_and_caps_at_25() {
        let mut cache = Vec::new();
        for id in 0..30 {
            upsert_tracks_cache(
                &mut cache,
                PlaylistTracksEntry {
                    id: format!("p{id}"),
                    fetched_at: Some(id),
                    revision: String::new(),
                    tracks: vec![track("t")],
                },
            );
        }
        assert_eq!(cache.len(), 25);
        assert_eq!(cache[0].id, "p29");
        assert_eq!(cache[24].id, "p5");
    }

    #[test]
    fn upsert_refreshes_an_existing_entry_in_place() {
        let mut cache = vec![PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "old".into(),
            tracks: vec![track("a")],
        }];
        upsert_tracks_cache(
            &mut cache,
            PlaylistTracksEntry {
                id: "p1".into(),
                fetched_at: Some(2),
                revision: "new".into(),
                tracks: vec![track("b")],
            },
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].revision, "new");
        assert_eq!(cache[0].tracks[0].id, "b");
    }

    #[test]
    fn detail_from_cache_falls_back_to_blank_metadata() {
        let state = AppState::new(PathBuf::from("unused"));
        let entry = PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "rev".into(),
            tracks: vec![track("t")],
        };
        let detail = playlist_detail_from_cache(&state, &entry);
        assert_eq!(detail.playlist.id, "p1");
        assert_eq!(detail.playlist.uri, "spotify:playlist:p1");
        assert_eq!(detail.playlist.snapshot_id, "rev");
        assert_eq!(detail.playlist.tracks_total, 1);
        assert_eq!(detail.tracks[0].id, "t");
    }

    #[test]
    fn tracks_cache_round_trips_through_the_disk_format() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-test-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let entries = vec![PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(42),
            revision: "rev".into(),
            tracks: vec![track("t")],
        }];
        save_tracks_cache(&dir, &entries);
        let loaded = load_tracks_cache(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "p1");
        assert_eq!(loaded[0].revision, "rev");
        assert_eq!(loaded[0].tracks[0].id, "t");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
