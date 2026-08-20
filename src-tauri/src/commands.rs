//! Tauri command layer: frontend-facing commands plus the background tasks
//! that keep `AppState`, the disk caches, and the frontend events in sync.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::app::{
    AppSettings, AppState, PlaylistListCache, PlaylistTracksEntry, CACHE_STATS_TTL_SECS,
    LIBRARY_LENGTH,
    carry_local_fields, clear_cache_directory, compute_cache_stats, data_dir, engine_state_dir,
    load_app_settings, load_playlist_list, now_secs, order_by_last_played,
    playlist_detail_from_cache, save_app_settings, save_playlist_list, save_tracks_cache,
    touch_playlist_played, upsert_playlist, upsert_tracks_cache,
};
use crate::covers;
use crate::engine_client::{EngineClient, PositionHeartbeat, RestoreSnapshot, StateLine};
use crate::log;
use crate::types::{
    AlbumDetail, AppState as AppStateSnapshot, ArtistCataloguePageDetail, ArtistDetail,
    ArtistReleasePageDetail, CacheStats, LikedSongsDetail, Playlist, PlaylistDetail, SearchResult,
    Track, TrackCreditsDetail,
};

// ---------------------------------------------------------------------------
// Playback commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn play(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.play().await
}

#[tauri::command]
pub async fn pause(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.pause().await
}

#[tauri::command]
pub async fn next(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.next().await
}

#[tauri::command]
pub async fn previous(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.previous().await
}

#[tauri::command]
pub async fn seek(client: State<'_, Arc<EngineClient>>, position_ms: u32) -> Result<(), String> {
    client.seek(position_ms).await
}

#[tauri::command]
pub async fn set_volume(client: State<'_, Arc<EngineClient>>, percent: u8) -> Result<(), String> {
    client.set_volume(percent).await
}

#[tauri::command]
pub async fn set_shuffle(client: State<'_, Arc<EngineClient>>, enabled: bool) -> Result<(), String> {
    client.set_shuffle(enabled).await
}

#[tauri::command]
pub async fn set_repeat(client: State<'_, Arc<EngineClient>>, mode: String) -> Result<(), String> {
    client.set_repeat(&mode).await
}

#[tauri::command]
pub async fn play_queue(
    client: State<'_, Arc<EngineClient>>,
    queue: Vec<Track>,
    index: usize,
) -> Result<(), String> {
    client.play_queue(&queue, index, 0).await
}

#[tauri::command]
pub async fn play_queue_index(
    client: State<'_, Arc<EngineClient>>,
    index: usize,
) -> Result<(), String> {
    client.play_queue_index(index).await
}

#[tauri::command]
pub async fn add_queue(client: State<'_, Arc<EngineClient>>, track: Track) -> Result<(), String> {
    client.add_queue(&track).await
}

#[tauri::command]
pub async fn add_queue_batch(
    client: State<'_, Arc<EngineClient>>,
    tracks: Vec<Track>,
) -> Result<(), String> {
    client.add_queue_batch(&tracks).await
}

#[tauri::command]
pub async fn remove_queue(client: State<'_, Arc<EngineClient>>, index: usize) -> Result<(), String> {
    client.remove_queue(index).await
}

#[tauri::command]
pub async fn move_queue(
    client: State<'_, Arc<EngineClient>>,
    from: usize,
    to: usize,
) -> Result<(), String> {
    client.move_queue(from, to).await
}

// ---------------------------------------------------------------------------
// Browse commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn search(
    client: State<'_, Arc<EngineClient>>,
    query: String,
    limit: Option<usize>,
) -> Result<SearchResult, String> {
    let limit = limit.unwrap_or(10).clamp(1, 50);
    let browse = client.browse_search(&query, limit).await?;
    Ok(SearchResult::from(browse))
}

#[tauri::command]
pub async fn browse_playlists(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
) -> Result<Vec<Playlist>, String> {
    let result = fetch_library(&state, &client).await;
    if result.is_err() {
        // The engine may still be coming up; retry in the background so
        // the cached library is replaced as soon as it can be fetched.
        spawn_refresh_library(app.clone());
    }
    result
}

