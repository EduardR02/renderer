//! Tauri command layer: frontend-facing commands plus the background tasks
//! that keep `AppState`, the disk caches, and the frontend events in sync.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::app::{
    AppState, PlaylistListCache, PlaylistTracksEntry, LIBRARY_LENGTH, apply_cover_candidates,
    carry_browse_fields, data_dir, load_playlist_list, now_secs, playlist_detail_from_cache,
    playlists_missing_covers, save_playlist_list, save_tracks_cache, upsert_playlist,
    upsert_tracks_cache,
};
use crate::covers;
use crate::engine_client::{EngineClient, RestoreSnapshot};
use crate::log;
use crate::types::{
    AlbumDetail, AppState as AppStateSnapshot, ArtistDetail, PlaybackState, Playlist,
    PlaylistDetail, SearchResult, Track,
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
pub async fn add_queue(client: State<'_, Arc<EngineClient>>, track: Track) -> Result<(), String> {
    client.add_queue(&track).await
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
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    query: String,
    limit: Option<usize>,
) -> Result<SearchResult, String> {
    let limit = limit.unwrap_or(10).clamp(1, 50);
    let browse = client.browse_search(&query, limit).await?;
    let result = SearchResult::from(browse);
    let _ = app.emit("search-results", &result);
    Ok(result)
}

#[tauri::command]
pub async fn browse_playlists(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, Arc<EngineClient>>,
) -> Result<Vec<Playlist>, String> {
    match fetch_library(&state, &client, &app).await {
        Ok(playlists) => Ok(playlists),
        Err(error) => {
            // The engine may still be coming up; retry in the background so
            // the cached library is replaced as soon as it can be fetched.
            spawn_refresh_library(app.clone());
            Err(error)
        }
    }
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
        guard.tracks_cache.iter().find(|entry| entry.id == id).cloned()
    };
    if let Some(entry) = cached {
        let detail = {
            let guard = state.lock();
            playlist_detail_from_cache(&guard, &entry)
        };
        let _ = app.emit("playlist-tracks", &detail);
        spawn_refresh_playlist(app, id);
        return Ok(detail);
    }
    let detail = fetch_playlist(&state, &client, &id).await?;
    let _ = app.emit("playlist-tracks", &detail);
    Ok(detail)
}

