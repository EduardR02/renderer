//! Application state and on-disk caches.
//!
//! The disk formats are frozen: `playlist_list.json` and
//! `playlist_tracks_cache.json` under `%LOCALAPPDATA%\SpotifyRenderer` reuse
//! the old app's layout so existing user data migrates unchanged. Covers are
//! raw image bytes keyed by `sha1(url)`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::types::{
    align_artist_ids, cover_urls_from_tracks, forget_cached_audio, CacheStats, CacheUsage,
    PlaybackState, Playlist,
    PlaylistDetail, Track,
};

/// Number of playlists requested from the engine's rootlist browse.
pub const LIBRARY_LENGTH: usize = 100;

/// Most-recent-first cap for the playlist tracks cache.
const TRACKS_CACHE_MAX: usize = 25;

/// Default audio cache cap. Kept in MB because that is the user-facing unit
/// and the engine command line accepts the same value without rounding.
pub const DEFAULT_AUDIO_CACHE_LIMIT_MB: u64 = 1024;

/// Persistent preferences that affect process startup rather than live
/// playback state. Zero means an unlimited audio cache.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub audio_cache_limit_mb: u64,
    pub launch_at_login: bool,
    pub start_minimized: bool,
    pub animated_canvas: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            audio_cache_limit_mb: DEFAULT_AUDIO_CACHE_LIMIT_MB,
            launch_at_login: false,
            start_minimized: false,
            animated_canvas: false,
        }
    }
}

pub const PLAYBACK_STATE_VERSION: u32 = 2;

/// App-owned durable playback state. Deliberately excludes `playing`: every
/// normal process start restores paused, while crash-only resume intent stays
/// in memory in `EngineClient`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PlaybackSnapshot {
    pub version: u32,
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
    pub playback_speed: f32,
}

impl PlaybackSnapshot {
    pub fn from_playback(state: &PlaybackState) -> Self {
        Self {
            version: PLAYBACK_STATE_VERSION,
            queue: state.queue.clone(),
            current_index: state.current_index,
            position_ms: state.position_ms,
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: state.repeat.clone(),
            playback_speed: state.playback_speed,
        }
    }

    fn is_valid(&self) -> bool {
        self.version == PLAYBACK_STATE_VERSION
            && self.volume <= 100
            && matches!(self.repeat.as_str(), "off" | "context" | "track")
            && self.playback_speed.is_finite()
            && (0.5..=2.0).contains(&self.playback_speed)
            && self.current_index.is_none_or(|index| index < self.queue.len())
            && (!self.queue.is_empty() || self.current_index.is_none())
            && self.queue.iter().all(|track| {
                track.uri.starts_with("spotify:track:")
                    && (track.duration_ms > 0 || track.unavailable)
            })
            && match self.current_index {
                Some(index) => self.position_ms <= self.queue[index].duration_ms,
                None => self.position_ms == 0,
            }
    }
}

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
    /// Last computed cache sizes and the unix second they were computed at,
    /// so reopening Settings does not re-walk thousands of files. See
    /// [`CACHE_STATS_TTL_SECS`].
    #[serde(skip)]
    pub cache_stats: Option<(i64, CacheStats)>,
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
            cache_stats: None,
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

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn load_app_settings() -> AppSettings {
    let mut settings: AppSettings = std::fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if !matches!(settings.audio_cache_limit_mb, 0 | 1024 | 2048 | 4096 | 8192) {
        settings.audio_cache_limit_mb = DEFAULT_AUDIO_CACHE_LIMIT_MB;
    }
    settings
}

pub fn save_app_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create settings directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("could not serialize settings: {error}"))?;
    std::fs::write(path, bytes).map_err(|error| format!("could not save settings: {error}"))
}

fn playback_state_path(dir: &Path) -> PathBuf {
    dir.join("playback_state.json")
}

fn load_playback_snapshot_from(dir: &Path) -> Option<PlaybackSnapshot> {
    let snapshot: PlaybackSnapshot =
        serde_json::from_slice(&std::fs::read(playback_state_path(dir)).ok()?).ok()?;
    snapshot.is_valid().then_some(snapshot)
}

pub fn load_playback_snapshot() -> Option<PlaybackSnapshot> {
    load_playback_snapshot_from(&data_dir())
}

