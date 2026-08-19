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
    align_artist_ids, cover_urls_from_tracks, CacheStats, CacheUsage, PlaybackState, Playlist,
    PlaylistDetail, Track,
};

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
    /// True while the background cover-candidate sweep is running.
    #[serde(skip)]
    pub covers_sweeping: bool,
    /// Same coalescing as `library_refresh_queued`: a sweep snapshots its
    /// worklist up front, so a trigger arriving mid-sweep re-runs it once.
    #[serde(skip)]
    pub covers_sweep_queued: bool,
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
            covers_sweeping: false,
            covers_sweep_queued: false,
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
        Ok(mut cache) => {
            // Entries written before `artist_ids` existed carry names but no
            // ids; align them on the way in so the two lists are always the
            // same length, whatever wrote the file.
            for entry in &mut cache.playlists {
                for track in &mut entry.tracks {
                    align_artist_ids(track);
                }
            }
            cache.playlists
        }
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
    // These tracks are what the detail page renders, so its candidates come
    // from them rather than from whatever revision the library entry was last
    // browsed at — candidates and revision always travel together.
    playlist.cover_urls = cover_urls_from_tracks(&entry.tracks);
    PlaylistDetail {
        playlist,
        tracks: entry.tracks.clone(),
    }
}

/// Upserts a playlist into the in-memory library list.
///
/// A browsed playlist knows nothing about when it was last played from, so an
/// incoming `None` never overwrites a stored timestamp: the background
/// refresh that follows every browse would otherwise erase a stamp written
/// moments earlier.
pub fn upsert_playlist(playlists: &mut Vec<Playlist>, mut playlist: Playlist) {
    if let Some(existing) = playlists.iter_mut().find(|entry| entry.id == playlist.id) {
        playlist.last_played = playlist.last_played.or(existing.last_played);
        *existing = playlist;
    } else {
        playlists.push(playlist);
    }
}

/// Copies the fields the rootlist cannot produce — the revision, the derived
/// cover candidates, and the last-played timestamp — from the previous
/// library snapshot onto a freshly fetched one.
///
/// The rootlist carries none of them, so a plain replacement would blank all
/// three on every library refresh. For the candidates that is not merely
/// cosmetic: the cover sweep keys off "no candidates yet", so a wipe would
/// re-browse the whole library after every refresh — and refreshes fire on
/// engine ready and on every playlist edit — walking straight into Spotify's
/// rate limiter. For `last_played` a wipe would reset the library to rootlist
/// order at the first refresh after startup, which is exactly the ordering
/// the timestamp exists to replace.
pub fn carry_local_fields(previous: &[Playlist], fresh: &mut [Playlist]) {
    for playlist in fresh.iter_mut() {
        if let Some(old) = previous.iter().find(|entry| entry.id == playlist.id) {
            playlist.cover_urls = old.cover_urls.clone();
            playlist.snapshot_id = old.snapshot_id.clone();
            playlist.last_played = old.last_played;
        }
    }
}

/// Sorts the library most-recently-played-from first.
///
/// Playlists never played from keep their rootlist order behind the rest: the
/// sort is stable and their key is the minimum, so an untouched library reads
/// exactly as the rootlist returned it.
///
/// This ordering is entirely ours. Spotify has no such notion and none is
/// wanted here, so the timestamps live only in `playlist_list.json` and are
/// never sent anywhere.
pub fn order_by_last_played(playlists: &mut [Playlist]) {
    playlists.sort_by_key(|playlist| {
        std::cmp::Reverse(playlist.last_played.unwrap_or(i64::MIN))
    });
}

/// Stamps `id` as played from at `at` and re-sorts the library so it leads.
///
/// Deliberately *not* driven by opening a playlist: browsing one is not using
/// it, and the owner wants the library to reflect what gets played. Which
/// playlist a play came from is also not derivable here — the same track sits
/// in many playlists, so a track URI cannot identify its origin — which is
/// why the caller passes the id explicitly rather than anything inferring it.
///
/// Returns false when the id is not in the library — playback started from an
/// album, an artist page, or a playlist the user does not follow — in which
/// case nothing changed and the caller can skip persisting.
pub fn touch_playlist_played(playlists: &mut Vec<Playlist>, id: &str, at: i64) -> bool {
    let Some(playlist) = playlists.iter_mut().find(|entry| entry.id == id) else {
        return false;
    };
    playlist.last_played = Some(at);
    order_by_last_played(playlists);
    true
}

