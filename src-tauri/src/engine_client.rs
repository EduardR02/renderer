//! Line-JSON protocol client for the `SpotifyPlaybackEngine` subprocess.
//!
//! Owns the engine's stdin/stdout pipes, serializes requests with
//! incrementing request ids, routes replies back through tokio oneshots, and
//! fans `state` and `position` lines out on a broadcast channel in wire
//! order. A supervisor task respawns the engine with backoff when its pipe
//! closes and re-requests `status` so the session re-syncs after a restart.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, watch};

use crate::app::{
    PlaybackSnapshot, clear_playback_snapshot, engine_state_dir, load_app_settings,
    load_playback_snapshot, save_playback_snapshot,
};
use crate::log;
use crate::types::{PlaybackState, Track};

/// Replies for playback/session commands arrive promptly; browse and edit
/// commands run network round-trips inside the engine.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Engine respawn backoff bounds.
const RESPAWN_BACKOFF_START: Duration = Duration::from_secs(2);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Roll the engine log once past this size, keeping one previous generation.
const ENGINE_LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Spawns the engine with no console at all (Windows only).
///
/// `CREATE_NO_WINDOW` only hides the console window — Windows still allocates
/// a console and a `conhost.exe` to back it. The engine needs neither: stdin
/// and stdout are pipes and stderr is redirected to the log file, so nothing
/// ever touches a console. `DETACHED_PROCESS` skips the allocation outright,
/// dropping a process and any chance of a window flashing on startup.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// One engine reply (any non-`state`/`position` line). `data` carries the
/// payload of `browse_*`/`edit_*` responses; plain `response` lines leave
/// it `None`.
#[derive(Debug, Clone)]
pub struct EngineReply {
    pub ok: bool,
    pub error: Option<String>,
    pub data: Option<Value>,
}

/// The engine's 2-second scalar position heartbeat. Deliberately not a
/// [`PlaybackState`]: a heartbeat carries only the playhead scalars the
/// frontend projects and clamps against, so parsing and forwarding it never
/// touches (or clones) the queue.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
pub struct PositionHeartbeat {
    pub position_ms: u32,
    pub duration_ms: u32,
}

/// One playback line the engine emitted, fanned out to subscribers in wire
/// order. Heartbeats share the channel with full states so a consumer can
/// never apply an older heartbeat after a newer full state (two channels
/// would race them).
#[derive(Clone, Debug)]
pub enum StateLine {
    State(PlaybackState),
    Position(PositionHeartbeat),
}

/// Queue/settings captured either from the durable app snapshot (normal
/// startup, always paused) or from the last fresh state before an unexpected
/// child exit (resume only when that state was playing).
#[derive(Debug, Clone)]
pub struct RestoreSnapshot {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
    pub playback_speed: f32,
    pub resume_playing: bool,
}

impl RestoreSnapshot {
    fn from_playback(state: &PlaybackState, resume_playing: bool) -> Self {
        Self {
            queue: state.queue.clone(),
            current_index: state.current_index,
            position_ms: state.position_ms,
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: state.repeat.clone(),
            playback_speed: state.playback_speed,
            resume_playing,
        }
        .normalized()
    }

    fn from_durable(snapshot: PlaybackSnapshot) -> Self {
        Self {
            queue: snapshot.queue,
            current_index: snapshot.current_index,
            position_ms: snapshot.position_ms,
            volume: snapshot.volume,
            shuffle: snapshot.shuffle,
            repeat: snapshot.repeat,
            playback_speed: snapshot.playback_speed,
            resume_playing: false,
        }
        .normalized()
    }

    fn normalized(mut self) -> Self {
        let requested = self.current_index.unwrap_or(0);
        self.current_index = if self.queue.is_empty() {
            None
        } else {
            (requested..self.queue.len())
                .chain(0..requested.min(self.queue.len()))
                .find(|index| !self.queue[*index].unavailable)
        };
        if self.current_index != Some(requested) {
            self.position_ms = 0;
        } else if let Some(index) = self.current_index {
            self.position_ms = self.position_ms.min(self.queue[index].duration_ms);
        }
        self
    }

    pub fn matches(&self, state: &PlaybackState) -> bool {
        let position_matches = if self.resume_playing {
            state.position_ms.abs_diff(self.position_ms) <= 2_500
        } else {
            state.position_ms == self.position_ms
        };
        state.auth_state == "ready"
            && state.queue == self.queue
            && state.current_index == self.current_index
            && state.volume == self.volume
            && state.shuffle == self.shuffle
            && state.repeat == self.repeat
            && state.playback_speed == self.playback_speed
            && state.playing == self.resume_playing
            && position_matches
    }

    fn durable(&self) -> PlaybackSnapshot {
        let mut state = PlaybackState::default();
        state.queue = self.queue.clone();
        state.current_index = self.current_index;
        state.position_ms = self.position_ms;
        state.volume = self.volume;
        state.shuffle = self.shuffle;
        state.repeat = self.repeat.clone();
        state.playback_speed = self.playback_speed;
        PlaybackSnapshot::from_playback(&state)
    }
}

