//! Tauri command layer: frontend-facing commands plus the background tasks
//! that keep `AppState`, the disk caches, and the frontend events in sync.

use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::app::{
    AppState, PlaylistListCache, PlaylistTracksEntry, LIBRARY_LENGTH, data_dir, load_playlist_list,
    now_secs, playlist_detail_from_cache, save_playlist_list, save_tracks_cache, upsert_playlist,
    upsert_tracks_cache,
};
use crate::covers;
use crate::engine_client::{EngineClient, RestoreSnapshot};
use crate::types::{
    AlbumDetail, AppState as AppStateSnapshot, ArtistDetail, Playlist, PlaylistDetail,
    SearchResult, Track,
};

// ---------------------------------------------------------------------------
// Playback commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn play(client: State<'_, EngineClient>) -> Result<(), String> {
    client.play().await
}

#[tauri::command]
pub async fn pause(client: State<'_, EngineClient>) -> Result<(), String> {
    client.pause().await
}

#[tauri::command]
pub async fn next(client: State<'_, EngineClient>) -> Result<(), String> {
    client.next().await
}

#[tauri::command]
pub async fn previous(client: State<'_, EngineClient>) -> Result<(), String> {
    client.previous().await
}

#[tauri::command]
pub async fn seek(client: State<'_, EngineClient>, position_ms: u32) -> Result<(), String> {
    client.seek(position_ms).await
}

#[tauri::command]
pub async fn set_volume(client: State<'_, EngineClient>, percent: u8) -> Result<(), String> {
    client.set_volume(percent).await
}

#[tauri::command]
pub async fn set_shuffle(client: State<'_, EngineClient>, enabled: bool) -> Result<(), String> {
    client.set_shuffle(enabled).await
}

#[tauri::command]
pub async fn set_repeat(client: State<'_, EngineClient>, mode: String) -> Result<(), String> {
    client.set_repeat(&mode).await
}

#[tauri::command]
pub async fn play_queue(
    client: State<'_, EngineClient>,
    queue: Vec<Track>,
    index: usize,
) -> Result<(), String> {
    client.play_queue(&queue, index, 0).await
}

#[tauri::command]
pub async fn add_queue(client: State<'_, EngineClient>, track: Track) -> Result<(), String> {
    client.add_queue(&track).await
}

#[tauri::command]
pub async fn remove_queue(client: State<'_, EngineClient>, index: usize) -> Result<(), String> {
    client.remove_queue(index).await
}

#[tauri::command]
pub async fn move_queue(
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
) -> Result<Vec<Playlist>, String> {
    fetch_library(&state, &client, &app).await
}