fn save_playback_snapshot_to(dir: &Path, snapshot: &PlaybackSnapshot) -> Result<(), String> {
    if !snapshot.is_valid() {
        return Err("refusing to persist an invalid playback snapshot".to_owned());
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create playback state directory: {error}"))?;
    write_json_atomic_result(playback_state_path(dir), snapshot)
}

pub fn save_playback_snapshot(snapshot: &PlaybackSnapshot) -> Result<(), String> {
    save_playback_snapshot_to(&data_dir(), snapshot)
}

pub fn clear_playback_snapshot() -> Result<(), String> {
    match std::fs::remove_file(playback_state_path(&data_dir())) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not clear playback state: {error}")),
    }
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

#[derive(Serialize)]
struct PlaylistTracksCacheRef<'a> {
    version: u32,
    saved_at: Option<i64>,
    playlists: &'a [PlaylistTracksEntry],
}

pub fn load_tracks_cache(dir: &Path) -> Vec<PlaylistTracksEntry> {
    let bytes = match std::fs::read(dir.join("playlist_tracks_cache.json")) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<PlaylistTracksCache>(&bytes) {
        Ok(mut cache) => {
            // Entries written before `artist_ids` existed carry names but no
            // ids; align them on the way in so the two lists are always the
            // same length, whatever wrote the file.
            for entry in &mut cache.playlists {
                for track in &mut entry.tracks {
                    align_artist_ids(track);
                    // The download mark is about the AUDIO cache, whose
                    // contents this file knows nothing about — it may have been
                    // pruned or cleared since. Only a live browse can answer it.
                    forget_cached_audio(track);
                }
            }
            cache.playlists
        }
        Err(_) => Vec::new(),
    }
}

