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
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, watch};

use crate::app::{
    clear_playback_snapshot, engine_state_dir, load_app_settings, load_playback_snapshot,
    save_playback_snapshot, PlaybackSnapshot,
};
use crate::log;
use crate::types::{PlaybackState, Track};

/// Replies for playback/session commands arrive promptly; browse and edit
/// commands run network round-trips inside the engine.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSE_TIMEOUT: Duration = Duration::from_secs(30);
const WAVEFORM_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Engine respawn backoff bounds.
const RESPAWN_BACKOFF_START: Duration = Duration::from_secs(2);
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Playback-state writes are coalesced on one background thread. Successful
/// writes are throttled, while transient failures retry promptly without
/// claiming that the snapshot was committed.
const PERSIST_MIN_INTERVAL: Duration = Duration::from_secs(5);
const PERSIST_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const PERSIST_POSITION_DRIFT_MS: u32 = 15_000;

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

/// One playback or lifecycle event, fanned out to subscribers in wire order.
/// Heartbeats share the channel with full states so a consumer can never
/// apply an older heartbeat after a newer full state; `Disconnected` marks
/// the gap between an engine EOF and its replacement becoming ready.
#[derive(Clone, Debug)]
pub enum StateLine {
    State(PlaybackState),
    Position(PositionHeartbeat),
    Disconnected,
}

/// Queue/settings captured either from the durable app snapshot (normal
/// startup, always paused) or from the last fresh state before an unexpected
/// child exit (resume only when that state was playing).
#[derive(Debug, Clone)]
pub struct RestoreSnapshot {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    /// Position in the compiled transport timeline. The engine maps it after
    /// resolving the active edit against the live edit store.
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: String,
    pub playback_speed: f32,
    pub resume_playing: bool,
}

impl RestoreSnapshot {
    /// Queue equality for restore completion: identity, order, and
    /// playability. The engine owns several row fields across an install —
    /// `effective_edit` re-resolves against the live edit store, `context`
    /// is refilled, metadata may have drifted server-side since the plan was
    /// captured — so equality stops at the stable identity of each row.
    /// Comparing anything the engine rewrites would hang the restore forever:
    /// every state suppressed as "not yet restored", an empty player bar over
    /// working audio.
    fn rows_match(a: &[Track], b: &[Track]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.id == y.id
                    && x.uri == y.uri
                    && x.duration_ms == y.duration_ms
                    && x.unavailable == y.unavailable
            })
    }

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
        // Download marks never survive the trip into a plan: they are
        // session-live truth, and this is the one boundary where disk claims
        // become engine instructions.
        for track in &mut self.queue {
            track.cached = false;
        }
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
        // Restore positions are compiled-timeline values. Resolving the live
        // edit store can shorten that timeline after the snapshot was written;
        // RestoreQueue then clamps the request to the new compiled duration.
        // Match the same canonical value or the restore gate would suppress
        // every ready state forever.
        let expected_position = if state.duration_ms == 0 {
            self.position_ms
        } else {
            self.position_ms.min(state.duration_ms)
        };
        let position_matches = if self.resume_playing {
            state.position_ms.abs_diff(expected_position) <= 2_500
        } else {
            state.position_ms == expected_position
        };
        state.auth_state == "ready"
            && Self::rows_match(&state.queue, &self.queue)
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

/// What the writer last committed to disk.
#[derive(Debug)]
struct PersistedSnapshot {
    written_at: Instant,
    snapshot: PlaybackSnapshot,
}

impl PersistedSnapshot {
    /// Position alone is exempt until it has drifted far enough to be worth a
    /// write. Compare fields directly so checking the threshold never clones
    /// the queue.
    fn superseded_by(&self, next: &PlaybackSnapshot) -> bool {
        let previous = &self.snapshot;
        previous.version != next.version
            || previous.queue != next.queue
            || previous.current_index != next.current_index
            || previous.volume != next.volume
            || previous.shuffle != next.shuffle
            || previous.repeat != next.repeat
            || previous.playback_speed != next.playback_speed
            || next.position_ms.abs_diff(previous.position_ms) >= PERSIST_POSITION_DRIFT_MS
    }
}

#[derive(Debug, Default)]
struct PersistenceSchedule {
    committed: Option<PersistedSnapshot>,
    committed_generation: u64,
    pending_generation: Option<u64>,
    retry_at: Option<Instant>,
}

impl PersistenceSchedule {
    fn mark_dirty(&mut self, generation: u64) {
        if generation > self.committed_generation {
            self.pending_generation = Some(
                self.pending_generation
                    .map_or(generation, |pending| pending.max(generation)),
            );
        }
    }

    fn wait_for(&self, now: Instant) -> Option<Duration> {
        self.pending_generation.map(|_| {
            let deadline = self.retry_at.unwrap_or_else(|| {
                self.committed
                    .as_ref()
                    .map_or(now, |committed| committed.written_at + PERSIST_MIN_INTERVAL)
            });
            deadline.saturating_duration_since(now)
        })
    }

    fn should_write(&self, snapshot: &PlaybackSnapshot) -> bool {
        self.committed
            .as_ref()
            .is_none_or(|committed| committed.superseded_by(snapshot))
    }

    fn record_unchanged(&mut self, attempted_generation: u64, latest_generation: u64) {
        self.committed_generation = self.committed_generation.max(attempted_generation);
        self.pending_generation =
            (latest_generation > self.committed_generation).then_some(latest_generation);
        self.retry_at = None;
    }

    fn record_success(
        &mut self,
        snapshot: PlaybackSnapshot,
        attempted_generation: u64,
        latest_generation: u64,
        written_at: Instant,
    ) {
        self.committed = Some(PersistedSnapshot {
            written_at,
            snapshot,
        });
        self.committed_generation = attempted_generation;
        self.pending_generation =
            (latest_generation > attempted_generation).then_some(latest_generation);
        self.retry_at = None;
    }

    fn record_failure(&mut self, latest_generation: u64, now: Instant) {
        self.mark_dirty(latest_generation);
        self.retry_at = Some(now + PERSIST_RETRY_INTERVAL);
    }
}

enum PersistCommand {
    Dirty(u64),
    Clear(mpsc::Sender<Result<(), String>>),
    Shutdown {
        generation: u64,
        result_tx: mpsc::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Clone)]