/// Opens a playlist: serves the disk cache instantly when present and
/// refreshes it in the background; otherwise fetches from the engine.
#[tauri::command]
pub async fn browse_playlist(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<PlaylistDetail, String> {
    let cached = {
        let guard = state.lock();
        guard
            .tracks_cache
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .map(|entry| playlist_detail_from_cache(&guard, entry))
    };
    if let Some(detail) = cached {
        spawn_refresh_playlist(app, id);
        return Ok(detail);
    }
    fetch_playlist(&state, &client, &id).await
}

#[tauri::command]
pub async fn browse_album(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<AlbumDetail, String> {
    Ok(AlbumDetail::from(client.browse_album(&id).await?))
}

#[tauri::command]
pub async fn browse_artist(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<ArtistDetail, String> {
    Ok(ArtistDetail::from(client.browse_artist(&id).await?))
}

#[tauri::command]
pub async fn browse_artist_releases(
    client: State<'_, Arc<EngineClient>>,
    id: String,
    release_types: Option<Vec<String>>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ArtistReleasePageDetail, String> {
    Ok(ArtistReleasePageDetail::from(
        client
            .browse_artist_releases(
                &id,
                release_types.as_deref().unwrap_or_default(),
                offset.unwrap_or(0),
                limit.unwrap_or(20).clamp(1, 40),
            )
            .await?,
    ))
}

#[tauri::command]
pub async fn browse_artist_catalogue(
    client: State<'_, Arc<EngineClient>>,
    id: String,
    release_types: Option<Vec<String>>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ArtistCataloguePageDetail, String> {
    Ok(ArtistCataloguePageDetail::from(
        client
            .browse_artist_catalogue(
                &id,
                release_types.as_deref().unwrap_or_default(),
                offset.unwrap_or(0),
                limit.unwrap_or(4).clamp(1, 6),
            )
            .await?,
    ))
}

#[tauri::command]
pub async fn browse_liked_songs(
    client: State<'_, Arc<EngineClient>>,
    cursor: Option<String>,
) -> Result<LikedSongsDetail, String> {
    Ok(LikedSongsDetail::from(
        client.browse_liked_songs(cursor.as_deref()).await?,
    ))
}

/// Songwriter/producer/performer credits for one track.
///
/// Returned only, never emitted: credits are opened for one track at a time
/// from an overflow menu, so the caller that asked is the only consumer and a
/// broadcast would just be a second copy of the payload crossing IPC.
///
/// One ~1 KB request per invocation, and nothing prefetches it — this is the
/// only endpoint here whose cost scales per track rather than per page, so it
/// must stay strictly on demand.
#[tauri::command]
pub async fn browse_track_credits(
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<TrackCreditsDetail, String> {
    Ok(TrackCreditsDetail::from(
        client.browse_track_credits(&id).await?,
    ))
}

// ---------------------------------------------------------------------------
// Playlist edit commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn create_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    name: String,
) -> Result<(), String> {
    client.create_playlist(&name).await?;
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn rename_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    name: String,
) -> Result<(), String> {
    client.rename_playlist(&id, &name).await?;
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn delete_playlist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<(), String> {
    client.delete_playlist(&id).await?;
    {
        let guard = app.state::<Mutex<AppState>>();
        let mut guard = guard.lock();
        guard.playlists.retain(|playlist| playlist.id != id);
        guard.tracks_cache.retain(|entry| entry.id != id);
    }
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn add_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    uris: Vec<String>,
) -> Result<(), String> {
    client.add_playlist_tracks(&id, &uris).await?;
    spawn_refresh_playlist(app, id);
    Ok(())
}

#[tauri::command]
pub async fn remove_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    uris: Vec<String>,
) -> Result<(), String> {
    client.remove_playlist_tracks(&id, &uris).await?;
    spawn_refresh_playlist(app, id);
    Ok(())
}

#[tauri::command]
pub async fn reorder_playlist_tracks(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
    from: usize,
    to: usize,
) -> Result<(), String> {
    client.reorder_playlist_tracks(&id, from, to).await?;
    spawn_refresh_playlist(app, id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Session commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn status(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.status().await
}

#[tauri::command]
pub async fn login(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.login().await
}

#[tauri::command]
pub async fn logout(client: State<'_, Arc<EngineClient>>) -> Result<(), String> {
    client.logout().await?;
    client.clear_restore_pending().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// State + covers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_state(state: State<'_, Mutex<AppState>>) -> Result<AppStateSnapshot, String> {
    let guard = state.lock();
    Ok(AppStateSnapshot {
        playback: guard.playback.clone(),
        playlists: guard.playlists.clone(),
        me_id: guard.me_id.clone(),
    })
}

#[tauri::command]
pub async fn get_cover(url: String) -> Result<String, String> {
    covers::get_cover(&url).await
}

/// Records that the user started playback *from* playlist `id`, which is what
/// the library's most-recent-first ordering is built on.
///
/// The frontend calls this because only the frontend knows the answer. A play
/// command carries track URIs, and the same track sits in any number of
/// playlists, so nothing on this side could work out which playlist a play
/// came from — it could only guess. The view the user pressed play in is the
/// unambiguous source, so it passes the id.
///
/// Browsing a playlist deliberately does not count: looking at something is
/// not using it.
///
/// The re-ordered library is persisted but *not* re-emitted. Promoting a row
/// the instant it is played would slide it to the top under the user's
/// cursor; the new order is picked up at the next natural library refresh or
/// at the next launch instead.
#[tauri::command]
pub async fn touch_playlist(
    state: State<'_, Mutex<AppState>>,
    id: String,
) -> Result<(), String> {
    let (dir, cache) = {
        let mut guard = state.lock();
        if !touch_playlist_played(&mut guard.playlists, &id, now_secs()) {
            // Playback started somewhere that is not a followed playlist (an
            // album, an artist page): nothing to order.
            return Ok(());
        }
        (
            guard.data_dir.clone(),
            PlaylistListCache {
                version: 1,
                fetched_at: guard.playlists_fetched_at,
                me_id: guard.me_id.clone(),
                playlists: guard.playlists.clone(),
            },
        )
    };
    save_playlist_list(&dir, &cache);
    Ok(())
}

#[tauri::command]
pub fn get_app_settings() -> AppSettings {
    load_app_settings()
}

#[tauri::command]
pub fn set_audio_cache_limit(mb: u64) -> Result<AppSettings, String> {
    if !matches!(mb, 0 | 1024 | 2048 | 4096 | 8192) {
        return Err("audio cache limit must be 1, 2, 4, or 8 GiB, or unlimited".to_owned());
    }
    let mut settings = load_app_settings();
    settings.audio_cache_limit_mb = mb;
    save_app_settings(&settings)?;
    Ok(settings)
}

/// File count and total bytes of the audio cache and the cover cache.
///
/// Two guards keep a Settings visit from costing anything noticeable. The
/// walk runs on the blocking pool, so counting thousands of files never
/// occupies an async worker (and never the UI thread, which only ever awaits
/// the IPC reply); and the result is memoised for [`CACHE_STATS_TTL_SECS`],
/// so a Settings page that re-invokes on every render still walks the disk at
/// most once a minute.
#[tauri::command]
pub async fn get_cache_stats(state: State<'_, Mutex<AppState>>) -> Result<CacheStats, String> {
    let now = now_secs();
    {
        let guard = state.lock();
        if let Some((computed_at, stats)) = guard.cache_stats {
            if now.saturating_sub(computed_at) < CACHE_STATS_TTL_SECS {
                return Ok(stats);
            }
        }
    }
    let stats = tauri::async_runtime::spawn_blocking(compute_cache_stats)
        .await
        .map_err(|error| format!("could not measure the caches: {error}"))?;
    state.lock().cache_stats = Some((now, stats));
    Ok(stats)
}

/// Clears one cache after an explicit Settings confirmation. Clearing audio
/// first stops playback and empties the queue so no decoder/download task can
/// keep a cache file open while Windows removes it. Credentials, volume,
/// playlist metadata, and diagnostic logs are outside both target directories.
#[tauri::command]
pub async fn clear_cache(
    kind: String,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
) -> Result<CacheStats, String> {
    let (root, keep): (std::path::PathBuf, &'static [&'static str]) = match kind.as_str() {
        "audio" => {
            // A logged-out/not-yet-ready engine has no active audio handles;
            // failure to empty that already-empty queue is harmless.
            let _ = client.play_queue(&[], 0, 0).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            (engine_state_dir().join("audio"), &["cache-version"])
        }
        "covers" => (data_dir().join("covers"), &[]),
        _ => return Err("cache kind must be 'audio' or 'covers'".to_owned()),
    };
    tauri::async_runtime::spawn_blocking(move || clear_cache_directory(&root, keep))
        .await
        .map_err(|error| format!("could not clear the {kind} cache: {error}"))??;

    let stats = tauri::async_runtime::spawn_blocking(compute_cache_stats)
        .await
        .map_err(|error| format!("could not measure caches after clearing: {error}"))?;
    state.lock().cache_stats = Some((now_secs(), stats));
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Background tasks
// ---------------------------------------------------------------------------

/// Library refresh retries: at most [`LIBRARY_RETRY_ATTEMPTS`] total fetch
/// attempts, with 5s backoff doubling to a 60s cap between attempts.
const LIBRARY_RETRY_ATTEMPTS: usize = 5;
const LIBRARY_RETRY_BASE: Duration = Duration::from_secs(5);
const LIBRARY_RETRY_MAX: Duration = Duration::from_secs(60);

/// Delay before the retry after the `attempt`-th failure (1-based): 5s,
/// doubling, capped at 60s.
fn library_retry_delay(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(4) as u32;
    LIBRARY_RETRY_BASE
        .saturating_mul(2_u32.pow(exponent))
        .min(LIBRARY_RETRY_MAX)
}

/// Applies a scalar position heartbeat to the shared snapshot: only the two
/// playhead scalars change, in place. The engine already distinguishes
/// heartbeats from real changes, so this never clones or compares the
/// queue — the cost is O(1) in queue length. Full states replace the whole
/// snapshot as before.
fn apply_position_heartbeat(snapshot: &mut AppState, heartbeat: PositionHeartbeat) {
    snapshot.playback.position_ms = heartbeat.position_ms;
    snapshot.playback.duration_ms = heartbeat.duration_ms;
}

/// Consumes engine state lines, mirrors them into `AppState`, and emits the
/// `state`/`position`/`session` events. Scalar position heartbeats are
/// forwarded directly as `position` — the playhead is projected in the
/// frontend between engine heartbeats, so there is no periodic work here.
pub async fn consume_states(app: AppHandle) {
    // Owned handle so spawned tasks do not borrow the AppHandle.
    let client = app.state::<Arc<EngineClient>>().inner().clone();
    let mut lines = client.subscribe_lines();

    // Instant first paint: hydrate `AppState` from the on-disk library
    // snapshot and emit it before the engine is ready, so a returning user
    // sees their playlists immediately (the frontend may also pull it via
    // get_state). The ready-transition fetch below supersedes it.
    load_library_from_disk(&app);

    let mut previous_identity: Option<(String, String)> = None;
    let mut last_error = String::new();

    loop {
        let state = match lines.recv().await {
            Ok(StateLine::State(state)) => state,
            Ok(StateLine::Position(heartbeat)) => {
                // A heartbeat only moved the playhead: freshen the snapshot
                // scalars in place and forward the existing scalar `position`
                // event unchanged (a number, no queue payload).
                let managed = app.state::<Mutex<AppState>>();
                let mut guard = managed.lock();
                apply_position_heartbeat(&mut guard, heartbeat);
                drop(guard);
                let _ = app.emit("position", heartbeat.position_ms);
                continue;
            }
            // The engine out-ran this consumer; the next line re-syncs
            // (a skipped full state gets re-emitted by the engine, and a
            // skipped heartbeat is just one projection step).
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };

        let (became_ready, session_changed, auth_changed) = match &previous_identity {
            Some((auth_state, username)) => (
                state.auth_state == "ready" && auth_state.as_str() != "ready",
                state.auth_state != auth_state.as_str() || state.username != username.as_str(),
                state.auth_state != auth_state.as_str(),
            ),
            None => (state.auth_state == "ready", true, true),
        };

        if auth_changed {
            log::info(&format!("engine auth_state -> {}", state.auth_state));
        }
        if became_ready {
            log::info(&format!("engine ready; username={}", state.username));
        }
        if !state.error.is_empty() && state.error != last_error {
            last_error = state.error.clone();
            log::error(&format!("engine error: {}", state.error));
        }

        // A respawned engine becomes ready with a blank session; put the
        // pre-crash playback back before anything else sees it.
        if let Some(snapshot) = client.take_restore_pending().await {
            if state.auth_state == "ready" {
                restore_playback(&client, &snapshot).await;
            } else {
                client.put_restore_pending(snapshot).await;
            }
        }

        {
            let guard = app.state::<Mutex<AppState>>();
            let mut guard = guard.lock();
            guard.playback = state.clone();
            if !state.username.is_empty() {
                guard.me_id = state.username.clone();
            }
        }

        // Full states are reserved for real changes; heartbeats were already
        // forwarded as scalar `position` events above.
        let _ = app.emit("state", &state);
        if session_changed {
            let _ = app.emit(
                "session",
                json!({
                    "auth_state": state.auth_state,
                    "username": state.username,
                    "error": state.error,
                }),
            );
        }
        if became_ready {
            spawn_refresh_library(app.clone());
        }

        previous_identity = Some((state.auth_state, state.username));
    }
}

/// Re-applies the pre-crash queue/volume/shuffle/repeat to a fresh engine.
async fn restore_playback(client: &EngineClient, snapshot: &RestoreSnapshot) {
    if !snapshot.queue.is_empty() {
        let index = snapshot
            .current_index
            .unwrap_or(0)
            .min(snapshot.queue.len().saturating_sub(1));
        if let Err(error) =
            client.play_queue(&snapshot.queue, index, snapshot.position_ms).await
        {
            log::warn(&format!(
                "could not restore the queue after engine restart: {error}"
            ));
        }
    }
    if let Err(error) = client.set_volume(snapshot.volume).await {
        log::warn(&format!("could not restore volume: {error}"));
    }
    let _ = client.set_shuffle(snapshot.shuffle).await;
    let _ = client.set_repeat(&snapshot.repeat).await;
}

/// Hydrates `AppState` from the on-disk library snapshot (instant first
/// paint) and emits it as `library`; the engine refresh that supersedes it
/// is spawned separately once the engine reports ready.
fn load_library_from_disk(app: &AppHandle) {
    let dir = data_dir();
    let playlists = load_playlist_list(&dir);
    {
        let guard = app.state::<Mutex<AppState>>();
        let mut guard = guard.lock();
        if let Some(cache) = &playlists {
            guard.playlists = cache.playlists.clone();
            guard.playlists_fetched_at = cache.fetched_at;
            if !cache.me_id.is_empty() {
                guard.me_id = cache.me_id.clone();
            }
        }
    }
    if let Some(cache) = playlists {
        let _ = app.emit("library", &cache.playlists);
    }
}

/// One background library-refresh chain: a fetch, then capped-exponential
/// retries so a transient browse failure (engine still coming up, spclient
/// hiccup) does not leave the library permanently stale. Only one chain runs
/// at a time; concurrent triggers (ready transitions, playlist edits, the
/// frontend boot pull) coalesce onto it.
fn spawn_refresh_library(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        {
            let mut guard = state.lock();
            if guard.library_fetching {
                // A chain is mid-flight, and its backoff can run for a
                // minute. Dropping this trigger would strand whatever just
                // changed (a rename, a new playlist) until the next restart,
                // so mark it and let the running chain pick it up.
                guard.library_refresh_queued = true;
                return;
            }
            guard.library_fetching = true;
        }
        loop {
            let result = refresh_library_with_retry(&state, &client, &app).await;
            if let Err(error) = result {
                log::error(&format!(
                    "library refresh failed after {LIBRARY_RETRY_ATTEMPTS} attempts: {error}"
                ));
            }
            let mut guard = state.lock();
            if !std::mem::take(&mut guard.library_refresh_queued) {
                guard.library_fetching = false;
                return;
            }
        }
    });
}

/// Fetches the library, retrying with capped exponential backoff until it
/// succeeds or [`LIBRARY_RETRY_ATTEMPTS`] attempts are exhausted. The cached
/// copy stays untouched (and on screen) while retries are in flight.
async fn refresh_library_with_retry(
    state: &Mutex<AppState>,
    client: &EngineClient,
    app: &AppHandle,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=LIBRARY_RETRY_ATTEMPTS {
        match fetch_library(state, client).await {
            Ok(playlists) => {
                let _ = app.emit("library", &playlists);
                return Ok(());
            }
            Err(error) => {
                log::warn(&format!(
                    "library refresh attempt {attempt} failed: {error}"
                ));
                last_error = error;
                if attempt < LIBRARY_RETRY_ATTEMPTS {
                    let delay = library_retry_delay(attempt);
                    log::warn(&format!("retrying library refresh in {}s", delay.as_secs()));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error)
}

fn spawn_refresh_playlist(app: AppHandle, id: String) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        if let Err(error) = fetch_playlist(&state, &client, &id).await {
            log::error(&format!(
                "background refresh of playlist {id} failed: {error}"
            ));
        }
    });
}

/// Engine round-trip for the library, stored to the disk cache.
async fn fetch_library(
    state: &Mutex<AppState>,
    client: &EngineClient,
) -> Result<Vec<Playlist>, String> {
    let references = client.browse_playlists(LIBRARY_LENGTH).await?;
    let mut playlists: Vec<Playlist> = references.iter().map(Playlist::from).collect();
    let fetched_at = now_secs();
    let (dir, me_id) = {
        let guard = state.lock();
        carry_local_fields(&guard.playlists, &mut playlists);
        (guard.data_dir.clone(), guard.me_id.clone())
    };
    // Rootlist order is only the tiebreaker; the fetch arrives in it, so the
    // sort has to happen after the timestamps are carried over.
    order_by_last_played(&mut playlists);
    save_playlist_list(
        &dir,
        &PlaylistListCache {
            version: 1,
            fetched_at: Some(fetched_at),
            me_id,
            playlists: playlists.clone(),
        },
    );
    {
        let mut guard = state.lock();
        guard.playlists = playlists.clone();
        guard.playlists_fetched_at = Some(fetched_at);
    }
    Ok(playlists)
}

/// Engine round-trip for one playlist; updates the tracks cache and the
/// library entry (snapshot id, track total) and saves both caches.
async fn fetch_playlist(
    state: &Mutex<AppState>,
    client: &EngineClient,
    id: &str,
) -> Result<PlaylistDetail, String> {
    let detail = PlaylistDetail::from(client.browse_playlist(id).await?);
    let fetched_at = now_secs();
    let mut guard = state.lock();
    upsert_tracks_cache(
        &mut guard.tracks_cache,
        PlaylistTracksEntry {
            id: detail.playlist.id.clone(),
            fetched_at: Some(fetched_at),
            revision: detail.playlist.snapshot_id.clone(),
            tracks: detail.tracks.clone(),
        },
    );
    upsert_playlist(&mut guard.playlists, detail.playlist.clone());
    let dir = guard.data_dir.clone();
    let list_cache = PlaylistListCache {
        version: 1,
        fetched_at: guard.playlists_fetched_at,
        me_id: guard.me_id.clone(),
        playlists: guard.playlists.clone(),
    };
    let tracks_cache = guard.tracks_cache.clone();
    drop(guard);
    save_tracks_cache(&dir, &tracks_cache);
    save_playlist_list(&dir, &list_cache);
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_retry_delay_doubles_from_5s_and_caps_at_60s() {
        let delays: Vec<u64> = (1..=6)
            .map(|attempt| library_retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(delays, vec![5, 10, 20, 40, 60, 60]);
    }

    fn playing_state(position_ms: u32) -> PlaybackState {
        PlaybackState {
            auth_state: "ready".to_owned(),
            playing: true,
            position_ms,
            duration_ms: 200_000,
            current_index: Some(0),
            current_uri: "spotify:track:a".to_owned(),
            queue: vec![Track::default()],
            ..PlaybackState::default()
        }
    }

    #[test]
    fn position_heartbeats_update_only_the_playhead_scalars_in_place() {
        let mut snapshot = AppState::new(std::path::PathBuf::new());
        snapshot.playback = playing_state(1_000);
        let queue_ptr = snapshot.playback.queue.as_ptr();

        apply_position_heartbeat(
            &mut snapshot,
            PositionHeartbeat {
                position_ms: 3_000,
                duration_ms: 250_000,
            },
        );

        assert_eq!(snapshot.playback.position_ms, 3_000);
        assert_eq!(snapshot.playback.duration_ms, 250_000);
        // The queue is never cloned, compared, or rebuilt: the scalars are
        // written in place over the existing snapshot.
        assert_eq!(
            snapshot.playback.queue.as_ptr(),
            queue_ptr,
            "heartbeat must not touch the queue"
        );
        assert_eq!(snapshot.playback.playing, true);
        assert_eq!(snapshot.playback.current_uri, "spotify:track:a");
        assert_eq!(snapshot.playback.volume, 50);
        assert_eq!(snapshot.playback.queue, vec![Track::default()]);
    }
}