#[derive(Debug, Clone)]
struct RestorePlan {
    snapshot: RestoreSnapshot,
    sent: bool,
}

pub struct EngineClient {
    state_dir: PathBuf,
    pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<EngineReply>>>,
    stdin: Mutex<Option<ChildStdin>>,
    process: Mutex<Option<Child>>,
    state_tx: tokio::sync::broadcast::Sender<StateLine>,
    exit_tx: watch::Sender<bool>,
    last_state: Mutex<Option<PlaybackState>>,
    restore_pending: Mutex<Option<RestorePlan>>,
    next_request_id: AtomicU64,
    shutting_down: AtomicBool,
}

impl EngineClient {
    /// Creates a client and spawns the engine subprocess. The supervisor task
    /// (see [`EngineClient::supervise`]) takes over respawns from there.
    pub fn start() -> Arc<Self> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = watch::channel(false);
        let restore_pending = load_playback_snapshot().map(|snapshot| RestorePlan {
            snapshot: RestoreSnapshot::from_durable(snapshot),
            sent: false,
        });
        let client = Arc::new(Self {
            state_dir: engine_state_dir(),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(None),
            restore_pending: Mutex::new(restore_pending),
            next_request_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        });
        if let Err(error) = client.spawn_engine() {
            log::error(&format!("engine spawn failed at startup: {error}"));
        }
        client
    }

    /// Subscribes to the engine's playback lines (full `state` and scalar
    /// `position` heartbeats) in wire order.
    pub fn subscribe_lines(&self) -> tokio::sync::broadcast::Receiver<StateLine> {
        self.state_tx.subscribe()
    }

    /// Starts the pending restore exactly once, after a fresh engine reports
    /// that authentication is ready. The plan remains stored until the engine
    /// emits the matching final state.
    pub fn begin_pending_restore(&self, state: &PlaybackState) -> Option<RestoreSnapshot> {
        let mut pending = self.restore_pending.lock();
        let plan = pending.as_mut()?;
        if plan.sent || state.auth_state != "ready" {
            return None;
        }
        plan.sent = true;
        Some(plan.snapshot.clone())
    }

    pub fn restore_is_pending(&self) -> bool {
        self.restore_pending.lock().is_some()
    }

    /// Drops any pending restore (used after an explicit logout).
    pub fn clear_restore_pending(&self) {
        *self.restore_pending.lock() = None;
    }

    pub fn retry_pending_restore(&self) {
        if let Some(plan) = self.restore_pending.lock().as_mut() {
            plan.sent = false;
        }
    }

    /// Respawn loop: waits for the reader thread to signal EOF, sleeps with
    /// exponential backoff, then starts a fresh engine and re-requests
    /// `status` so the engine re-emits its current state.
    pub async fn supervise(self: Arc<Self>) {
        let mut backoff = RESPAWN_BACKOFF_START;
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            if self.process.lock().is_none() {
                // Startup spawn failed or the engine died; (re)start it.
                match self.spawn_engine() {
                    Ok(()) => {
                        backoff = RESPAWN_BACKOFF_START;
                        // Clear the exit latch for the fresh process.
                        let _ = self.exit_tx.send(false);
                    }
                    Err(error) => {
                        log::error(&format!(
                            "engine spawn failed ({error}); retrying in {}s",
                            backoff.as_secs()
                        ));
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
                        continue;
                    }
                }
                // Re-sync the fresh engine's session ("re-send status").
                match self.request("status", Value::Null).await {
                    Ok(_) => log::info("engine respawned; status re-requested"),
                    Err(error) => log::warn(&format!(
                        "engine respawned but status re-request failed: {error}"
                    )),
                }
            }
            let mut exited = self.exit_tx.subscribe();
            if self.process.lock().is_none() {
                // Died between the spawn and the subscribe (or a stale latch
                // raced the reset); loop around to respawn immediately.
                continue;
            }
            while !*exited.borrow() {
                if exited.changed().await.is_err() {
                    return; // sender dropped: client is gone
                }
            }
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            log::warn(&format!(
                "engine exited; respawning in {}s",
                backoff.as_secs()
            ));
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RESPAWN_BACKOFF_MAX);
        }
    }

    /// Spawns the engine process and its reader thread. Only called when no
    /// process is currently running.
    fn spawn_engine(self: &Arc<Self>) -> Result<(), String> {
        let exe = locate_engine().ok_or_else(|| {
            "SpotifyPlaybackEngine.exe not found (set SPOTIFY_ENGINE_PATH)".to_owned()
        })?;
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|error| format!("could not create engine state dir: {error}"))?;
        // The engine's diagnostics live in the same file the panic hook
        // appends to: env_logger stderr (apresolve/connect attempts and
        // failures at info level) plus panic reports via --log-file.
        let logs_dir = crate::app::logs_dir();
        let _ = std::fs::create_dir_all(&logs_dir);
        let engine_log = logs_dir.join("playback_engine.log");
        rotate_if_large(&engine_log);
        let mut command = Command::new(&exe);
        let audio_cache_limit_mb = load_app_settings().audio_cache_limit_mb;
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--log-file")
            .arg(&engine_log)
            .arg("--audio-cache-limit-mb")
            .arg(audio_cache_limit_mb.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match OpenOptions::new().create(true).append(true).open(&engine_log) {
                Ok(file) => Stdio::from(file),
                Err(_) => Stdio::null(),
            })
            // Info level surfaces librespot's "Connecting to AP ..." lines
            // and apresolve failures; the default warn level only shows
            // failures, which hides whether the engine is even trying.
            // Symphonia is pinned to error: it warns once per frame gap
            // ("skipping junk at N bytes"), thousands of lines per track,
            // which is continuous disk I/O for no diagnostic value.
            .env(
                "RUST_LOG",
                "info,symphonia_bundle_mp3=error,symphonia_core=error",
            );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(DETACHED_PROCESS);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not spawn {exe:?}: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "engine stdout is unavailable".to_owned())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "engine stdin is unavailable".to_owned())?;
        let pid = child.id();
        *self.stdin.lock() = Some(stdin);
        *self.process.lock() = Some(child);
        spawn_reader(stdout, self);
        log::info(&format!(
            "engine spawned (pid {pid}); log={}",
            engine_log.display()
        ));
        Ok(())
    }

    fn next_request_id(&self) -> String {
        format!(
            "request-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Writes one request line and awaits its reply.
    pub async fn request(&self, kind: &str, args: Value) -> Result<EngineReply, String> {
        let request_id = self.next_request_id();
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(request_id.clone(), sender);
        }
        let line = build_line(&request_id, kind, args);
        if let Err(error) = self.write_line(&line) {
            let mut pending = self.pending.lock().await;
            pending.remove(&request_id);
            return Err(error);
        }
        let timeout = if kind.starts_with("browse_")
            || kind.starts_with("edit_")
            || kind == "extract_track_waveform"
        {
            BROWSE_TIMEOUT
        } else {
            COMMAND_TIMEOUT
        };
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(reply)) if reply.ok => Ok(reply),
            Ok(Ok(reply)) => Err(reply
                .error
                .unwrap_or_else(|| "engine rejected the request".to_owned())),
            Ok(Err(_)) => Err("engine request was dropped".to_owned()),
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&request_id);
                Err(format!(
                    "engine did not answer {kind} within {}s",
                    timeout.as_secs()
                ))
            }
        }
    }

    fn write_line(&self, line: &str) -> Result<(), String> {
        let mut stdin = self.stdin.lock();
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| "the playback engine is not running".to_owned())?;
        pipe.write_all(line.as_bytes())
            .and_then(|_| pipe.write_all(b"\n"))
            .and_then(|_| pipe.flush())
            .map_err(|error| format!("could not write to the playback engine: {error}"))
    }

    // ------------------------------------------------------------------
    // Typed requests
    // ------------------------------------------------------------------

    pub async fn status(&self) -> Result<(), String> {
        self.request("status", Value::Null).await.map(|_| ())
    }

    pub async fn play(&self) -> Result<(), String> {
        self.request("play", Value::Null).await.map(|_| ())
    }

    pub async fn pause(&self) -> Result<(), String> {
        self.request("pause", Value::Null).await.map(|_| ())
    }

    pub async fn next(&self) -> Result<(), String> {
        self.request("next", Value::Null).await.map(|_| ())
    }

    pub async fn previous(&self) -> Result<(), String> {
        self.request("previous", Value::Null).await.map(|_| ())
    }

    pub async fn seek(&self, position_ms: u32) -> Result<(), String> {
        self.request("seek", json!({"position_ms": position_ms}))
            .await
            .map(|_| ())
    }

    pub async fn set_volume(&self, percent: u8) -> Result<(), String> {
        self.request("set_volume", json!({"percent": percent}))
            .await
            .map(|_| ())
    }

    pub async fn set_shuffle(&self, enabled: bool) -> Result<(), String> {
        self.request("set_shuffle", json!({"enabled": enabled}))
            .await
            .map(|_| ())
    }

    pub async fn set_repeat(&self, mode: &str) -> Result<(), String> {
        if !matches!(mode, "off" | "context" | "track") {
            return Err(format!(
                "invalid repeat mode {mode:?} (expected off, context or track)"
            ));
        }
        self.request("set_repeat", json!({"mode": mode}))
            .await
            .map(|_| ())
    }

    pub async fn set_playback_speed(&self, speed: f32) -> Result<(), String> {
        if !speed.is_finite() || !(0.5..=2.0).contains(&speed) {
            return Err("playback speed must be between 0.5 and 2.0".to_owned());
        }
        self.request("set_playback_speed", json!({"speed": speed}))
            .await
            .map(|_| ())
    }

    pub async fn play_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
        context: &str,
    ) -> Result<(), String> {
        self.request(
            "play_queue",
            json!({
                "queue": queue,
                "index": index,
                "position_ms": position_ms,
                "context": context,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn restore_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
        context: &str,
    ) -> Result<(), String> {
        self.request(
            "restore_queue",
            json!({
                "queue": queue,
                "index": index,
                "position_ms": position_ms,
                "context": context,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn play_queue_index(&self, index: usize) -> Result<(), String> {
        self.request("play_queue_index", json!({"index": index}))
            .await
            .map(|_| ())
    }

    pub async fn add_queue(&self, track: &Track, context: &str) -> Result<(), String> {
        self.request("add_queue", json!({"track": track, "context": context}))
            .await
            .map(|_| ())
    }

    pub async fn add_queue_batch(
        &self,
        tracks: &[Track],
        context: &str,
    ) -> Result<(), String> {
        self.request(
            "add_queue_batch",
            json!({"tracks": tracks, "context": context}),
        )
        .await
        .map(|_| ())
    }

    pub async fn remove_queue(&self, index: usize) -> Result<(), String> {
        self.request("remove_queue", json!({"index": index}))
            .await
            .map(|_| ())
    }

    pub async fn move_queue(&self, from: usize, to: usize) -> Result<(), String> {
        self.request("move_queue", json!({"from": from, "to": to}))
            .await
            .map(|_| ())
    }


    pub async fn get_history(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<spotify_playback_engine::protocol::HistoryPage, String> {
        let reply = self
            .request("get_history", json!({"offset": offset, "limit": limit}))
            .await?;
        parse_data(reply, "get_history")
    }

    pub async fn clear_history(&self) -> Result<(), String> {
        self.request("clear_history", Value::Null).await.map(|_| ())
    }
    pub async fn login(&self) -> Result<(), String> {
        self.request("login", Value::Null).await.map(|_| ())
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.request("logout", Value::Null).await?;
        self.clear_restore_pending();
        *self.last_state.lock() = None;
        clear_playback_snapshot()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Browse / edit requests (typed payloads from `data`)
    // ------------------------------------------------------------------

    pub async fn browse_playlists(
        &self,
        length: usize,
    ) -> Result<Vec<spotify_playback_engine::protocol::PlaylistRef>, String> {
        let reply = self.request("browse_playlists", json!({"length": length})).await?;
        parse_data(reply, "browse_playlists")
    }

    pub async fn browse_playlist(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::PlaylistBrowse, String> {
        let reply = self.request("browse_playlist", json!({"id": id})).await?;
        parse_data(reply, "browse_playlist")
    }

    pub async fn browse_radio(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::RadioBrowse, String> {
        let reply = self.request("browse_radio", json!({"id": id})).await?;
        parse_data(reply, "browse_radio")
    }

    pub async fn browse_playlist_recommendations(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::PlaylistRecommendations, String> {
        let reply = self
            .request("browse_playlist_recommendations", json!({"id": id}))
            .await?;
        parse_data(reply, "browse_playlist_recommendations")
    }

    pub async fn browse_album(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::AlbumBrowse, String> {
        let reply = self.request("browse_album", json!({"id": id})).await?;
        parse_data(reply, "browse_album")
    }

    pub async fn browse_artist(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::ArtistBrowse, String> {
        let reply = self.request("browse_artist", json!({"id": id})).await?;
        parse_data(reply, "browse_artist")
    }

    pub async fn browse_artist_releases(
        &self,
        id: &str,
        release_types: &[String],
        offset: usize,
        limit: usize,
    ) -> Result<spotify_playback_engine::protocol::ArtistReleasePage, String> {
        let reply = self
            .request(
                "browse_artist_releases",
                json!({"id": id, "release_types": release_types, "offset": offset, "limit": limit}),
            )
            .await?;
        parse_data(reply, "browse_artist_releases")
    }

    pub async fn browse_artist_catalogue(
        &self,
        id: &str,
        release_types: &[String],
        offset: usize,
        limit: usize,
    ) -> Result<spotify_playback_engine::protocol::ArtistCataloguePage, String> {
        let reply = self
            .request(
                "browse_artist_catalogue",
                json!({"id": id, "release_types": release_types, "offset": offset, "limit": limit}),
            )
            .await?;
        parse_data(reply, "browse_artist_catalogue")
    }

    pub async fn browse_liked_songs(
        &self,
        cursor: Option<&str>,
    ) -> Result<spotify_playback_engine::protocol::LikedSongsPage, String> {
        let reply = self
            .request("browse_liked_songs", json!({"cursor": cursor}))
            .await?;
        parse_data(reply, "browse_liked_songs")
    }

    pub async fn browse_track_credits(
        &self,
        id: &str,
    ) -> Result<spotify_playback_engine::protocol::TrackCredits, String> {
        let reply = self
            .request("browse_track_credits", json!({"id": id}))
            .await?;
        parse_data(reply, "browse_track_credits")
    }

    pub async fn browse_canvas(
        &self,
        id: &str,
    ) -> Result<Option<spotify_playback_engine::protocol::Canvas>, String> {
        let reply = self.request("browse_canvas", json!({"id": id})).await?;
        parse_data(reply, "browse_canvas")
    }

    pub async fn track_edit_status(
        &self,
        track_id: &str,
        playlist_id: Option<&str>,
    ) -> Result<spotify_playback_engine::protocol::TrackEditStatus, String> {
        let reply = self
            .request(
                "get_track_edit",
                json!({"track_id": track_id, "playlist_id": playlist_id}),
            )
            .await?;
        parse_data(reply, "get_track_edit")
    }

    pub async fn save_track_edit(
        &self,
        track_id: &str,
        duration_ms: u32,
        cuts: &[spotify_playback_engine::protocol::TimeRange],
        loop_range: Option<spotify_playback_engine::protocol::TimeRange>,
    ) -> Result<spotify_playback_engine::protocol::TrackEditDefinition, String> {
        let reply = self
            .request(
                "save_track_edit",
                json!({
                    "track_id": track_id,
                    "duration_ms": duration_ms,
                    "cuts": cuts,
                    "loop_range": loop_range,
                }),
            )
            .await?;
        parse_data(reply, "save_track_edit")
    }

    pub async fn delete_track_edit(&self, track_id: &str) -> Result<(), String> {
        self.request("delete_track_edit", json!({"track_id": track_id}))
            .await
            .map(|_| ())
    }

    pub async fn set_playlist_track_edit_enabled(
        &self,
        playlist_id: &str,
        track_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.request(
            "set_playlist_track_edit_enabled",
            json!({
                "playlist_id": playlist_id,
                "track_id": track_id,
                "enabled": enabled,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn extract_track_waveform(
        &self,
        track_id: &str,
        points: u16,
    ) -> Result<spotify_playback_engine::protocol::TrackWaveform, String> {
        let reply = self
            .request(
                "extract_track_waveform",
                json!({"track_id": track_id, "points": points}),
            )
            .await?;
        parse_data(reply, "extract_track_waveform")
    }

    pub async fn browse_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<spotify_playback_engine::protocol::SearchBrowse, String> {
        let reply = self
            .request("browse_search", json!({"query": query, "limit": limit}))
            .await?;
        parse_data(reply, "browse_search")
    }

    pub async fn create_playlist(
        &self,
        name: &str,
    ) -> Result<spotify_playback_engine::protocol::PlaylistRef, String> {
        let reply = self
            .request("edit_create_playlist", json!({"name": name}))
            .await?;
        parse_data(reply, "edit_create_playlist")
    }

    pub async fn rename_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let _ = self
            .request("edit_rename_playlist", json!({"id": id, "name": name}))
            .await?;
        Ok(())
    }

    pub async fn delete_playlist(&self, id: &str) -> Result<(), String> {
        let _ = self.request("edit_delete_playlist", json!({"id": id})).await?;
        Ok(())
    }

    pub async fn add_playlist_tracks(&self, id: &str, uris: &[String]) -> Result<(), String> {
        let _ = self
            .request("edit_add_playlist_tracks", json!({"id": id, "uris": uris}))
            .await?;
        Ok(())
    }

    pub async fn remove_playlist_tracks(&self, id: &str, uris: &[String]) -> Result<(), String> {
        let _ = self
            .request("edit_remove_playlist_tracks", json!({"id": id, "uris": uris}))
            .await?;
        Ok(())
    }

    pub async fn reorder_playlist_tracks(
        &self,
        id: &str,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        let _ = self
            .request(
                "edit_reorder_playlist_tracks",
                json!({"id": id, "from": from, "to": to}),
            )
            .await?;
        Ok(())
    }

    /// Persists one heartbeat-freshened snapshot, then gracefully stops the
    /// engine. No playback mutation performs recurring full-queue disk writes.
    pub fn shutdown_engine(&self) {
        log::info("engine shutdown requested");
        if let Err(error) = self.persist_playback_state() {
            log::warn(&format!("could not persist playback state: {error}"));
        }
        self.shutting_down.store(true, Ordering::SeqCst);
        let line = build_line(&self.next_request_id(), "shutdown", Value::Null);
        let _ = self.write_line(&line);
        std::thread::sleep(Duration::from_millis(400));
        if let Some(mut child) = self.process.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *self.stdin.lock() = None;
    }

    fn persist_playback_state(&self) -> Result<(), String> {
        let snapshot = self
            .last_state
            .lock()
            .as_ref()
            .filter(|state| state.auth_state == "ready")
            .map(PlaybackSnapshot::from_playback)
            .or_else(|| {
                self.restore_pending
                    .lock()
                    .as_ref()
                    .map(|plan| plan.snapshot.durable())
            });
        match snapshot {
            Some(snapshot) => save_playback_snapshot(&snapshot),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------------
    // Reader-thread callbacks
    // ------------------------------------------------------------------

    fn on_state(&self, state: &PlaybackState) {
        let mut pending = self.restore_pending.lock();
        let matched = pending
            .as_ref()
            .is_some_and(|plan| plan.sent && plan.snapshot.matches(state));
        if matched {
            *pending = None;
        }
        let suppress_snapshot = pending.is_some() && state.auth_state == "ready";
        drop(pending);
        if !suppress_snapshot {
            *self.last_state.lock() = Some(state.clone());
        }
        let _ = self.state_tx.send(StateLine::State(state.clone()));
    }

    /// Applies a scalar position heartbeat to the heartbeat-fresh last state.
    fn on_position(&self, heartbeat: PositionHeartbeat) {
        if let Some(last) = self.last_state.lock().as_mut() {
            last.position_ms = heartbeat.position_ms;
            last.duration_ms = heartbeat.duration_ms;
        }
        let _ = self.state_tx.send(StateLine::Position(heartbeat));
    }

    fn on_eof(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        log::warn("engine process exited (stdout closed)");
        let mut pending = self.pending.blocking_lock();
        for (_, sender) in pending.drain() {
            let _ = sender.send(EngineReply {
                ok: false,
                error: Some("the playback engine exited".to_owned()),
                data: None,
            });
        }
        drop(pending);
        let last = self.last_state.lock().clone();
        let mut restore = self.restore_pending.lock();
        if let Some(last) = last.filter(|state| state.auth_state == "ready") {
            *restore = Some(RestorePlan {
                snapshot: RestoreSnapshot::from_playback(&last, last.playing),
                sent: false,
            });
        } else if let Some(plan) = restore.as_mut() {
            plan.sent = false;
        }
        *self.stdin.lock() = None;
        *self.process.lock() = None;
        let _ = self.exit_tx.send(true);
    }

    fn deliver(&self, request_id: &str, reply: EngineReply) {
        let mut pending = self.pending.blocking_lock();
        if let Some(sender) = pending.remove(request_id) {
            let _ = sender.send(reply);
        }
    }
}

/// Reader thread: parses one protocol line per iteration. `state` and
/// `position` lines are fanned out to subscribers in wire order; every other
/// line is routed to the pending request with the matching id.
fn spawn_reader(stdout: ChildStdout, client: &Arc<EngineClient>) {
    let client = Arc::clone(client);
    std::thread::Builder::new()
        .name("engine-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let value: Value = match serde_json::from_str(trimmed) {
                            Ok(value) => value,
                            // Never produced by the engine; be tolerant.
                            Err(_) => continue,
                        };
                        match parse_line(value) {
                            Some(Line::State(state)) => client.on_state(&state),
                            Some(Line::Position(heartbeat)) => client.on_position(heartbeat),
                            Some(Line::Reply {
                                request_id,
                                ok,
                                error,
                                data,
                            }) => {
                                client.deliver(&request_id, EngineReply { ok, error, data });
                            }
                            None => {} // unparseable line: logged, dropped
                        }
                    }
                    Err(_) => break,
                }
            }
            client.on_eof();
        })
        .expect("could not start engine reader thread");
}

/// What one engine output line means. `state` and `position` lines are
/// fanned out; every other line is a reply to a pending request.
#[derive(Debug)]
enum Line {
    State(PlaybackState),
    Position(PositionHeartbeat),
    Reply {
        request_id: String,
        ok: bool,
        error: Option<String>,
        data: Option<Value>,
    },
}

/// Classifies and parses one protocol line. Position heartbeats are parsed
/// into their two scalars only — never into a [`PlaybackState`], so a
/// heartbeat can never cost a queue parse. Malformed `state`/`position`
/// lines are logged and dropped (`None`), matching the reader's old
/// tolerance; unknown lines fall through to reply routing.
fn parse_line(value: Value) -> Option<Line> {
    match value.get("type").and_then(Value::as_str) {
        Some("state") => match serde_json::from_value::<PlaybackState>(value) {
            Ok(state) => Some(Line::State(state)),
            Err(error) => {
                log::error(&format!("could not parse engine state line: {error}"));
                None
            }
        },
        Some("position") => match serde_json::from_value::<PositionHeartbeat>(value) {
            Ok(heartbeat) => Some(Line::Position(heartbeat)),
            Err(error) => {
                log::error(&format!("could not parse engine position line: {error}"));
                None
            }
        },
        _ => {
            let Value::Object(mut object) = value else {
                return Some(Line::Reply {
                    request_id: String::new(),
                    ok: false,
                    error: None,
                    data: None,
                });
            };
            let request_id = match object.remove("request_id") {
                Some(Value::String(request_id)) => request_id,
                _ => String::new(),
            };
            let ok = matches!(object.remove("ok"), Some(Value::Bool(true)));
            let error = match object.remove("error") {
                Some(Value::String(error)) => Some(error),
                _ => None,
            };
            let data = object.remove("data");
            Some(Line::Reply {
                request_id,
                ok,
                error,
                data,
            })
        }
    }
}

/// Rolls the engine log to `<name>.log.1` once it grows past
/// [`ENGINE_LOG_MAX_BYTES`], discarding the previous generation. Best-effort:
/// if anything fails the current file simply keeps growing.
fn rotate_if_large(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() < ENGINE_LOG_MAX_BYTES {
        return;
    }
    let previous = path.with_extension("log.1");
    let _ = std::fs::remove_file(&previous);
    if std::fs::rename(path, &previous).is_ok() {
        log::info(&format!(
            "engine log rolled at {} bytes -> {}",
            metadata.len(),
            previous.display()
        ));
    }
}

fn build_line(request_id: &str, kind: &str, args: Value) -> String {
    let mut object = Map::new();
    object.insert("request_id".to_owned(), Value::String(request_id.to_owned()));
    object.insert("type".to_owned(), Value::String(kind.to_owned()));
    if let Value::Object(fields) = args {
        object.extend(fields);
    }
    serde_json::to_string(&Value::Object(object)).expect("request serialization cannot fail")
}

fn parse_data<T: serde::de::DeserializeOwned>(
    reply: EngineReply,
    kind: &str,
) -> Result<T, String> {
    let data = reply
        .data
        .ok_or_else(|| format!("{kind} reply carried no payload"))?;
    serde_json::from_value(data).map_err(|error| format!("unexpected {kind} payload: {error}"))
}

/// Locates the engine executable: `SPOTIFY_ENGINE_PATH`, a sibling of the
/// app executable, or the repository's release build.
fn locate_engine() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTIFY_ENGINE_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Bundled next to the app executable.
    let sibling = exe_dir.join("SpotifyPlaybackEngine.exe");
    if sibling.is_file() {
        return Some(sibling);
    }
    // Repository layout: <repo>/engine/target/release/SpotifyPlaybackEngine.exe.
    for depth in 1..=3 {
        let mut candidate = exe_dir.clone();
        for _ in 0..depth {
            candidate.push("..");
        }
        candidate = candidate
            .join("engine")
            .join("target")
            .join("release")
            .join("SpotifyPlaybackEngine.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `EngineClient` with no live engine process, for unit tests of the
    /// reader-side callbacks.
    fn client_with_last_state(state: PlaybackState) -> Arc<EngineClient> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = tokio::sync::watch::channel(false);
        Arc::new(EngineClient {
            state_dir: PathBuf::new(),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(Some(state)),
            restore_pending: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        })
    }

    #[test]
    fn request_lines_keep_protocol_and_payload_fields() {
        let line = build_line(
            "request-7",
            "play_queue",
            serde_json::json!({
                "queue": [{"uri": "spotify:track:abc"}],
                "index": 0,
                "position_ms": 12,
            }),
        );
        let value: Value = serde_json::from_str(&line).expect("request line is JSON");
        assert_eq!(value["request_id"], "request-7");
        assert_eq!(value["type"], "play_queue");
        assert_eq!(value["queue"][0]["uri"], "spotify:track:abc");
        assert_eq!(value["index"], 0);
        assert_eq!(value["position_ms"], 12);
    }

    #[test]
    fn reply_lines_keep_error_and_data_fields() {
        let reply = parse_line(serde_json::json!({
            "type": "browse_playlist",
            "request_id": "request-8",
            "ok": false,
            "error": "browse failed",
            "data": {"tracks": [{"id": "track-1"}]},
        }));
        match reply {
            Some(Line::Reply {
                request_id,
                ok,
                error,
                data,
            }) => {
                assert_eq!(request_id, "request-8");
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("browse failed"));
                assert_eq!(data.unwrap()["tracks"][0]["id"], "track-1");
            }
            _ => panic!("reply fields stay on the reply path"),
        }
    }

    #[test]
    fn position_lines_parse_as_scalar_heartbeats_not_states() {
        let line = parse_line(serde_json::json!({
            "type": "position",
            "position_ms": 12_345,
            "duration_ms": 240_000,
        }));
        let Line::Position(heartbeat) = line.expect("position line parses") else {
            panic!("expected a position heartbeat");
        };
        assert_eq!(heartbeat.position_ms, 12_345);
        assert_eq!(heartbeat.duration_ms, 240_000);
    }

    #[test]
    fn state_and_reply_lines_still_classify_as_before() {
        let state = parse_line(serde_json::json!({
            "type": "state",
            "ready": true,
            "auth_state": "ready",
            "playing": true,
            "position_ms": 1,
            "duration_ms": 2,
            "volume": 50,
            "shuffle": false,
            "repeat": "off",
            "queue": [],
        }));
        assert!(matches!(state, Some(Line::State(_))));

        let reply = parse_line(serde_json::json!({
            "type": "response",
            "request_id": "request-7",
            "ok": true,
        }));
        match reply {
            Some(Line::Reply { request_id, ok, error, data }) => {
                assert_eq!(request_id, "request-7");
                assert!(ok);
                assert!(error.is_none());
                assert!(data.is_none());
            }
            _ => panic!("response lines stay on the reply path"),
        }
    }

    #[test]
    fn position_heartbeats_freshen_the_last_state_scalars_in_place() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        state.queue = vec![Track::default(); 3];
        let client = client_with_last_state(state);
        let queue_ptr = client.last_state.lock().as_ref().unwrap().queue.as_ptr();

        // Subscribe before the send: broadcast messages sent with no active
        // receiver are dropped, exactly like the production flow where
        // consume_states subscribes before the engine produces lines.
        let mut receiver = client.subscribe_lines();
        client.on_position(PositionHeartbeat {
            position_ms: 42_000,
            duration_ms: 240_000,
        });

        let last = client.last_state.lock();
        let last = last.as_ref().expect("last state kept");
        assert_eq!(last.position_ms, 42_000);
        assert_eq!(last.duration_ms, 240_000);
        assert_eq!(
            last.queue.as_ptr(),
            queue_ptr,
            "a heartbeat must never clone the queue"
        );
        assert_eq!(last.queue.len(), 3, "queue contents untouched");

        // Subscribers see the heartbeat as a Position line, in wire order.
        match receiver.try_recv().expect("heartbeat fanned out") {
            StateLine::Position(heartbeat) => {
                assert_eq!(heartbeat.position_ms, 42_000);
                assert_eq!(heartbeat.duration_ms, 240_000);
            }
            StateLine::State(_) => panic!("heartbeat must not arrive as a full state"),
        }
    }

    #[test]
    fn blank_ready_state_is_suppressed_until_restored_state_matches() {
        let mut previous = PlaybackState::default();
        previous.auth_state = "ready".to_owned();
        previous.queue = vec![Track {
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }];
        previous.current_index = Some(0);
        previous.position_ms = 42_000;
        previous.volume = 37;
        previous.shuffle = true;
        previous.repeat = "context".to_owned();
        let client = client_with_last_state(previous.clone());
        *client.restore_pending.lock() = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&previous, false),
            sent: false,
        });

        let mut blank = PlaybackState::default();
        blank.ready = true;
        blank.auth_state = "ready".to_owned();
        client.on_state(&blank);
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().queue,
            previous.queue,
            "fresh-child blank state must not erase the crash snapshot"
        );
        let restore = client
            .begin_pending_restore(&blank)
            .expect("ready state starts restore");
        assert!(!restore.resume_playing);
        assert!(client.begin_pending_restore(&blank).is_none());

        let mut restored = previous;
        restored.playing = false;
        client.on_state(&restored);
        assert!(!client.restore_is_pending());
        assert_eq!(
            client.last_state.lock().as_ref().unwrap().position_ms,
            42_000
        );
    }

    #[test]
    fn crash_snapshot_resumes_only_when_the_previous_state_was_playing() {
        let mut state = PlaybackState::default();
        state.auth_state = "ready".to_owned();
        state.queue = vec![Track {
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            ..Track::default()
        }];
        state.current_index = Some(0);
        state.playing = true;
        assert!(RestoreSnapshot::from_playback(&state, state.playing).resume_playing);
        state.playing = false;
        assert!(!RestoreSnapshot::from_playback(&state, state.playing).resume_playing);
    }

    /// Finds a built engine binary: `SPOTIFY_ENGINE_PATH`, the workspace
    /// target dir, or the engine package's own target dir.
    fn find_engine() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("SPOTIFY_ENGINE_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        for candidate in [
            root.join("target").join("debug").join("SpotifyPlaybackEngine.exe"),
            root.join("target").join("release").join("SpotifyPlaybackEngine.exe"),
            root.join("engine").join("target").join("debug").join("SpotifyPlaybackEngine.exe"),
            root.join("engine").join("target").join("release").join("SpotifyPlaybackEngine.exe"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// End-to-end wire test against the real engine: spawn, initial state
    /// line, a `status` round-trip, the unauthenticated browse error path,
    /// and a graceful shutdown.
    #[tokio::test]
    async fn engine_client_round_trips_over_the_line_protocol() {
        let Some(exe) = find_engine() else {
            eprintln!(
                "skipping: SpotifyPlaybackEngine.exe is not built \
                 (run `cargo build -p spotify-playback-engine`)"
            );
            return;
        };
        let state_dir = std::env::temp_dir().join(format!(
            "spotify-renderer-engine-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).unwrap();
        std::env::set_var("SPOTIFY_ENGINE_PATH", &exe);
        std::env::set_var("SPOTIFY_STATE_DIR", &state_dir);

        let client = EngineClient::start();
        let mut lines = client.subscribe_lines();

        // `start` launches the child synchronously, so a fast engine can
        // publish its first broadcast before this receiver exists. Production
        // performs the same post-subscription status sync during setup; do it
        // explicitly here to make the wire test deterministic.
        client.status().await.expect("initial status command round-trips");

        // The engine announces its session in response to the status sync.
        let first = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("engine emits its initial state within 20s")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("the initial line is a full state, not a heartbeat"),
        };
        assert!(!first.auth_state.is_empty());
        assert_eq!(first.auth_state, "needs_login", "fresh state dir has no session");

        // A command round-trip: status is answered and re-emits the state.
        client.status().await.expect("status command round-trips");
        let after_status = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("status triggers a fresh state line")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("status re-emits a full state"),
        };
        assert_eq!(after_status.auth_state, first.auth_state);

        // Browse without a session fails cleanly through the reply channel.
        let error = client.browse_playlists(10).await;
        assert!(error.is_err(), "browse without a session must fail cleanly");

        client.shutdown_engine();
        let _ = std::fs::remove_dir_all(&state_dir);
    }
}