struct RestorePlan {
    snapshot: RestoreSnapshot,
    sent: bool,
    /// Preview leases must never be mistaken for startup/crash recovery.
    only_if_preview: bool,
    /// The editor context that currently owns this preview restore. The
    /// snapshot itself remains the first real state captured for the lease.
    preview_lease_id: Option<u64>,
}

pub struct EngineClient {
    state_dir: PathBuf,
    pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<EngineReply>>>,
    stdin: Mutex<Option<ChildStdin>>,
    process: Mutex<Option<Child>>,
    state_tx: tokio::sync::broadcast::Sender<StateLine>,
    exit_tx: watch::Sender<bool>,
    last_state: Mutex<Option<PlaybackState>>,
    /// The latest full engine state is a draft preview. Scalar heartbeats carry
    /// no mode bit, so this keeps them from advancing the retained real queue
    /// snapshot while a preview is live.
    preview_active: AtomicBool,
    restore_pending: Mutex<Option<RestorePlan>>,
    persist_tx: SyncSender<PersistCommand>,
    persist_generation: AtomicU64,
    persist_thread: Mutex<Option<JoinHandle<()>>>,
    next_request_id: AtomicU64,
    shutting_down: AtomicBool,
}

impl EngineClient {
    /// Creates a client and spawns the engine subprocess. The supervisor task
    /// (see [`EngineClient::supervise`]) takes over respawns from there.
    pub fn start() -> Arc<Self> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = watch::channel(false);
        let (persist_tx, persist_rx) = mpsc::sync_channel(1);
        let restore_pending = load_playback_snapshot().map(|snapshot| RestorePlan {
            snapshot: RestoreSnapshot::from_durable(snapshot),
            sent: false,
            only_if_preview: false,
            preview_lease_id: None,
        });
        let client = Arc::new(Self {
            state_dir: engine_state_dir(),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(None),
            preview_active: AtomicBool::new(false),
            restore_pending: Mutex::new(restore_pending),
            persist_tx,
            persist_generation: AtomicU64::new(0),
            persist_thread: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        });
        *client.persist_thread.lock() = Some(spawn_persistence_writer(&client, persist_rx));
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

    /// Starts the pending startup/crash restore exactly once, after a fresh
    /// engine reports that authentication is ready. Preview leases are kept
    /// separate from this path and are never consumed here.
    pub fn begin_pending_restore(&self, state: &PlaybackState) -> Option<RestoreSnapshot> {
        let mut pending = self.restore_pending.lock();
        let plan = pending.as_mut()?;
        if plan.only_if_preview || plan.sent || state.auth_state != "ready" {
            return None;
        }
        plan.sent = true;
        Some(plan.snapshot.clone())
    }

    pub fn restore_is_pending(&self) -> bool {
        self.restore_pending
            .lock()
            .as_ref()
            .is_some_and(|plan| !plan.only_if_preview)
    }

    fn clear_preview_restore_pending(&self) {
        let mut pending = self.restore_pending.lock();
        if pending.as_ref().is_some_and(|plan| plan.only_if_preview) {
            *pending = None;
        }
    }

    /// Claims the preview restore for `preview_lease_id`, capturing the first
    /// real state when necessary. Replacements transfer ownership before the
    /// engine validates the draft, but never replace the retained snapshot.
    fn capture_preview_restore(&self, preview_lease_id: u64) -> bool {
        let mut pending = self.restore_pending.lock();
        if let Some(plan) = pending.as_mut() {
            if plan.only_if_preview {
                plan.preview_lease_id = Some(preview_lease_id);
                plan.sent = false;
            } else {
                // A startup/crash restore is already a real snapshot. If an
                // editor opens before that restore is observed, promote the
                // same snapshot to a preview lease rather than losing it.
                plan.only_if_preview = true;
                plan.preview_lease_id = Some(preview_lease_id);
                plan.sent = false;
            }
            return false;
        }
        let Some(state) = self
            .last_state
            .lock()
            .as_ref()
            .filter(|state| state.auth_state == "ready")
            .cloned()
        else {
            return false;
        };
        *pending = Some(RestorePlan {
            snapshot: RestoreSnapshot::from_playback(&state, state.playing),
            sent: false,
            only_if_preview: true,
            preview_lease_id: Some(preview_lease_id),
        });
        true
    }

    fn finish_preview_restore(&self, preview_lease_id: u64, success: bool) {
        let mut pending = self.restore_pending.lock();
        let Some(plan) = pending
            .as_mut()
            .filter(|plan| plan.only_if_preview && plan.preview_lease_id == Some(preview_lease_id))
        else {
            return;
        };
        if success {
            *pending = None;
        } else {
            plan.sent = false;
        }
    }

