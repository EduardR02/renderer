//! Line-JSON protocol client for the `SpotifyPlaybackEngine` subprocess.
//!
//! Owns the engine's stdin/stdout pipes, serializes requests with
//! incrementing request ids, routes replies back through tokio oneshots, and
//! fans `state` lines out on a broadcast channel. A supervisor task respawns
//! the engine with backoff when its pipe closes and re-requests `status` so
//! the session re-syncs after a restart.

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

use crate::app::engine_state_dir;
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

/// One engine reply (any non-`state` line). `data` carries the payload of
/// `browse_*`/`edit_*` responses; plain `response` lines leave it `None`.
#[derive(Debug, Clone)]
pub struct EngineReply {
    pub ok: bool,
    pub error: Option<String>,
    pub data: Option<Value>,
}

/// Playback settings to re-apply after the engine is respawned
/// (`RestorePlaybackAfterRespawn`).
#[derive(Debug, Clone)]
pub struct RestoreSnapshot {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
}

impl From<&PlaybackState> for RestoreSnapshot {
    fn from(state: &PlaybackState) -> Self {
        Self {
            queue: state.queue.clone(),
            current_index: state.current_index,
            position_ms: state.position_ms,
            volume: state.volume,
            shuffle: state.shuffle,
            repeat: state.repeat.clone(),
        }
    }
}

pub struct EngineClient {
    state_dir: PathBuf,
    pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<EngineReply>>>,
    stdin: Mutex<Option<ChildStdin>>,
    process: Mutex<Option<Child>>,
    state_tx: tokio::sync::broadcast::Sender<PlaybackState>,
    exit_tx: watch::Sender<bool>,
    last_state: Mutex<Option<PlaybackState>>,
    restore_pending: tokio::sync::Mutex<Option<RestoreSnapshot>>,
    next_request_id: AtomicU64,
    shutting_down: AtomicBool,
}