#[tauri::command]
pub async fn browse_album(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<AlbumDetail, String> {
    let detail = AlbumDetail::from(client.browse_album(&id).await?);
    let _ = app.emit("album", &detail);
    Ok(detail)
}

#[tauri::command]
pub async fn browse_artist(
    app: AppHandle,
    client: State<'_, Arc<EngineClient>>,
    id: String,
) -> Result<ArtistDetail, String> {
    let detail = ArtistDetail::from(client.browse_artist(&id).await?);
    let _ = app.emit("artist", &detail);
    Ok(detail)
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

/// True when `next` differs from `previous` in nothing but `position_ms`.
///
/// The engine heartbeats its whole state — queue included — every couple of
/// seconds. Re-emitting that to the frontend just to advance a progress bar
/// costs a full serialize + IPC hop + `JSON.parse` + Svelte proxy rebuild of
/// the entire queue, which is precisely the per-second churn this app exists
/// to avoid. Heartbeats that only moved the playhead go out as a `position`
/// number instead; the frontend projects between them off a monotonic clock.
fn position_only_change(previous: &PlaybackState, next: &PlaybackState) -> bool {
    if previous.position_ms == next.position_ms {
        return false;
    }
    let mut probe = previous.clone();
    probe.position_ms = next.position_ms;
    probe == *next
}

/// Consumes engine state lines, mirrors them into `AppState`, and emits the
/// `state`/`position`/`session` events. There is no periodic work here: the
/// playhead is projected in the frontend between engine heartbeats.
pub async fn consume_states(app: AppHandle) {
    // Owned handle so spawned tasks do not borrow the AppHandle.
    let client = app.state::<Arc<EngineClient>>().inner().clone();
    let mut states = client.subscribe_state();

    // Instant first paint: hydrate `AppState` from the on-disk library
    // snapshot and emit it before the engine is ready, so a returning user
    // sees their playlists immediately (the frontend may also pull it via
    // get_state). The ready-transition fetch below supersedes it.
    load_library_from_disk(&app);

    let mut previous: Option<PlaybackState> = None;
    let mut last_error = String::new();

    loop {
        let state = match states.recv().await {
            Ok(state) => state,
            // The engine out-ran this consumer; the next line is a full
            // state, so resyncing on it loses nothing.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };

        let (became_ready, session_changed, auth_changed, position_only) = match &previous {
            Some(prev) => (
                state.auth_state == "ready" && prev.auth_state != "ready",
                state.auth_state != prev.auth_state || state.username != prev.username,
                state.auth_state != prev.auth_state,
                position_only_change(prev, &state),
            ),
            None => (state.auth_state == "ready", true, true, false),
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

        if position_only {
            let _ = app.emit("position", state.position_ms);
        } else {
            let _ = app.emit("state", &state);
        }
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

        previous = Some(state);
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
        match fetch_library(state, client, app).await {
            Ok(_) => return Ok(()),
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
        match fetch_playlist(&state, &client, &id).await {
            Ok(detail) => {
                let _ = app.emit("playlist-tracks", &detail);
            }
            Err(error) => log::error(&format!(
                "background refresh of playlist {id} failed: {error}"
            )),
        }
    });
}

/// Engine round-trip for the library, stored to the disk cache and emitted
/// as `library`.
async fn fetch_library(
    state: &Mutex<AppState>,
    client: &EngineClient,
    app: &AppHandle,
) -> Result<Vec<Playlist>, String> {
    let references = client.browse_playlists(LIBRARY_LENGTH).await?;
    let mut playlists: Vec<Playlist> = references.iter().map(Playlist::from).collect();
    let fetched_at = now_secs();
    let (dir, me_id) = {
        let guard = state.lock();
        carry_browse_fields(&guard.playlists, &mut playlists);
        (guard.data_dir.clone(), guard.me_id.clone())
    };
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
    let _ = app.emit("library", &playlists);
    // Fire-and-forget: the fresh library is already on screen, and the sweep
    // only fills in artwork for the playlists that have none.
    spawn_cover_sweep(app.clone());
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

// ---------------------------------------------------------------------------
// Cover candidate sweep
// ---------------------------------------------------------------------------

/// At most this many sweep browses are ever in flight.
///
/// Spotify rate-limits — that is why the library refresh backs off at all —
/// and the sweep is the one place in this app that could fire a hundred
/// browses back to back. Two at a time, spaced out, keeps it below what a user
/// clicking through their own playlists already costs.
const COVER_SWEEP_CONCURRENCY: usize = 2;
// The batch loop pairs its browses with `join!`, so the width is fixed at
// compile time: raising the constant alone would leave batches unhandled.
const _: () = assert!(COVER_SWEEP_CONCURRENCY == 2);

/// Pause between sweep batches.
const COVER_SWEEP_BATCH_DELAY: Duration = Duration::from_millis(750);

/// Head start given to whatever the user is doing before the sweep begins. The
/// triggers cluster at startup — the engine's ready transition plus the
/// frontend's own boot pull — which is exactly when the engine is busiest
/// serving what the user is actually looking at.
const COVER_SWEEP_START_DELAY: Duration = Duration::from_secs(5);

/// One background cover-candidate sweep: browses the playlists that have no
/// artwork of any kind so the sidebar and the home grid can paint the same
/// mosaic the detail page does, instead of a monogram tile for the very same
/// playlist. Coalesced like the library refresh, since its trigger — a
/// successful refresh — fires on engine ready and on every playlist edit.
fn spawn_cover_sweep(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<Arc<EngineClient>>();
        let state = app.state::<Mutex<AppState>>();
        {
            let mut guard = state.lock();
            if guard.covers_sweeping {
                guard.covers_sweep_queued = true;
                return;
            }
            guard.covers_sweeping = true;
        }
        tokio::time::sleep(COVER_SWEEP_START_DELAY).await;
        loop {
            sweep_cover_candidates(&state, &client, &app).await;
            let mut guard = state.lock();
            if !std::mem::take(&mut guard.covers_sweep_queued) {
                guard.covers_sweeping = false;
                return;
            }
        }
    });
}

/// Browses every playlist without artwork and fills in its cover candidates.
///
/// The library cache is saved and `library` re-emitted once, at the end: the
/// payload is the whole library, and re-emitting it per playlist would put it
/// back through IPC every few hundred milliseconds for as long as the sweep
/// runs — the per-second churn this app exists to avoid.
async fn sweep_cover_candidates(
    state: &Mutex<AppState>,
    client: &EngineClient,
    app: &AppHandle,
) {
    let pending = {
        let guard = state.lock();
        playlists_missing_covers(&guard.playlists)
    };
    if pending.is_empty() {
        return;
    }
    log::info(&format!(
        "cover sweep: browsing {} playlists with no artwork",
        pending.len()
    ));

    let mut updated = 0_usize;
    for (index, batch) in pending.chunks(COVER_SWEEP_CONCURRENCY).enumerate() {
        if index > 0 {
            tokio::time::sleep(COVER_SWEEP_BATCH_DELAY).await;
        }
        // `join!` rather than spawned tasks: it bounds the browses in flight to
        // the batch by construction, and the sweep stays one task.
        let results = match batch {
            [only] => vec![fetch_cover_candidates(state, client, only).await],
            [first, second] => {
                let (first, second) = tokio::join!(
                    fetch_cover_candidates(state, client, first),
                    fetch_cover_candidates(state, client, second),
                );
                vec![first, second]
            }
            _ => unreachable!("chunks() yields at most COVER_SWEEP_CONCURRENCY ids"),
        };
        for result in results {
            match result {
                Ok(true) => updated += 1,
                // The playlist was deleted while its browse was in flight.
                Ok(false) => {}
                Err(error) => log::warn(&format!("cover sweep browse failed: {error}")),
            }
        }
    }
    if updated == 0 {
        return;
    }

    let (dir, list_cache) = {
        let guard = state.lock();
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
    save_playlist_list(&dir, &list_cache);
    let _ = app.emit("library", &list_cache.playlists);
    log::info(&format!("cover sweep: filled in {updated} playlists"));
}

/// Browses one playlist purely for its cover candidates.
///
/// Deliberately not [`fetch_playlist`]: that inserts into the 25-entry tracks
/// cache, so sweeping a whole library would evict every playlist the user
/// actually opened in favour of whatever the sweep touched last. Returns
/// whether the library entry was updated.
async fn fetch_cover_candidates(
    state: &Mutex<AppState>,
    client: &EngineClient,
    id: &str,
) -> Result<bool, String> {
    let detail = PlaylistDetail::from(client.browse_playlist(id).await?);
    let mut guard = state.lock();
    Ok(apply_cover_candidates(&mut guard.playlists, &detail.playlist))
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
    fn heartbeats_that_only_move_the_playhead_skip_the_full_state_emit() {
        assert!(position_only_change(&playing_state(1_000), &playing_state(3_000)));
        // An unchanged heartbeat is not a position change: nothing is emitted
        // for it either way, but it must not masquerade as one.
        assert!(!position_only_change(&playing_state(1_000), &playing_state(1_000)));
    }

    #[test]
    fn any_other_field_forces_a_full_state_emit() {
        let before = playing_state(1_000);

        let mut paused = playing_state(3_000);
        paused.playing = false;
        assert!(!position_only_change(&before, &paused), "play/pause is a full state");

        let mut skipped = playing_state(3_000);
        skipped.current_uri = "spotify:track:b".to_owned();
        assert!(!position_only_change(&before, &skipped), "track change is a full state");

        let mut requeued = playing_state(3_000);
        requeued.queue.clear();
        assert!(!position_only_change(&before, &requeued), "queue change is a full state");

        let mut louder = playing_state(3_000);
        louder.volume = louder.volume.wrapping_add(1);
        assert!(!position_only_change(&before, &louder), "volume change is a full state");
    }
}