/// Ids of the playlists with no artwork of any kind: Spotify returned no
/// custom cover and no browse has derived candidates yet. This is the sweep's
/// worklist.
pub fn playlists_missing_covers(playlists: &[Playlist]) -> Vec<String> {
    playlists
        .iter()
        .filter(|playlist| playlist.cover_url.is_empty() && playlist.cover_urls.is_empty())
        .map(|playlist| playlist.id.clone())
        .collect()
}

/// Writes a browse's cover candidates onto the matching library entry.
///
/// Candidates and `snapshot_id` are replaced as a pair, never independently:
/// candidates are taken from the first tracks of one browse, and a revision
/// bump can replace exactly those tracks, so a stored pair whose revision no
/// longer matches is stale by construction.
///
/// Returns false when the playlist is gone — deleted while the sweep that
/// browsed it was in flight.
pub fn apply_cover_candidates(playlists: &mut [Playlist], browsed: &Playlist) -> bool {
    let Some(entry) = playlists.iter_mut().find(|entry| entry.id == browsed.id) else {
        return false;
    };
    entry.cover_urls = browsed.cover_urls.clone();
    entry.snapshot_id = browsed.snapshot_id.clone();
    // A real cover outranks any mosaic, and the browse sees custom artwork the
    // rootlist omits — but an empty one there means "not reported", not
    // "removed", so it must not clobber what the rootlist did give us.
    if !browsed.cover_url.is_empty() {
        entry.cover_url = browsed.cover_url.clone();
    }
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
        let detail = playlist_detail_from_cache(&state, &entry);
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
        let detail = playlist_detail_from_cache(&state, &entry);
        assert_eq!(detail.playlist.cover_urls, vec!["cover-a", "cover-b"]);
    }

    #[test]
    fn a_library_refresh_carries_the_local_fields_forward() {
        // The rootlist supplies none of them, and losing the candidates would
        // put the whole library back into the sweep's worklist.
        let previous = vec![Playlist {
            cover_urls: vec!["cover-a".into(), "cover-b".into()],
            snapshot_id: "rev-a".into(),
            last_played: Some(1_000),
            ..playlist("p1")
        }];
        let mut fresh = vec![playlist("p1"), playlist("p2")];
        carry_local_fields(&previous, &mut fresh);
        assert_eq!(fresh[0].cover_urls, vec!["cover-a", "cover-b"]);
        assert_eq!(fresh[0].snapshot_id, "rev-a");
        assert_eq!(fresh[0].last_played, Some(1_000));
        // A playlist the previous snapshot never had stays empty, so the sweep
        // picks it up — and stays unopened, so it keeps its rootlist place.
        assert!(fresh[1].cover_urls.is_empty());
        assert_eq!(fresh[1].last_played, None);
    }

    #[test]
    fn the_library_is_ordered_most_recently_played_first() {
        let mut playlists = vec![
            playlist("never-a"),
            Playlist { last_played: Some(100), ..playlist("old") },
            playlist("never-b"),
            Playlist { last_played: Some(300), ..playlist("newest") },
            Playlist { last_played: Some(200), ..playlist("middle") },
        ];
        order_by_last_played(&mut playlists);
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        // Played ones by recency, then the never-played ones in rootlist order.
        assert_eq!(ids, vec!["newest", "middle", "old", "never-a", "never-b"]);
    }

    #[test]
    fn playing_from_a_playlist_stamps_it_and_moves_it_to_the_front() {
        let mut playlists = vec![playlist("p1"), playlist("p2"), playlist("p3")];
        assert!(touch_playlist_played(&mut playlists, "p3", 500));
        assert_eq!(playlists[0].id, "p3");
        assert_eq!(playlists[0].last_played, Some(500));
        // The untouched ones keep rootlist order behind it.
        assert_eq!(playlists[1].id, "p1");
        assert_eq!(playlists[2].id, "p2");

        // A later open of a different playlist takes the lead in turn.
        assert!(touch_playlist_played(&mut playlists, "p1", 600));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);

        // Playback from outside the library (an album, an artist page) is a no-op.
        assert!(!touch_playlist_played(&mut playlists, "not-followed", 700));
        let ids: Vec<&str> = playlists.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p3", "p2"]);
    }

    #[test]
    fn a_background_refresh_cannot_erase_a_play_stamp() {
        // fetch_playlist upserts the browsed playlist, which carries no
        // timestamp; without the guard in upsert_playlist that would undo the
        // stamp written moments earlier by touch_playlist.
        let mut playlists = vec![Playlist { last_played: Some(900), ..playlist("p1") }];
        upsert_playlist(&mut playlists, Playlist { snapshot_id: "rev-b".into(), ..playlist("p1") });
        assert_eq!(playlists[0].last_played, Some(900));
        assert_eq!(playlists[0].snapshot_id, "rev-b");

        // An explicit newer timestamp still wins.
        upsert_playlist(&mut playlists, Playlist { last_played: Some(1_000), ..playlist("p1") });
        assert_eq!(playlists[0].last_played, Some(1_000));
    }

    #[test]
    fn a_browse_at_a_new_revision_replaces_the_stored_candidates() {
        let mut playlists = vec![Playlist {
            cover_urls: vec!["cover-a".into(), "cover-b".into()],
            snapshot_id: "rev-a".into(),
            ..playlist("p1")
        }];
        let browsed = Playlist {
            cover_urls: vec!["cover-c".into()],
            snapshot_id: "rev-b".into(),
            ..playlist("p1")
        };
        assert!(apply_cover_candidates(&mut playlists, &browsed));
        assert_eq!(playlists[0].cover_urls, vec!["cover-c"]);
        assert_eq!(playlists[0].snapshot_id, "rev-b");
        // The browse reported no custom cover; that is "not reported", so what
        // the rootlist gave us stands.
        assert!(playlists[0].cover_url.is_empty());
    }

    #[test]
    fn candidates_for_a_playlist_deleted_mid_sweep_are_dropped() {
        let mut playlists = vec![playlist("p1")];
        let browsed = Playlist {
            cover_urls: vec!["cover-a".into()],
            ..playlist("gone")
        };
        assert!(!apply_cover_candidates(&mut playlists, &browsed));
        assert_eq!(playlists.len(), 1);
        assert!(playlists[0].cover_urls.is_empty());
    }

    #[test]
    fn the_sweep_worklist_is_the_playlists_with_no_artwork_at_all() {
        let playlists = vec![
            playlist("bare"),
            Playlist {
                cover_url: "https://i.scdn.co/image/abc".into(),
                ..playlist("has-cover")
            },
            Playlist {
                cover_urls: vec!["cover-a".into()],
                ..playlist("has-candidates")
            },
        ];
        assert_eq!(playlists_missing_covers(&playlists), vec!["bare"]);
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
        // Written before `last_played` existed: it reads as never-opened, so
        // the playlist keeps its rootlist position instead of sorting to the
        // epoch.
        assert_eq!(cache.playlists[0].last_played, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_last_played_stamp_survives_a_save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("spotify-renderer-lru-{}", now_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        save_playlist_list(
            &dir,
            &PlaylistListCache {
                version: 1,
                fetched_at: Some(42),
                me_id: "me".to_owned(),
                playlists: vec![
                    Playlist { last_played: Some(1_700), ..playlist("opened") },
                    playlist("never"),
                ],
            },
        );
        let cache = load_playlist_list(&dir).expect("the cache round-trips");
        assert_eq!(cache.playlists[0].last_played, Some(1_700));
        assert_eq!(cache.playlists[1].last_played, None);
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