/// Opens a playlist: serves the disk cache instantly when present and
/// refreshes it in the background; otherwise fetches from the engine.
#[tauri::command]
pub async fn browse_playlist(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    client: State<'_, EngineClient>,
    id: String,
) -> Result<PlaylistDetail, String> {
    let cached = {
        let guard = state.lock().unwrap();
        guard.tracks_cache.iter().find(|entry| entry.id == id).cloned()
    };
    if let Some(entry) = cached {
        let detail = {
            let guard = state.lock().unwrap();
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
    client: State<'_, EngineClient>,
    id: String,
) -> Result<AlbumDetail, String> {
    let detail = AlbumDetail::from(client.browse_album(&id).await?);
    let _ = app.emit("album", &detail);
    Ok(detail)
}

#[tauri::command]
pub async fn browse_artist(
    app: AppHandle,
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
    name: String,
) -> Result<(), String> {
    client.create_playlist(&name).await?;
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn rename_playlist(
    app: AppHandle,
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
    id: String,
) -> Result<(), String> {
    client.delete_playlist(&id).await?;
    {
        let guard = app.state::<Mutex<AppState>>();
        let mut guard = guard.lock().unwrap();
        guard.playlists.retain(|playlist| playlist.id != id);
        guard.tracks_cache.retain(|entry| entry.id != id);
    }
    spawn_refresh_library(app);
    Ok(())
}

#[tauri::command]
pub async fn add_playlist_tracks(
    app: AppHandle,
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
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
    client: State<'_, EngineClient>,
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
pub async fn status(client: State<'_, EngineClient>) -> Result<(), String> {
    client.status().await
}

#[tauri::command]
pub async fn login(client: State<'_, EngineClient>) -> Result<(), String> {
    client.login().await
}

#[tauri::command]
pub async fn logout(client: State<'_, EngineClient>) -> Result<(), String> {
    client.logout().await?;
    client.clear_restore_pending().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// State + covers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_state(state: State<'_, Mutex<AppState>>) -> Result<AppStateSnapshot, String> {
    let guard = state.lock().unwrap();
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

/// Consumes engine state lines, mirrors them into `AppState`, emits the
/// `state`/`session` events, restores playback after an engine respawn, and
/// projects `position_ms` locally between heartbeats.
pub async fn consume_states(app: AppHandle) {
    let client = app.state::<EngineClient>();
    let mut states = client.subscribe_state();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(1000));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // anchor so the first projection tick counts a full second

    let mut last_auth = String::new();
    let mut last_username = String::new();
    let mut library_loaded = false;

    loop {
        tokio::select! {
            state = states.recv() => {
                let Ok(state) = state else { continue };
                let became_ready = state.auth_state == "ready" && last_auth != "ready";
                let session_changed =
                    state.auth_state != last_auth || state.username != last_username;
                last_auth = state.auth_state.clone();
                last_username = state.username.clone();

                // A respawned engine becomes ready with a blank session; put
                // the pre-crash playback back before anything else sees it.
                if let Some(snapshot) = client.take_restore_pending().await {
                    if state.auth_state == "ready" {
                        restore_playback(&client, &snapshot).await;
                    } else {
                        client.put_restore_pending(snapshot).await;
                    }
                }

                {
                    let guard = app.state::<Mutex<AppState>>();
                    let mut guard = guard.lock().unwrap();
                    guard.playback = state.clone();
                    if !state.username.is_empty() {
                        guard.me_id = state.username.clone();
                    }
                }
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
                    if !library_loaded {
                        library_loaded = true;
                        load_library_from_disk(&app);
                    }
                    spawn_refresh_library(app.clone());
                }
            }
            _ = tick.tick() => {
                let guard = app.state::<Mutex<AppState>>();
                let mut guard = guard.lock().unwrap();
                if guard.playback.playing {
                    let projected = guard.playback.position_ms.saturating_add(1000);
                    let duration = guard.playback.duration_ms;
                    guard.playback.position_ms =
                        if duration > 0 { projected.min(duration) } else { projected };
                    let snapshot = guard.playback.clone();
                    drop(guard);
                    let _ = app.emit("state", &snapshot);
                }
            }
        }
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
            eprintln!("SpotifyRenderer: could not restore the queue after engine restart: {error}");
        }
    }
    if let Err(error) = client.set_volume(snapshot.volume).await {
        eprintln!("SpotifyRenderer: could not restore volume: {error}");
    }
    let _ = client.set_shuffle(snapshot.shuffle).await;
    let _ = client.set_repeat(&snapshot.repeat).await;
}

/// Hydrates `AppState` from the on-disk library snapshot (instant first
/// paint), then refreshes from the engine in the background.
fn load_library_from_disk(app: &AppHandle) {
    let dir = data_dir();
    let playlists = load_playlist_list(&dir);
    {
        let guard = app.state::<Mutex<AppState>>();
        let mut guard = guard.lock().unwrap();
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

fn spawn_refresh_library(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<EngineClient>();
        let state = app.state::<Mutex<AppState>>();
        if let Err(error) = fetch_library(&state, &client, &app).await {
            eprintln!("SpotifyRenderer: library refresh failed: {error}");
        }
    });
}

fn spawn_refresh_playlist(app: AppHandle, id: String) {
    tauri::async_runtime::spawn(async move {
        let client = app.state::<EngineClient>();
        let state = app.state::<Mutex<AppState>>();
        match fetch_playlist(&state, &client, &id).await {
            Ok(detail) => {
                let _ = app.emit("playlist-tracks", &detail);
            }
            Err(error) => eprintln!(
                "SpotifyRenderer: background refresh of playlist {id} failed: {error}"
            ),
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
    let playlists: Vec<Playlist> = references.iter().map(Playlist::from).collect();
    let fetched_at = now_secs();
    let (dir, me_id) = {
        let guard = state.lock().unwrap();
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
        let mut guard = state.lock().unwrap();
        guard.playlists = playlists.clone();
        guard.playlists_fetched_at = Some(fetched_at);
    }
    let _ = app.emit("library", &playlists);
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
    let mut guard = state.lock().unwrap();
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