impl EngineClient {
    /// Creates a client and spawns the engine subprocess. The supervisor task
    /// (see [`EngineClient::supervise`]) takes over respawns from there.
    pub fn start() -> Arc<Self> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = watch::channel(false);
        let client = Arc::new(Self {
            state_dir: engine_state_dir(),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(None),
            restore_pending: tokio::sync::Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        });
        if let Err(error) = client.spawn_engine() {
            log::error(&format!("engine spawn failed at startup: {error}"));
        }
        client
    }

    pub fn subscribe_state(&self) -> tokio::sync::broadcast::Receiver<PlaybackState> {
        self.state_tx.subscribe()
    }

    /// Takes the restore snapshot captured at the last engine death, if any.
    pub async fn take_restore_pending(&self) -> Option<RestoreSnapshot> {
        self.restore_pending.lock().await.take()
    }

    /// Puts a restore snapshot back (the fresh engine is not ready yet).
    pub async fn put_restore_pending(&self, snapshot: RestoreSnapshot) {
        *self.restore_pending.lock().await = Some(snapshot);
    }

    /// Drops any pending restore (used after an explicit logout).
    pub async fn clear_restore_pending(&self) {
        *self.restore_pending.lock().await = None;
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
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--log-file")
            .arg(&engine_log)
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
        let line = build_line(&request_id, kind, &args);
        if let Err(error) = self.write_line(&line) {
            let mut pending = self.pending.lock().await;
            pending.remove(&request_id);
            return Err(error);
        }
        let timeout = if kind.starts_with("browse_") || kind.starts_with("edit_") {
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

    pub async fn play_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
    ) -> Result<(), String> {
        let queue: Vec<Value> = queue
            .iter()
            .map(|track| serde_json::to_value(track).unwrap_or(Value::Null))
            .collect();
        self.request(
            "play_queue",
            json!({"queue": queue, "index": index, "position_ms": position_ms}),
        )
        .await
        .map(|_| ())
    }

    pub async fn add_queue(&self, track: &Track) -> Result<(), String> {
        self.request(
            "add_queue",
            json!({"track": serde_json::to_value(track).map_err(|error| error.to_string())?}),
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

    pub async fn login(&self) -> Result<(), String> {
        self.request("login", Value::Null).await.map(|_| ())
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.request("logout", Value::Null).await.map(|_| ())
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

    pub async fn create_playlist(&self, name: &str) -> Result<(), String> {
        let _ = self.request("edit_create_playlist", json!({"name": name})).await?;
        Ok(())
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

    /// Graceful engine shutdown for app exit; falls back to a hard kill.
    pub fn shutdown_engine(&self) {
        log::info("engine shutdown requested");
        self.shutting_down.store(true, Ordering::SeqCst);
        let line = build_line(&self.next_request_id(), "shutdown", &Value::Null);
        let _ = self.write_line(&line);
        std::thread::sleep(Duration::from_millis(400));
        if let Some(mut child) = self.process.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *self.stdin.lock() = None;
    }

    // ------------------------------------------------------------------
    // Reader-thread callbacks
    // ------------------------------------------------------------------

    fn on_state(&self, state: &PlaybackState) {
        *self.last_state.lock() = Some(state.clone());
        let _ = self.state_tx.send(state.clone());
    }

    fn on_eof(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        log::warn("engine process exited (stdout closed)");
        // Fail any in-flight requests so awaiters do not hang.
        let mut pending = self.pending.blocking_lock();
        for (_, sender) in pending.drain() {
            let _ = sender.send(EngineReply {
                ok: false,
                error: Some("the playback engine exited".to_owned()),
                data: None,
            });
        }
        drop(pending);
        // Capture what to restore once a fresh engine is ready again.
        if let Some(last) = self.last_state.lock().clone() {
            if last.auth_state == "ready" {
                *self.restore_pending.blocking_lock() = Some(RestoreSnapshot::from(&last));
            }
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

/// Reader thread: parses one protocol line per iteration. `state` lines are
/// fanned out to subscribers; every other line is routed to the pending
/// request with the matching id.
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
                        if value.get("type").and_then(Value::as_str) == Some("state") {
                            match serde_json::from_value::<PlaybackState>(value) {
                                Ok(state) => client.on_state(&state),
                                Err(error) => log::error(&format!(
                                    "could not parse engine state line: {error}"
                                )),
                            }
                        } else {
                            let request_id = value
                                .get("request_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                            let error =
                                value.get("error").and_then(Value::as_str).map(str::to_owned);
                            let data = value.get("data").cloned();
                            client.deliver(&request_id, EngineReply { ok, error, data });
                        }
                    }
                    Err(_) => break,
                }
            }
            client.on_eof();
        })
        .expect("could not start engine reader thread");
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

fn build_line(request_id: &str, kind: &str, args: &Value) -> String {
    let mut object = Map::new();
    object.insert("request_id".to_owned(), Value::String(request_id.to_owned()));
    object.insert("type".to_owned(), Value::String(kind.to_owned()));
    if let Some(fields) = args.as_object() {
        for (key, value) in fields {
            object.insert(key.clone(), value.clone());
        }
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
        let mut states = client.subscribe_state();

        // The engine announces its session immediately after startup.
        let first = tokio::time::timeout(Duration::from_secs(20), states.recv())
            .await
            .expect("engine emits its initial state within 20s")
            .expect("state channel stays open");
        assert!(!first.auth_state.is_empty());
        assert_eq!(first.auth_state, "needs_login", "fresh state dir has no session");

        // A command round-trip: status is answered and re-emits the state.
        client.status().await.expect("status command round-trips");
        let after_status = tokio::time::timeout(Duration::from_secs(20), states.recv())
            .await
            .expect("status triggers a fresh state line")
            .expect("state channel stays open");
        assert_eq!(after_status.auth_state, first.auth_state);

        // Browse without a session fails cleanly through the reply channel.
        let error = client.browse_playlists(10).await;
        assert!(error.is_err(), "browse without a session must fail cleanly");

        client.shutdown_engine();
        let _ = std::fs::remove_dir_all(&state_dir);
    }
}