pub fn save_tracks_cache(dir: &Path, playlists: &[PlaylistTracksEntry]) {
    let cache = PlaylistTracksCacheRef {
        version: 1,
        saved_at: Some(now_secs()),
        playlists,
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
pub fn playlist_detail_from_cache(
    state: &AppState,
    entry: PlaylistTracksEntry,
) -> PlaylistDetail {
    let meta = state.playlists.iter().find(|playlist| playlist.id == entry.id);
    let mut playlist = meta.cloned().unwrap_or_else(|| Playlist {
        id: entry.id.clone(),
        uri: format!("spotify:playlist:{}", entry.id),
        ..Playlist::default()
    });
    playlist.tracks_total = entry.tracks.len() as u32;
    playlist.snapshot_id = entry.revision;
    // These tracks are what the detail page renders, so its candidates come
    // from them rather than from whatever revision the library entry was last
    // browsed at — candidates and revision always travel together.
    playlist.cover_urls = cover_urls_from_tracks(&entry.tracks);
    PlaylistDetail {
        playlist,
        tracks: entry.tracks,
    }
}

/// Upserts a playlist into the in-memory library list.
///
/// A browsed playlist knows nothing about when it was last played or used in
/// the library, so incoming `None`s never overwrite stored timestamps: the
/// background refresh that follows every browse would otherwise erase a stamp
/// written moments earlier.
pub fn upsert_playlist(playlists: &mut Vec<Playlist>, mut playlist: Playlist) {
    if let Some(existing) = playlists.iter_mut().find(|entry| entry.id == playlist.id) {
        playlist.last_played = playlist.last_played.or(existing.last_played);
        playlist.last_activity = playlist.last_activity.or(existing.last_activity);
        *existing = playlist;
    } else {
        playlists.push(playlist);
    }
}

/// Copies the fields the rootlist cannot produce — the revision, the derived
/// cover candidates, and the local activity timestamps — from the previous
/// library snapshot onto a freshly fetched one.
///
/// The rootlist carries none of them, so a plain replacement would blank all
/// four local fields on every library refresh. For the candidates that is not
/// cosmetic: they are derived from a browse of the playlist itself, so a wipe
/// would drop the sidebar and home-grid mosaics back to monogram tiles until
/// each playlist happens to be browsed again. The timestamps likewise must
/// survive refresh so the sidebar does not jump back to rootlist order and
/// Home does not lose its listening history.
pub fn carry_local_fields(previous: &[Playlist], fresh: &mut [Playlist]) {
    for playlist in fresh.iter_mut() {
        if let Some(old) = previous.iter().find(|entry| entry.id == playlist.id) {
            playlist.cover_urls = old.cover_urls.clone();
            playlist.snapshot_id = old.snapshot_id.clone();
            playlist.last_played = old.last_played;
            playlist.last_activity = old.last_activity;
        }
    }
}

/// Sorts the library most-recently-used first.
///
/// Playlists with no local activity keep their rootlist order behind the
/// active ones: the sort is stable and their key is the minimum. This is the
/// order consumed by both the sidebar and Home's remaining-library grid.
pub fn order_by_last_activity(playlists: &mut [Playlist]) {
    playlists.sort_by_key(|playlist| {
        std::cmp::Reverse(playlist.last_activity.unwrap_or(i64::MIN))
    });
}

/// Stamps `id` as used in the library at `at` and re-sorts the list.
///
/// This is used for a successful add-to-playlist. It deliberately does not
/// touch `last_played`, so a library edit cannot create a fake listening-
/// history entry on Home.
pub fn touch_playlist_activity(playlists: &mut Vec<Playlist>, id: &str, at: i64) -> bool {
    let Some(playlist) = playlists.iter_mut().find(|entry| entry.id == id) else {
        return false;
    };
    playlist.last_activity = Some(at);
    order_by_last_activity(playlists);
    true
}

/// Stamps `id` as played from at `at`, updating both local recency fields.
///
/// Deliberately not driven by opening a playlist: browsing one is not using
/// it. Which playlist a play came from is also not derivable from a track URI —
/// the same track sits in many playlists — which is why the caller passes the
/// id explicitly.
pub fn touch_playlist_played(playlists: &mut Vec<Playlist>, id: &str, at: i64) -> bool {
    let Some(playlist) = playlists.iter_mut().find(|entry| entry.id == id) else {
        return false;
    };
    playlist.last_played = Some(at);
    playlist.last_activity = Some(at);
    order_by_last_activity(playlists);
    true
}

// ---------------------------------------------------------------------------
// Cache sizes
// ---------------------------------------------------------------------------

/// How long a computed [`CacheStats`] stays fresh. Reopening Settings inside
/// this window reuses the numbers instead of re-walking the caches; a minute
/// is far shorter than it takes a user to notice a size change, and the walk
/// only ever runs off the UI thread anyway.
pub const CACHE_STATS_TTL_SECS: i64 = 60;

/// Bookkeeping files that sit in a cache directory without being cached
/// content. `cache-version` is the audio cache's layout marker (written by
/// the engine's `version_audio_cache`); counting it would report one song too
/// many for an empty cache.
const CACHE_NON_CONTENT: &[&str] = &["cache-version"];

/// Walks one cache directory and totals the files it holds.
///
/// Recursive because librespot shards its audio cache by the first byte of the
/// file id (`audio/ab/cdef…`), while the cover cache is flat. Unreadable
/// entries are skipped rather than failing the whole figure — a stat is not
/// worth an error dialog — and directory symlinks are never followed, so a
/// junction inside the cache cannot send this into a loop.
pub fn directory_usage(root: &Path) -> CacheUsage {
    let mut usage = CacheUsage::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            let name = entry.file_name();
            if CACHE_NON_CONTENT
                .iter()
                .any(|skipped| name.eq_ignore_ascii_case(skipped))
            {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                usage.files += 1;
                usage.bytes += metadata.len();
            }
        }
    }
    usage
}

/// Sizes of both on-disk caches. Blocking filesystem work: call it from a
/// blocking task, never on the UI thread (see `commands::get_cache_stats`).
pub fn compute_cache_stats() -> CacheStats {
    CacheStats {
        audio: directory_usage(&engine_state_dir().join("audio")),
        covers: directory_usage(&data_dir().join("covers")),
    }
}

/// Removes cached content below one exact app-owned directory while retaining
/// named bookkeeping files such as the audio cache's layout marker.
///
/// Callers resolve the root (`engine/audio` or `covers`) before entering this
/// helper; no user input becomes a path. A partial clear is reported instead
/// of pretending success when Windows still has a file open.
pub fn clear_cache_directory(root: &Path, keep: &[&str]) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("could not create cache directory {}: {error}", root.display()))?;
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("could not read cache directory {}: {error}", root.display()))?;
    let mut failures = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep.iter().any(|kept| name.eq_ignore_ascii_case(kept)) {
            continue;
        }
        let path = entry.path();
        let result = match entry.file_type() {
            Ok(kind) if kind.is_dir() && !kind.is_symlink() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error}", path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("could not remove {} cache item(s): {}", failures.len(), failures.join("; ")))
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