    pub fn retry_pending_restore(&self) {
        if let Some(plan) = self
            .restore_pending
            .lock()
            .as_mut()
            .filter(|plan| !plan.only_if_preview)
        {
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
        // Read fresh on every spawn (including supervisor respawns) so a
        // saved preference is picked up without restarting the whole app.
        let settings = load_app_settings();
        let audio_cache_limit_mb = settings.audio_cache_limit_mb;
        command
            .arg("--state-dir")
            .arg(&self.state_dir)
            .arg("--log-file")
            .arg(&engine_log)
            .arg("--audio-cache-limit-mb")
            .arg(audio_cache_limit_mb.to_string())
            .arg("--normalisation")
            .arg(settings.normalisation.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(
                match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&engine_log)
                {
                    Ok(file) => Stdio::from(file),
                    Err(_) => Stdio::null(),
                },
            )
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
        let cancellation_track_id = waveform_timeout_cancellation(kind, &args);
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
        let timeout = request_timeout(kind);
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(reply)) if reply.ok => Ok(reply),
            Ok(Ok(reply)) => Err(reply
                .error
                .unwrap_or_else(|| "engine rejected the request".to_owned())),
            Ok(Err(_)) => Err("engine request was dropped".to_owned()),
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&request_id);
                if let Some(track_id) = cancellation_track_id {
                    // The caller has stopped waiting, so release the engine's
                    // one global waveform worker as well. This is deliberately
                    // untracked: cancellation is idempotent and its response
                    // has no payload the shell needs.
                    let cancel_id = self.next_request_id();
                    let cancel = build_line(
                        &cancel_id,
                        "cancel_track_waveform",
                        json!({"track_id": track_id}),
                    );
                    let _ = self.write_line(&cancel);
                }
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
        automatic_start: bool,
    ) -> Result<(), String> {
        let result = self
            .request(
                "play_queue",
                json!({
                    "queue": queue,
                    "index": index,
                    "position_ms": position_ms,
                    "context": context,
                    "automatic_start": automatic_start,
                }),
            )
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn preview_track_edit(
        &self,
        track: &Track,
        cuts: &[spotify_playback_engine::protocol::TimeRange],
        loop_range: Option<spotify_playback_engine::protocol::LoopRange>,
        position_ms: u32,
        preview_lease_id: u64,
    ) -> Result<(), String> {
        // Claim ownership before sending the draft. The engine performs the
        // same transfer before replacement validation, so a rejected draft
        // still leaves the installed preview restorable by this context.
        self.capture_preview_restore(preview_lease_id);
        self.request(
            "preview_track_edit",
            json!({
                "track": track,
                "cuts": cuts,
                "loop_range": loop_range,
                "position_ms": position_ms,
                "preview_lease_id": preview_lease_id,
            }),
        )
        .await
        .map(|_| ())
    }

    /// Restores the first real queue captured before an editor preview. The
    /// engine guards this request with preview mode and the lease ID, so
    /// tearing down an old editor can never replace a newer real queue or
    /// preview.
    pub async fn restore_preview(&self, preview_lease_id: u64) -> Result<(), String> {
        let snapshot = {
            let mut pending = self.restore_pending.lock();
            let Some(plan) = pending.as_mut().filter(|plan| {
                plan.only_if_preview && plan.preview_lease_id == Some(preview_lease_id)
            }) else {
                // A stale teardown is deliberately an idempotent no-op. In
                // particular it must not consume the current editor lease.
                return Ok(());
            };
            if plan.sent {
                return Ok(());
            }
            plan.sent = true;
            plan.snapshot.clone()
        };
        let result = self
            .request(
                "restore_queue",
                json!({
                    "queue": snapshot.queue,
                    "index": snapshot.current_index.unwrap_or(0),
                    "position_ms": snapshot.position_ms,
                    "context": "",
                    "only_if_preview": true,
                    "preview_lease_id": preview_lease_id,
                    "resume_playing": snapshot.resume_playing,
                }),
            )
            .await
            .map(|_| ());
        // A transport error is uncertain: the engine may have executed the
        // restore. Keep ownership and permit a later settled teardown to
        // retry. A matching successful response may consume the lease.
        self.finish_preview_restore(preview_lease_id, result.is_ok());
        result
    }

    pub async fn restore_queue(
        &self,
        queue: &[Track],
        index: usize,
        position_ms: u32,
        context: &str,
    ) -> Result<(), String> {
        let result = self
            .request(
                "restore_queue",
                json!({
                    "queue": queue,
                    "index": index,
                    "position_ms": position_ms,
                    "context": context,
                }),
            )
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn play_queue_index(&self, index: usize) -> Result<(), String> {
        let result = self
            .request("play_queue_index", json!({"index": index}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn add_queue(&self, track: &Track, context: &str) -> Result<(), String> {
        let result = self
            .request("add_queue", json!({"track": track, "context": context}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn add_queue_batch(&self, tracks: &[Track], context: &str) -> Result<(), String> {
        let result = self
            .request(
                "add_queue_batch",
                json!({"tracks": tracks, "context": context}),
            )
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn remove_queue(&self, index: usize) -> Result<(), String> {
        let result = self
            .request("remove_queue", json!({"index": index}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn move_queue(&self, from: usize, to: usize) -> Result<(), String> {
        let result = self
            .request("move_queue", json!({"from": from, "to": to}))
            .await
            .map(|_| ());
        if result.is_ok() {
            self.clear_preview_restore_pending();
        }
        result
    }

    pub async fn get_history(
        &self,
    ) -> Result<Vec<spotify_playback_engine::protocol::HistoryItem>, String> {
        let reply = self.request("get_history", Value::Null).await?;
        parse_data(reply, "get_history")
    }

    pub async fn clear_history(&self) -> Result<(), String> {
        self.request("clear_history", Value::Null).await.map(|_| ())
    }

    pub async fn get_track_waveform(
        &self,
        track_id: &str,
    ) -> Result<spotify_playback_engine::protocol::TrackWaveform, String> {
        let reply = self
            .request("get_track_waveform", json!({"track_id": track_id}))
            .await?;
        parse_data(reply, "get_track_waveform")
    }

    pub async fn cancel_track_waveform(&self, track_id: &str) -> Result<(), String> {
        self.request("cancel_track_waveform", json!({"track_id": track_id}))
            .await
            .map(|_| ())
    }

    pub async fn login(&self) -> Result<(), String> {
        self.request("login", Value::Null).await.map(|_| ())
    }

    /// Applies the volume-normalisation preference to the live engine. The
    /// engine rebuilds its player from cached credentials, so a moment of
    /// reconnection is expected while a session is up.
    pub async fn set_normalisation(&self, enabled: bool) -> Result<(), String> {
        self.request("set_normalisation", json!({ "enabled": enabled }))
            .await
            .map(|_| ())
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.request("logout", Value::Null).await?;
        // EOF can race a successful logout. Keep this lock order identical to
        // on_eof and on_state so a pre-logout real queue can never be
        // installed after logout has invalidated it.
        {
            let mut pending = self.restore_pending.lock();
            let mut last_state = self.last_state.lock();
            *pending = None;
            *last_state = None;
        }
        self.preview_active.store(false, Ordering::Release);
        self.clear_persisted_playback_state()
    }

    fn clear_persisted_playback_state(&self) -> Result<(), String> {
        // Invalidates any snapshot an in-flight attempt captured before logout.
        self.persist_generation.fetch_add(1, Ordering::AcqRel);
        let (result_tx, result_rx) = mpsc::channel();
        let result = self
            .persist_tx
            .send(PersistCommand::Clear(result_tx))
            .map_err(|_| "playback-state writer is unavailable".to_owned())
            .and_then(|_| {
                result_rx.recv().map_err(|error| {
                    format!("playback-state writer stopped during clear: {error}")
                })?
            });
        match result {
            Ok(()) => Ok(()),
            Err(error) => clear_playback_snapshot().map_err(|fallback| {
                format!("{error}; direct playback-state clear failed: {fallback}")
            }),
        }
    }

    // ------------------------------------------------------------------
    // Browse / edit requests (typed payloads from `data`)
    // ------------------------------------------------------------------

    pub async fn browse_playlists(
        &self,
        length: usize,
    ) -> Result<Vec<spotify_playback_engine::protocol::PlaylistRef>, String> {
        let reply = self
            .request("browse_playlists", json!({"length": length}))
            .await?;
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

    pub async fn browse_artist_songwriter(
        &self,
        id: &str,
        name: &str,
    ) -> Result<Option<spotify_playback_engine::protocol::SongwriterPlaylist>, String> {
        let reply = self
            .request("browse_artist_songwriter", json!({"id": id, "name": name}))
            .await?;
        parse_data(reply, "browse_artist_songwriter")
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

    /// Saved Tracks as bare URIs — the membership lookup the saved mark
    /// reconciles with. Never fetches track metadata.
    pub async fn browse_liked_uris(
        &self,
        cursor: Option<&str>,
    ) -> Result<spotify_playback_engine::protocol::LikedUrisPage, String> {
        let reply = self
            .request("browse_liked_uris", json!({"cursor": cursor}))
            .await?;
        parse_data(reply, "browse_liked_uris")
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
        loop_range: Option<spotify_playback_engine::protocol::LoopRange>,
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

    pub async fn set_playlist_track_excluded(
        &self,
        playlist_id: &str,
        track_id: &str,
        excluded: bool,
    ) -> Result<(), String> {
        self.request(
            "set_playlist_track_excluded",
            json!({
                "playlist_id": playlist_id,
                "track_id": track_id,
                "excluded": excluded,
            }),
        )
        .await
        .map(|_| ())
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
        let _ = self
            .request("edit_delete_playlist", json!({"id": id}))
            .await?;
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
            .request(
                "edit_remove_playlist_tracks",
                json!({"id": id, "uris": uris}),
            )
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

    /// Stops the engine before requesting an unthrottled final flush from the
    /// sole persistence writer. Once shutdown starts, reader callbacks freeze
    /// state publication so the final finite generation is stable.
    pub fn shutdown_engine(&self) {
        log::info("engine shutdown requested");
        self.shutting_down.store(true, Ordering::SeqCst);

        let line = build_line(&self.next_request_id(), "shutdown", Value::Null);
        let _ = self.write_line(&line);
        std::thread::sleep(Duration::from_millis(400));
        if let Some(mut child) = self.process.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *self.stdin.lock() = None;

        let generation = self.persist_generation.load(Ordering::Acquire);
        let (result_tx, result_rx) = mpsc::channel();
        if self
            .persist_tx
            .send(PersistCommand::Shutdown {
                generation,
                result_tx,
            })
            .is_ok()
        {
            match result_rx.recv() {
                Ok(Err(error)) => log::warn(&format!("could not persist playback state: {error}")),
                Err(error) => log::warn(&format!(
                    "playback-state writer stopped before shutdown flush: {error}"
                )),
                Ok(Ok(())) => {}
            }
        }
        if let Some(writer) = self.persist_thread.lock().take() {
            let _ = writer.join();
        }
    }

    /// O(1) reader-thread notification. The bounded channel coalesces bursts;
    /// the generation lets the writer detect changes that arrived while it
    /// cloned or wrote a previous snapshot.
    fn mark_persistence_dirty(&self) {
        let generation = self.persist_generation.fetch_add(1, Ordering::AcqRel) + 1;
        match self.persist_tx.try_send(PersistCommand::Dirty(generation)) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) if self.shutting_down.load(Ordering::Relaxed) => {}
            Err(TrySendError::Disconnected(_)) => {
                log::warn("playback-state writer is unavailable");
            }
        }
    }

    /// Clones and normalizes the queue only when the writer's deadline has
    /// arrived. Restore intermediates are never durable state.
    fn playback_snapshot_for_writer(&self, include_restore: bool) -> Option<PlaybackSnapshot> {
        let pending = self.restore_pending.lock();
        if !include_restore && pending.is_some() {
            return None;
        }
        let last_state = self.last_state.lock();
        last_state
            .as_ref()
            .filter(|state| state.auth_state == "ready")
            .map(PlaybackSnapshot::from_playback)
            .or_else(|| {
                if include_restore {
                    pending.as_ref().map(|plan| plan.snapshot.durable())
                } else {
                    None
                }
            })
    }

    // ------------------------------------------------------------------
    // Reader-thread callbacks
    // ------------------------------------------------------------------

    fn on_state(&self, state: &PlaybackState) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        // A false state only reconciles a lease after this client has
        // observed the engine in preview mode. An unrelated false state
        // arriving before activation must not clear a freshly claimed lease.
        let was_preview = self.preview_active.swap(state.preview, Ordering::AcqRel);
        let mut pending = self.restore_pending.lock();
        if was_preview
            && !state.preview
            && pending.as_ref().is_some_and(|plan| plan.only_if_preview)
        {
            *pending = None;
        }
        let matched = pending
            .as_ref()
            .is_some_and(|plan| !plan.only_if_preview && plan.sent && plan.snapshot.matches(state));
        if matched {
            *pending = None;
        }
        let suppress_snapshot = state.preview
            || pending
                .as_ref()
                .is_some_and(|plan| !plan.only_if_preview && state.auth_state == "ready");
        if !suppress_snapshot {
            *self.last_state.lock() = Some(state.clone());
            self.mark_persistence_dirty();
        }
        drop(pending);
        let _ = self.state_tx.send(StateLine::State(state.clone()));
    }

    /// Applies a scalar position heartbeat to the heartbeat-fresh last real
    /// state. Preview heartbeats still reach the window, but cannot mutate the
    /// queue snapshot used for persistence and child-crash recovery.
    fn on_position(&self, heartbeat: PositionHeartbeat) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let changed = if self.preview_active.load(Ordering::Acquire) {
            false
        } else if let Some(last) = self.last_state.lock().as_mut() {
            last.position_ms = heartbeat.position_ms;
            last.duration_ms = heartbeat.duration_ms;
            true
        } else {
            false
        };
        if changed {
            self.mark_persistence_dirty();
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
        let mut restore = self.restore_pending.lock();
        let last = self.last_state.lock().clone();
        if let Some(last) = last.filter(|state| state.auth_state == "ready") {
            *restore = Some(RestorePlan {
                snapshot: RestoreSnapshot::from_playback(&last, last.playing),
                sent: false,
                only_if_preview: false,
                preview_lease_id: None,
            });
        } else if restore.as_ref().is_some_and(|plan| plan.only_if_preview) {
            *restore = None;
        } else if let Some(plan) = restore.as_mut() {
            plan.sent = false;
        }
        let _ = self.state_tx.send(StateLine::Disconnected);
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

fn spawn_persistence_writer(
    client: &Arc<EngineClient>,
    receiver: Receiver<PersistCommand>,
) -> JoinHandle<()> {
    let client = Arc::downgrade(client);
    std::thread::Builder::new()
        .name("playback-state-writer".to_owned())
        .spawn(move || run_persistence_writer(client, receiver))
        .expect("could not start playback-state writer")
}

fn run_persistence_writer(client: Weak<EngineClient>, receiver: Receiver<PersistCommand>) {
    let mut schedule = PersistenceSchedule::default();
    loop {
        let command = match schedule.wait_for(Instant::now()) {
            Some(wait) => match receiver.recv_timeout(wait) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match receiver.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            },
        };

        match command {
            Some(PersistCommand::Dirty(generation)) => {
                schedule.mark_dirty(generation);
            }
            Some(PersistCommand::Clear(result_tx)) => {
                let result = clear_playback_snapshot();
                if result.is_ok() {
                    schedule = PersistenceSchedule::default();
                    schedule.committed_generation = client.upgrade().map_or(0, |client| {
                        client.persist_generation.load(Ordering::Acquire)
                    });
                }
                let _ = result_tx.send(result);
            }
            Some(PersistCommand::Shutdown {
                generation,
                result_tx,
            }) => {
                let result = client.upgrade().map_or(Ok(()), |client| {
                    // Shutdown is ordered after the engine is frozen, but
                    // retain a finite generation fence around the final clone
                    // so a last reader callback can never overwrite it with
                    // an older snapshot.
                    let current = client.persist_generation.load(Ordering::Acquire);
                    let mut snapshot = client.playback_snapshot_for_writer(true);
                    if current > generation {
                        snapshot = client.playback_snapshot_for_writer(true);
                    }
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    if latest > current {
                        snapshot = client.playback_snapshot_for_writer(true);
                    }
                    snapshot.map_or(Ok(()), |snapshot| save_playback_snapshot(&snapshot))
                });
                let _ = result_tx.send(result);
                return;
            }
            None => {
                let Some(client) = client.upgrade() else {
                    return;
                };
                // The queued wake may represent an older generation because
                // the bounded channel coalesces bursts. Snapshot the current
                // generation at the deadline so this attempt commits exactly
                // the state it is about to clone.
                let attempted_generation = client.persist_generation.load(Ordering::Acquire);
                let Some(snapshot) = client.playback_snapshot_for_writer(false) else {
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    schedule.record_unchanged(attempted_generation, latest);
                    continue;
                };
                if !schedule.should_write(&snapshot) {
                    let latest = client.persist_generation.load(Ordering::Acquire);
                    schedule.record_unchanged(attempted_generation, latest);
                    continue;
                }

                let written_at = Instant::now();
                match save_playback_snapshot(&snapshot) {
                    Ok(()) => {
                        let latest = client.persist_generation.load(Ordering::Acquire);
                        schedule.record_success(snapshot, attempted_generation, latest, written_at);
                    }
                    Err(error) => {
                        log::warn(&format!("could not persist playback state: {error}"));
                        let latest = client.persist_generation.load(Ordering::Acquire);
                        schedule.record_failure(latest.max(attempted_generation), written_at);
                    }
                }
            }
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

fn request_timeout(kind: &str) -> Duration {
    if kind == "get_track_waveform" {
        WAVEFORM_TIMEOUT
    } else if kind.starts_with("browse_") || kind.starts_with("edit_") {
        BROWSE_TIMEOUT
    } else {
        COMMAND_TIMEOUT
    }
}

fn waveform_timeout_cancellation(kind: &str, args: &Value) -> Option<String> {
    if kind != "get_track_waveform" {
        return None;
    }
    args.get("track_id")?.as_str().map(str::to_owned)
}

fn build_line(request_id: &str, kind: &str, args: Value) -> String {
    let mut object = Map::new();
    object.insert(
        "request_id".to_owned(),
        Value::String(request_id.to_owned()),
    );
    object.insert("type".to_owned(), Value::String(kind.to_owned()));
    if let Value::Object(fields) = args {
        object.extend(fields);
    }
    serde_json::to_string(&Value::Object(object)).expect("request serialization cannot fail")
}

fn parse_data<T: serde::de::DeserializeOwned>(reply: EngineReply, kind: &str) -> Result<T, String> {
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
    // Workspace builds use the same target directory as the bundler resource:
    // <repo>/target/release/SpotifyPlaybackEngine.exe. Do not fall back to an
    // engine-local target tree; that would create a second artifact contract.
    let workspace_release = exe_dir
        .parent()?
        .join("release")
        .join("SpotifyPlaybackEngine.exe");
    workspace_release.is_file().then_some(workspace_release)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `EngineClient` with no live engine process, for unit tests of the
    /// reader-side callbacks.
    fn client_with_last_state(state: PlaybackState) -> Arc<EngineClient> {
        let (state_tx, _) = tokio::sync::broadcast::channel(64);
        let (exit_tx, _) = tokio::sync::watch::channel(false);
        let (persist_tx, _persist_rx) = mpsc::sync_channel(1);
        Arc::new(EngineClient {
            state_dir: PathBuf::new(),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            process: Mutex::new(None),
            state_tx,
            exit_tx,
            last_state: Mutex::new(Some(state)),
            preview_active: AtomicBool::new(false),
            restore_pending: Mutex::new(None),
            persist_tx,
            persist_generation: AtomicU64::new(0),
            persist_thread: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        })
    }

    fn persisted(snapshot: PlaybackSnapshot) -> PersistedSnapshot {
        PersistedSnapshot {
            written_at: Instant::now(),
            snapshot,
        }
    }

    #[test]
    fn waveform_requests_use_the_long_timeout_and_shared_wire_shape() {
        assert_eq!(
            request_timeout("get_track_waveform"),
            Duration::from_secs(300)
        );
        assert_eq!(request_timeout("cancel_track_waveform"), COMMAND_TIMEOUT);
        assert_eq!(request_timeout("browse_album"), BROWSE_TIMEOUT);
        assert_eq!(
            waveform_timeout_cancellation(
                "get_track_waveform",
                &json!({"track_id": "0123456789ABCDEFGHIJKL"}),
            )
            .as_deref(),
            Some("0123456789ABCDEFGHIJKL"),
        );
        assert!(waveform_timeout_cancellation(
            "cancel_track_waveform",
            &json!({"track_id": "0123456789ABCDEFGHIJKL"}),
        )
        .is_none());

        let line = build_line(
            "waveform-1",
            "get_track_waveform",
            json!({"track_id": "0123456789ABCDEFGHIJKL"}),
        );
        let request: spotify_playback_engine::protocol::Request =
            serde_json::from_str(&line).unwrap();
        assert!(matches!(
            request.command,
            spotify_playback_engine::protocol::Command::GetTrackWaveform { track_id }
                if track_id == "0123456789ABCDEFGHIJKL"
        ));
    }

    #[test]
    fn preview_request_uses_the_shared_track_edit_wire_shape() {
        let track = Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        };
        let cuts = vec![spotify_playback_engine::protocol::TimeRange {
            start_ms: 1_000,
            end_ms: 2_500,
        }];
        let loop_range = Some(spotify_playback_engine::protocol::LoopRange {
            start_ms: 5_000,
            end_ms: 9_000,
            play_count: 2,
        });
        let line = build_line(
            "preview-1",
            "preview_track_edit",
            json!({
                "track": track,
                "cuts": cuts,
                "loop_range": loop_range,
                "position_ms": 42_000,
                "preview_lease_id": 17,
            }),
        );
        let request: spotify_playback_engine::protocol::Request =
            serde_json::from_str(&line).unwrap();
        let spotify_playback_engine::protocol::Command::PreviewTrackEdit {
            track,
            cuts,
            loop_range,
            position_ms,
            preview_lease_id,
        } = request.command
        else {
            panic!("Tauri preview request must retain the dedicated engine shape");
        };
        assert_eq!(track.uri, "spotify:track:0123456789ABCDEFGHIJKL");
        assert_eq!(cuts.len(), 1);
        assert_eq!(loop_range.unwrap().end_ms, 9_000);
        assert_eq!(position_ms, 42_000);
        assert_eq!(preview_lease_id, 17);
    }

    #[test]
    fn guarded_restore_request_carries_the_preview_lease_id() {
        let line = build_line(
            "restore-1",
            "restore_queue",
            json!({
                "queue": [],
                "index": 0,
                "position_ms": 0,
                "context": "",
                "only_if_preview": true,
                "preview_lease_id": 17,
                "resume_playing": true,
            }),
        );
        let request: spotify_playback_engine::protocol::Request =
            serde_json::from_str(&line).unwrap();
        let spotify_playback_engine::protocol::Command::RestoreQueue {
            only_if_preview,
            preview_lease_id,
            resume_playing,
            ..
        } = request.command
        else {
            panic!("preview teardown must use the restore queue command");
        };
        assert!(only_if_preview);
        assert_eq!(preview_lease_id, 17);
        assert!(resume_playing);
    }
    #[test]
    fn waveform_response_payload_is_typed_at_the_client_boundary() {
        let waveform: spotify_playback_engine::protocol::TrackWaveform = parse_data(
            EngineReply {
                ok: true,
                error: None,
                data: Some(json!({
                    "track_id": "0123456789ABCDEFGHIJKL",
                    "duration_ms": 25,
                    "interval_ms": 1,
                    "bin_count": 3,
                    "peaks_base64": "AAAAAAAAAAAAAAAA",
                })),
            },
            "get_track_waveform",
        )
        .unwrap();
        assert_eq!(waveform.bin_count, 3);
        assert_eq!(waveform.interval_ms, 1);
    }

    /// Heartbeats notify the writer several times a second. Rewriting the
    /// queue each time would be pure churn, so only a real change — or a
    /// playhead that has moved far enough to be worth restoring to — counts.
    #[test]
    fn only_a_real_change_or_a_drifted_playhead_supersedes_the_written_snapshot() {
        let mut state = PlaybackState::default();
        state.queue = vec![Track {
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }];
        state.current_index = Some(0);
        state.position_ms = 30_000;
        let written = persisted(PlaybackSnapshot::from_playback(&state));

        assert!(
            !written.superseded_by(&PlaybackSnapshot::from_playback(&state)),
            "an identical snapshot is not worth a write"
        );

        let mut nudged = state.clone();
        nudged.position_ms = 30_000 + PERSIST_POSITION_DRIFT_MS - 1;
        assert!(
            !written.superseded_by(&PlaybackSnapshot::from_playback(&nudged)),
            "the playhead alone is exempt until it has drifted far enough"
        );

        let mut drifted = state.clone();
        drifted.position_ms = 30_000 + PERSIST_POSITION_DRIFT_MS;
        assert!(written.superseded_by(&PlaybackSnapshot::from_playback(&drifted)));

        let mut paused_elsewhere = state.clone();
        paused_elsewhere.position_ms = 30_100;
        paused_elsewhere.volume = 41;
        assert!(
            written.superseded_by(&PlaybackSnapshot::from_playback(&paused_elsewhere)),
            "anything but the playhead is worth a write immediately"
        );
    }

    #[test]
    fn persistence_coalesces_dirty_generations_until_the_write_deadline() {
        let started = Instant::now();
        let snapshot = PlaybackSnapshot::from_playback(&PlaybackState::default());
        let mut schedule = PersistenceSchedule::default();

        schedule.mark_dirty(1);
        schedule.mark_dirty(2);
        assert_eq!(schedule.pending_generation, Some(2));
        assert_eq!(schedule.wait_for(started), Some(Duration::ZERO));

        schedule.record_success(snapshot, 2, 2, started);
        schedule.mark_dirty(3);
        schedule.mark_dirty(4);
        assert_eq!(schedule.pending_generation, Some(4));
        assert_eq!(
            schedule.wait_for(started + Duration::from_secs(2)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            schedule.wait_for(started + PERSIST_MIN_INTERVAL),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn persistence_failure_does_not_advance_the_committed_marker_and_retries() {
        let started = Instant::now();
        let original = PlaybackSnapshot::from_playback(&PlaybackState::default());
        let mut changed = original.clone();
        changed.volume = 42;
        let mut schedule = PersistenceSchedule::default();
        schedule.record_success(original.clone(), 1, 1, started);
        schedule.mark_dirty(2);

        let failed_at = started + PERSIST_MIN_INTERVAL;
        schedule.record_failure(2, failed_at);
        assert_eq!(schedule.committed_generation, 1);
        assert_eq!(
            schedule.committed.as_ref().map(|value| &value.snapshot),
            Some(&original)
        );
        assert_eq!(schedule.wait_for(failed_at), Some(PERSIST_RETRY_INTERVAL));
        assert!(schedule.should_write(&changed));

        schedule.record_success(changed.clone(), 2, 2, failed_at + PERSIST_RETRY_INTERVAL);
        assert_eq!(schedule.committed_generation, 2);
        assert_eq!(
            schedule.committed.as_ref().map(|value| &value.snapshot),
            Some(&changed)
        );
        assert_eq!(schedule.wait_for(Instant::now()), None);
    }

    #[test]
    fn persistence_keeps_a_newer_generation_dirty_after_a_success() {
        let mut schedule = PersistenceSchedule::default();
        let snapshot = PlaybackSnapshot::from_playback(&PlaybackState::default());
        schedule.mark_dirty(1);
        schedule.record_success(snapshot, 1, 3, Instant::now());
        assert_eq!(schedule.committed_generation, 1);
        assert_eq!(schedule.pending_generation, Some(3));
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
                "automatic_start": true,
            }),
        );
        let value: Value = serde_json::from_str(&line).expect("request line is JSON");
        assert_eq!(value["request_id"], "request-7");
        assert_eq!(value["type"], "play_queue");
        assert_eq!(value["queue"][0]["uri"], "spotify:track:abc");
        assert_eq!(value["index"], 0);
        assert_eq!(value["position_ms"], 12);
        assert!(value["automatic_start"].as_bool().unwrap());
        let request: spotify_playback_engine::protocol::Request =
            serde_json::from_str(&line).expect("play queue request is valid protocol JSON");
        let spotify_playback_engine::protocol::Command::PlayQueue {
            automatic_start, ..
        } = request.command
        else {
            panic!("play queue request must use its dedicated command");
        };
        assert!(automatic_start);

        let legacy = build_line(
            "legacy-play-queue",
            "play_queue",
            serde_json::json!({
                "queue": [],
                "index": 0,
                "position_ms": 0,
                "context": "",
            }),
        );
        let spotify_playback_engine::protocol::Request {
            command:
                spotify_playback_engine::protocol::Command::PlayQueue {
                    automatic_start, ..
                },
            ..
        } = serde_json::from_str(&legacy).expect("legacy play queue remains valid")
        else {
            panic!("legacy play queue must deserialize as PlayQueue");
        };
        assert!(!automatic_start);
    }

    #[test]
    fn playlist_exclusion_request_keeps_authoritative_wire_fields() {
        let line = build_line(
            "request-exclusion",
            "set_playlist_track_excluded",
            serde_json::json!({
                "playlist_id": "playlist-1",
                "track_id": "track-1",
                "excluded": true,
            }),
        );
        let request: spotify_playback_engine::protocol::Request =
            serde_json::from_str(&line).expect("exclusion request is valid protocol JSON");
        let spotify_playback_engine::protocol::Command::SetPlaylistTrackExcluded {
            playlist_id,
            track_id,
            excluded,
        } = request.command
        else {
            panic!("playlist exclusion request must use its dedicated command");
        };
        assert_eq!(playlist_id, "playlist-1");
        assert_eq!(track_id, "track-1");
        assert!(excluded);
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
            Some(Line::Reply {
                request_id,
                ok,
                error,
                data,
            }) => {
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
            StateLine::Disconnected => panic!("heartbeat path must stay connected"),
        }
    }

    #[test]
    fn preview_states_and_heartbeats_keep_the_real_crash_restore_snapshot() {
        let mut normal = PlaybackState::default();
        normal.ready = true;
        normal.auth_state = "ready".to_owned();
        normal.playing = true;
        normal.position_ms = 12_000;
        normal.duration_ms = 240_000;
        normal.current_index = Some(0);
        normal.queue = vec![Track {
            id: "real-track".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }];
        let client = client_with_last_state(normal.clone());

        let mut preview = normal.clone();
        preview.preview = true;
        preview.position_ms = 80_000;
        preview.queue[0].id = "draft-track".to_owned();
        client.on_state(&preview);
        client.on_position(PositionHeartbeat {
            position_ms: 90_000,
            duration_ms: 180_000,
        });

        let retained = client.last_state.lock().clone().unwrap();
        assert_eq!(retained.queue[0].id, "real-track");
        assert_eq!(retained.position_ms, 12_000);

        client.on_eof();
        let restore = client.restore_pending.lock();
        let restore = &restore.as_ref().expect("real queue retained").snapshot;
        assert_eq!(restore.queue[0].id, "real-track");
        assert_eq!(restore.position_ms, 12_000);
        assert!(restore.resume_playing);
    }

    #[test]
    fn preview_restore_lease_replacement_keeps_snapshot_and_changes_owner() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.playing = true;
        real.current_index = Some(0);
        real.queue = vec![Track {
            id: "real".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }];
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        let captured = client
            .restore_pending
            .lock()
            .as_ref()
            .expect("preview lease captured")
            .snapshot
            .clone();

        assert!(!client.capture_preview_restore(29));
        let plan = client.restore_pending.lock();
        let plan = plan.as_ref().expect("replacement keeps the lease");
        assert_eq!(plan.preview_lease_id, Some(29));
        assert_eq!(plan.snapshot.queue, captured.queue);
        assert_eq!(plan.snapshot.position_ms, captured.position_ms);
    }

    #[tokio::test]
    async fn stale_preview_restore_is_a_noop_for_the_current_lease() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()];
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        assert!(!client.capture_preview_restore(29));
        assert_eq!(client.restore_preview(17).await, Ok(()));

        let plan = client.restore_pending.lock();
        let plan = plan.as_ref().expect("stale restore keeps current lease");
        assert_eq!(plan.preview_lease_id, Some(29));
        assert!(!plan.sent, "stale restore must not reserve the request");
    }

    #[tokio::test]
    async fn preview_restore_request_error_retains_lease_for_reconciliation() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()];
        let client = client_with_last_state(real);

        assert!(client.capture_preview_restore(17));
        assert!(client.restore_preview(17).await.is_err());

        let plan = client
            .restore_pending
            .lock()
            .clone()
            .expect("transport uncertainty retains the snapshot");
        assert_eq!(plan.preview_lease_id, Some(17));
        assert!(!plan.sent, "a failed request is retryable");
    }

    #[test]
    fn preview_true_to_false_clears_only_an_authoritatively_active_lease() {
        let mut real = PlaybackState::default();
        real.auth_state = "ready".to_owned();
        real.queue = vec![Track::default()];
        let client = client_with_last_state(real.clone());
        assert!(client.capture_preview_restore(17));

        // An unrelated real state before activation cannot consume the lease.
        client.on_state(&real);
        assert!(client.restore_pending.lock().is_some());

        let mut preview = real.clone();
        preview.preview = true;
        client.on_state(&preview);
        assert!(client.restore_pending.lock().is_some());

        let mut restored = preview;
        restored.preview = false;
        client.on_state(&restored);
        assert!(
            client.restore_pending.lock().is_none(),
            "authoritative preview exit reconciles a dropped restore reply"
        );
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
            only_if_preview: false,
            preview_lease_id: None,
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

    #[test]
    fn restore_plans_never_carry_download_marks() {
        let mut state = PlaybackState::default();
        state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: true,
            ..Track::default()
        }];
        state.current_index = Some(0);

        // Both constructors funnel through `normalized`: a live crash-recovery
        // state and an on-disk snapshot written by an older build alike.
        assert!(!RestoreSnapshot::from_playback(&state, false).queue[0].cached);
        let durable = RestoreSnapshot::from_playback(&state, false).durable();
        assert!(!durable.queue[0].cached);
        assert!(!RestoreSnapshot::from_durable(durable).queue[0].cached);
    }

    #[test]
    fn restore_matches_ignores_fields_the_engine_rewrites_on_install() {
        // The snapshot as saved: marks, an edit, and a context from a past
        // session. The engine's restored queue legitimately differs in all
        // three — cached is dropped on install, edits re-resolve against the
        // store, context is refilled — and none of that may hang the restore.
        let mut snapshot_state = PlaybackState::default();
        snapshot_state.auth_state = "ready".to_owned();
        snapshot_state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: true,
            effective_edit: None,
            context: "playlist:abc".to_owned(),
            ..Track::default()
        }];
        snapshot_state.current_index = Some(0);
        let restore = RestoreSnapshot::from_playback(&snapshot_state, false);

        let mut restored = PlaybackState::default();
        restored.auth_state = "ready".to_owned();
        restored.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            cached: false,
            ..Track::default()
        }];
        restored.current_index = Some(0);

        assert!(
            restore.matches(&restored),
            "volatile row fields must not keep the restore pending forever"
        );

        // Identity still matters: a different queue is not "restored".
        restored.queue[0].id = "zzzzzzzzzzzzzzzzzzzzzz".to_owned();
        assert!(!restore.matches(&restored));
    }

    #[test]
    fn restore_matches_a_position_clamped_by_a_new_shorter_compiled_timeline() {
        let mut snapshot_state = PlaybackState::default();
        snapshot_state.auth_state = "ready".to_owned();
        snapshot_state.queue = vec![Track {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..Track::default()
        }];
        snapshot_state.current_index = Some(0);
        snapshot_state.position_ms = 200_000;
        let restore = RestoreSnapshot::from_playback(&snapshot_state, false);

        let mut restored = snapshot_state;
        restored.position_ms = 150_000;
        restored.duration_ms = 150_000;

        assert!(
            restore.matches(&restored),
            "a newly enabled cut may clamp an old compiled snapshot to the new duration"
        );
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
            root.join("target")
                .join("debug")
                .join("SpotifyPlaybackEngine.exe"),
            root.join("target")
                .join("release")
                .join("SpotifyPlaybackEngine.exe"),
            root.join("engine")
                .join("target")
                .join("debug")
                .join("SpotifyPlaybackEngine.exe"),
            root.join("engine")
                .join("target")
                .join("release")
                .join("SpotifyPlaybackEngine.exe"),
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
        client
            .status()
            .await
            .expect("initial status command round-trips");

        // The engine announces its session in response to the status sync.
        let first = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("engine emits its initial state within 20s")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("the initial line is a full state, not a heartbeat"),
            StateLine::Disconnected => panic!("engine disconnected before its initial state"),
        };
        assert!(!first.auth_state.is_empty());
        assert_eq!(
            first.auth_state, "needs_login",
            "fresh state dir has no session"
        );

        // A command round-trip: status is answered and re-emits the state.
        client.status().await.expect("status command round-trips");
        let after_status = match tokio::time::timeout(Duration::from_secs(20), lines.recv())
            .await
            .expect("status triggers a fresh state line")
            .expect("state channel stays open")
        {
            StateLine::State(state) => state,
            StateLine::Position(_) => panic!("status re-emits a full state"),
            StateLine::Disconnected => panic!("engine disconnected before the status state"),
        };
        assert_eq!(after_status.auth_state, first.auth_state);

        // Browse without a session fails cleanly through the reply channel.
        let error = client.browse_playlists(10).await;
        assert!(error.is_err(), "browse without a session must fail cleanly");

        client.shutdown_engine();
        let _ = std::fs::remove_dir_all(&state_dir);
    }
}