fn write_json_atomic_result<T: Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    use std::io::Write as _;

    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temp)
        .map_err(|error| format!("could not create {}: {error}", temp.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    replace_file_atomically(&temp, &path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_settings_missing_startup_fields_use_safe_defaults() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"audio_cache_limit_mb":2048}"#).unwrap();
        assert_eq!(settings.audio_cache_limit_mb, 2048);
        assert!(!settings.launch_at_login);
        assert!(!settings.start_minimized);

        let mut saved = AppSettings::default();
        saved.launch_at_login = true;
        saved.start_minimized = true;
        let round_trip: AppSettings =
            serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
        assert!(round_trip.launch_at_login);
        assert!(round_trip.start_minimized);
    }

    fn track(id: &str) -> Track {
        Track {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            ..Track::default()
        }
    }

    #[test]
    fn playback_snapshot_round_trips_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "spotify-renderer-playback-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let mut snapshot = PlaybackSnapshot {
            version: PLAYBACK_STATE_VERSION,
            queue: vec![Track {
                id: "0123456789ABCDEFGHIJKL".to_owned(),
                uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
                duration_ms: 240_000,
                ..Track::default()
            }],
            current_index: Some(0),
            position_ms: 42_000,
            volume: 37,
            shuffle: true,
            repeat: "context".to_owned(),
            playback_speed: 1.25,
        };
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot.clone()));

        snapshot.position_ms = 84_000;
        save_playback_snapshot_to(&dir, &snapshot).unwrap();
        assert_eq!(load_playback_snapshot_from(&dir), Some(snapshot));
        assert!(!playback_state_path(&dir).with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_or_unknown_playback_snapshots_are_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "spotify-renderer-bad-playback-state-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(playback_state_path(&dir), b"{not-json").unwrap();
        assert!(load_playback_snapshot_from(&dir).is_none());
        std::fs::write(
            playback_state_path(&dir),
            br#"{"version":99,"queue":[],"current_index":null,"position_ms":0,"volume":50,"shuffle":false,"repeat":"off"}"#,
        )
        .unwrap();
        assert!(load_playback_snapshot_from(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn track_with_cover(id: &str, cover_url: &str) -> Track {
        Track {
            cover_url: cover_url.to_owned(),
            ..track(id)
        }
    }

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.to_owned(),
            uri: format!("spotify:playlist:{id}"),
            ..Playlist::default()
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
        let detail = playlist_detail_from_cache(&state, entry);
        assert_eq!(detail.playlist.id, "p1");
        assert_eq!(detail.playlist.uri, "spotify:playlist:p1");
        assert_eq!(detail.playlist.snapshot_id, "rev");
        assert_eq!(detail.playlist.tracks_total, 1);
        assert_eq!(detail.tracks[0].id, "t");
    }

    #[test]
    fn detail_from_cache_derives_candidates_from_the_cached_tracks() {
        let state = AppState::new(PathBuf::from("unused"));
        let entry = PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(1),
            revision: "rev".into(),
            tracks: vec![
                track_with_cover("a", "cover-a"),
                track_with_cover("b", "cover-a"),
                track_with_cover("c", "cover-b"),
            ],
        };
        let detail = playlist_detail_from_cache(&state, entry);
        assert_eq!(detail.playlist.cover_urls, vec!["cover-a", "cover-b"]);
    }

    #[test]
    fn a_library_refresh_carries_local_fields_forward() {
        // The rootlist supplies none of them, and losing the candidates would
        // drop every browsed playlist's mosaic back to a monogram tile until
        // it is browsed again. Both timestamps are local-only as well.
        let previous = vec![Playlist {
            cover_urls: vec!["cover-a".into(), "cover-b".into()],
            snapshot_id: "rev-a".into(),
            last_played: Some(1_000),
            last_activity: Some(2_000),
            ..playlist("p1")
        }];
        let mut fresh = vec![playlist("p1"), playlist("p2")];
        carry_local_fields(&previous, &mut fresh);
        assert_eq!(fresh[0].cover_urls, vec!["cover-a", "cover-b"]);
        assert_eq!(fresh[0].snapshot_id, "rev-a");
        assert_eq!(fresh[0].last_played, Some(1_000));
        assert_eq!(fresh[0].last_activity, Some(2_000));
        // A playlist the previous snapshot never had stays empty — it has
        // never been browsed or used — and keeps its rootlist place.
        assert!(fresh[1].cover_urls.is_empty());
        assert_eq!(fresh[1].last_played, None);
        assert_eq!(fresh[1].last_activity, None);
    }

    #[test]
    fn the_library_is_ordered_most_recently_active_first() {
        let mut playlists = vec![
            playlist("never-a"),
            Playlist {
                last_played: Some(900),
                last_activity: Some(100),
                ..playlist("old")
            },
            playlist("never-b"),
            Playlist {
                last_played: Some(100),
                last_activity: Some(300),
                ..playlist("newest")
            },
            Playlist {
                last_played: Some(500),
                last_activity: Some(200),
                ..playlist("middle")
            },
        ];
        order_by_last_activity(&mut playlists);
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        // Library activity, not listening history, controls this order.
        assert_eq!(ids, vec!["newest", "middle", "old", "never-a", "never-b"]);
    }

    #[test]
    fn playing_from_a_playlist_stamps_played_and_activity_and_moves_it_to_the_front() {
        let mut playlists = vec![playlist("p1"), playlist("p2"), playlist("p3")];
        assert!(touch_playlist_played(&mut playlists, "p3", 500));
        assert_eq!(playlists[0].id, "p3");
        assert_eq!(playlists[0].last_played, Some(500));
        assert_eq!(playlists[0].last_activity, Some(500));
        // The untouched ones keep rootlist order behind it.
        assert_eq!(playlists[1].id, "p1");
        assert_eq!(playlists[2].id, "p2");

        // A later play from a different playlist takes the lead in turn.
        assert!(touch_playlist_played(&mut playlists, "p1", 600));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);

        // Playback from outside the library (an album, an artist page) is a no-op.
        assert!(!touch_playlist_played(&mut playlists, "not-followed", 700));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);
    }

    #[test]
    fn adding_to_a_playlist_stamps_activity_without_creating_listening_history() {
        let mut playlists = vec![
            Playlist {
                last_played: Some(100),
                last_activity: Some(100),
                ..playlist("played")
            },
            playlist("added"),
        ];
        assert!(touch_playlist_activity(&mut playlists, "added", 200));
        assert_eq!(playlists[0].id, "added");
        assert_eq!(playlists[0].last_activity, Some(200));
        assert_eq!(playlists[0].last_played, None);
        assert_eq!(playlists[1].last_played, Some(100));
        assert_eq!(playlists[1].last_activity, Some(100));

        // A missing playlist cannot create either kind of stamp.
        assert!(!touch_playlist_activity(&mut playlists, "not-followed", 300));
    }

    #[test]
    fn a_background_refresh_cannot_erase_activity_stamps() {
        // fetch_playlist upserts the browsed playlist, which carries no
        // timestamp; without the guard in upsert_playlist that would undo the
        // stamps written moments earlier by touch commands.
        let mut playlists = vec![Playlist {
            last_played: Some(900),
            last_activity: Some(950),
            ..playlist("p1")
        }];
        upsert_playlist(
            &mut playlists,
            Playlist {
                snapshot_id: "rev-b".into(),
                ..playlist("p1")
            },
        );
        assert_eq!(playlists[0].last_played, Some(900));
        assert_eq!(playlists[0].last_activity, Some(950));
        assert_eq!(playlists[0].snapshot_id, "rev-b");

        // Explicit newer timestamps still win independently.
        upsert_playlist(
            &mut playlists,
            Playlist {
                last_played: Some(1_000),
                last_activity: Some(1_100),
                ..playlist("p1")
            },
        );
        assert_eq!(playlists[0].last_played, Some(1_000));
        assert_eq!(playlists[0].last_activity, Some(1_100));
    }

    #[test]
    fn a_library_cache_written_before_cover_urls_existed_still_loads() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-old-cache-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("playlist_list.json"),
            r#"{"version":1,"fetched_at":42,"me_id":"me","playlists":[
                {"id":"p1","uri":"spotify:playlist:p1","name":"Mixtape","owner":"me",
                 "owner_id":"me","cover_url":"","collaborative":false,"tracks_total":7,
                 "snapshot_id":"rev-a"}]}"#,
        )
        .unwrap();
        let cache = load_playlist_list(&dir).expect("an old cache file must still deserialize");
        assert_eq!(cache.playlists.len(), 1);
        assert_eq!(cache.playlists[0].snapshot_id, "rev-a");
        assert!(cache.playlists[0].cover_urls.is_empty());
        // Written before either local timestamp existed: it reads as never
        // used, so the playlist keeps its rootlist position and does not
        // invent a Home listening-history entry.
        assert_eq!(cache.playlists[0].last_played, None);
        assert_eq!(cache.playlists[0].last_activity, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn local_activity_stamps_survive_a_save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-lru-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        save_playlist_list(
            &dir,
            &PlaylistListCache {
                version: 1,
                fetched_at: Some(42),
                me_id: "me".to_owned(),
                playlists: vec![
                    Playlist {
                        last_played: Some(1_700),
                        last_activity: Some(1_800),
                        ..playlist("opened")
                    },
                    playlist("never"),
                ],
            },
        );
        let cache = load_playlist_list(&dir).expect("the cache round-trips");
        assert_eq!(cache.playlists[0].last_played, Some(1_700));
        assert_eq!(cache.playlists[0].last_activity, Some(1_800));
        assert_eq!(cache.playlists[1].last_played, None);
        assert_eq!(cache.playlists[1].last_activity, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_usage_totals_files_recursively_and_skips_bookkeeping() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-usage-{}", now_secs()));
        let shard = dir.join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        // The audio cache shape: a layout marker beside sharded song files.
        std::fs::write(dir.join("cache-version"), "2\n").unwrap();
        std::fs::write(shard.join("song-one"), vec![0u8; 1_000]).unwrap();
        std::fs::write(shard.join("song-two"), vec![0u8; 2_500]).unwrap();

        let usage = directory_usage(&dir);
        assert_eq!(usage.files, 2, "the version marker is not a cached song");
        assert_eq!(usage.bytes, 3_500);

        // A directory that was never created reads as empty, not as an error.
        assert_eq!(directory_usage(&dir.join("missing")), CacheUsage::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_a_cache_keeps_only_explicit_bookkeeping() {
        let dir = std::env::temp_dir().join(format!(
            "spotify-renderer-clear-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let shard = dir.join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(dir.join("cache-version"), "2\n").unwrap();
        std::fs::write(dir.join("loose-cover"), b"cover").unwrap();
        std::fs::write(shard.join("song"), b"audio").unwrap();

        clear_cache_directory(&dir, &["cache-version"]).unwrap();
        assert!(dir.join("cache-version").exists());
        assert!(!dir.join("loose-cover").exists());
        assert!(!shard.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tracks_cache_written_before_artist_ids_loads_with_aligned_lists() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-old-tracks-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("playlist_tracks_cache.json"),
            r#"{"version":1,"saved_at":42,"playlists":[{"id":"p1","fetched_at":1,"revision":"rev",
                "tracks":[{"id":"t1","uri":"spotify:track:t1","name":"Track",
                           "artist_names":["A","B","C"],"artist_id":"a1",
                           "album_id":"al","album_name":"Album","cover_url":"","duration_ms":1000}]}]}"#,
        )
        .unwrap();
        let loaded = load_tracks_cache(&dir);
        let track = &loaded[0].tracks[0];
        assert_eq!(track.artist_names.len(), 3);
        assert_eq!(
            track.artist_ids.len(),
            3,
            "an upgraded cache must still zip index-for-index"
        );
        assert!(track.artist_ids.iter().all(|id| id.is_empty()));
        assert_eq!(track.artist_id, "a1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audio cache can be cleared or pruned between two runs of the app,
    /// and this file would not know. A download mark read back off disk is a
    /// claim about a directory this cache does not own, so it is dropped.
    #[test]
    fn a_tracks_cache_never_restores_a_download_mark() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-cached-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        let entries = vec![PlaylistTracksEntry {
            id: "p1".into(),
            fetched_at: Some(42),
            revision: "rev".into(),
            tracks: vec![Track { cached: true, ..track("t") }],
        }];
        save_tracks_cache(&dir, &entries);
        assert!(
            !load_tracks_cache(&dir)[0].tracks[0].cached,
            "only a live browse may claim a track is on disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
