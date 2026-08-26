use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_core::cache::Cache;
use librespot_core::SpotifyUri;
use librespot_metadata::Metadata;
use librespot_playback::mixer::{softmixer::SoftMixer, Mixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::audio::AudioSignal;
use crate::auth::{
    complete_oauth, connect_cached, create_playback, percent_to_volume, prepare_oauth, PendingAuth,
    PlaybackHandles,
};
use crate::customization::{validate_definition, EditTimeline, TrackEditStore};
use crate::history::ListeningHistory;
use crate::io::ProtocolWriter;
use serde::Serialize;
use spotify_playback_engine::protocol::{
    AuthState, BrowseResponse, Command, HistoryItem, LoopRange, PositionEvent, RepeatMode,
    Response, StateEvent, TimeRange, TrackEdit, TrackEditDefinition, TrackEditStatus, TrackRef,
};
/// Pressing previous within this many milliseconds of a track start restarts
/// the current track instead of switching tracks. Mirrors the UI's optimistic
/// restart window (OnPrevious in app.cpp) so both sides agree.
const PREVIOUS_RESTART_THRESHOLD_MS: u32 = 3_000;
/// Minimum spacing between command-driven track changes (queue replacement,
/// queue index changes, Next, and Previous). Loading an uncached track fetches
/// its audio key from Spotify's key service, which rate-limits bursts ("Unable to load
/// key, continuing without decryption") and playback dies with decoder
/// errors. 250 ms bounds key requests to roughly 4-8/s while keeping rapid
/// next/prev responsive; presses are delayed, never dropped. Natural
/// end-of-track advances are not paced.
const TRACK_CHANGE_MIN_INTERVAL: Duration = Duration::from_millis(250);
/// Failures from a rapid load burst are commonly key-service or network
/// transient errors, not evidence that every involved cache entry is bad.
/// Keep the burst window long enough to cover several paced changes.
const TRACK_CHANGE_BURST_WINDOW: Duration = Duration::from_secs(2);
const TRACK_CHANGE_BURST_MINIMUM: usize = 2;
/// A second current-track load failure within this window is treated as part
/// of the same transient burst. The first failure in a quiet period remains
/// eligible for cache cleanup.
///
/// This is sized against how far apart *failures* land, not how far apart
/// clicks land, and the two are nothing alike. A failing load takes seconds to
/// give up: the audio key times out, librespot falls back to downloading, and
/// the decoder waits out its own deadline. Measured across a real dead-session
/// episode, consecutive `Unavailable` events arrived 6.5 s and 8.9 s apart —
/// so the 2 s window this started at pruned itself empty between every pair,
/// classified each failure as isolated, and evicted the cache for every track
/// it touched. The window has to outlast the failure, not the gesture.
const UNAVAILABLE_BURST_WINDOW: Duration = Duration::from_secs(30);

/// How long to wait before the first automatic reconnect after the session
/// dies, and the ceiling the wait doubles up to while reconnects keep failing.
///
/// librespot invalidates its own `Session` when the access-point connection
/// drops (`session.rs`: the sender task's error arm calls `shutdown()`), which
/// closes the channel manager every audio-key request travels over. Metadata
/// does not go that way — `spclient` is plain HTTPS with its own pool — so the
/// library, search and browse all keep working while playback is dead, and
/// nothing surfaces the problem except tracks refusing to start. Nothing here
/// used to notice, so the engine held the corpse until it was restarted.
///
/// The first attempt is quick because the common cause is a transient drop the
/// reconnect will simply fix. The ceiling exists so a genuine outage is not
/// hammered at heartbeat rate.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

pub struct Engine {
    writer: ProtocolWriter,
    cache: Cache,
    temporary_directory: std::path::PathBuf,
    /// The app-owned `credentials.json` inside the cache, removed by
    /// `logout`. Kept separately because librespot's `Cache` offers no
    /// credential-removal API.
    credentials_file: std::path::PathBuf,
    track_edits: TrackEditStore,
    state: PlaybackState,
    player: Option<Arc<Player>>,
    mixer: Option<Arc<SoftMixer>>,
    session: Option<librespot_core::Session>,
    play_request_id: Option<u64>,
    /// Set when librespot reports `Unavailable` for the current load. The
    /// player remains in librespot's failed `Loading` state unless it is
    /// explicitly stopped; this flag makes the next Play command issue a
    /// fresh load instead of sending `play` to a dead loader.
    loading_failed: bool,
    /// The queue/playhead is installed but no librespot load exists. This is
    /// set by restore and by session teardown, and consumed by the next Play.
    current_needs_load: bool,
    /// Whether the installed queue is a draft editor preview. Preview player
    /// events still update transport state, but never create listening-history
    /// rows.
    preview_mode: bool,
    /// Process-local owner of the installed editor preview. Zero means that
    /// no preview lease is active.
    preview_lease_id: u64,
    /// Playback intent captured when an authenticated session dies. A normal
    /// app startup restore never sets this; only an unexpected reconnect may
    /// resume automatically.
    resume_after_reconnect: bool,
    position_anchor: Option<(u32, Instant)>,
    /// When the last command-driven track change (PlayQueue/Next/Previous)
    /// was dispatched, for pacing rapid presses (see
    /// [`TRACK_CHANGE_MIN_INTERVAL`]). `None` until the first change.
    last_track_change: Option<Instant>,
    /// Actual current-track load starts seen recently. This complements the
    /// 250 ms command pacing: when a failure arrives after a rapid sequence
    /// of loads, cache eviction is unsafe because the failure may be a
    /// clustered key/network rejection.
    recent_track_changes: VecDeque<Instant>,
    /// Current-track load failures seen recently. A later failure in the same
    /// window is considered transient even when the user did not change
    /// tracks between the failures.
    recent_unavailable: VecDeque<Instant>,
    /// Earliest time an automatic reconnect may be attempted, and how long to
    /// wait after the next failure. `None` means "no reconnect is pending":
    /// the session is healthy, or one is already running. See
    /// [`Engine::tick_session_health`].
    next_reconnect: Option<Instant>,
    reconnect_backoff: Duration,
    shuffle_pool: Vec<usize>,
    /// Queue navigation history used by Previous; local listening rows live
    /// in `listening_history`.
    history: Vec<usize>,
    listening_history: ListeningHistory,
    random_state: u64,
    generation: u64,
    /// A seek is mid-transition (the player was paused by [`Engine::seek_source`]).
    /// While set, the transient pause event from the seek is suppressed, and
    /// a stale Playing event cannot override the seek's requested play/pause
    /// intent.
    seek_in_flight: bool,
    /// Whether the seek that set `seek_in_flight` was issued while playing.
    seek_should_play: bool,
    /// The customization revision expected by loop-boundary signals. Markers
    /// can remain queued in the audio callback after a track/config change, so
    /// delivery must be tied to the pipeline that produced them.
    audio_revision: u64,
    /// The decoder reached physical EOF while a loop boundary was still
    /// draining through the output queue. `Player::seek` is invalid in
    /// librespot's EndOfTrack state, so the audible marker reloads the same
    /// track instead.
    loop_decoder_eof: bool,
    /// The audible marker has requested a loop jump but librespot has not yet
    /// confirmed the new load/position. If EOF wins that race, reload instead
    /// of waiting for a marker that was already consumed.
    loop_jump_pending: bool,
    /// One-based audible pass through the current finite loop. Fresh loads
    /// and user seeks derive it from their source position: pass one before
    /// the loop end, and the final pass at or after it. Internal loop jumps
    /// preserve and increment this value.
    loop_pass: u32,
    auth_running: bool,
    /// Track-gain volume normalisation (attenuation-only, see
    /// `auth::player_config`). Shared with in-flight authentication so it can
    /// build the latest preference without reconnecting the session.
    normalisation: Arc<AtomicBool>,
    /// The prepared OAuth attempt whose authorize URL is published in
    /// `needs_login` state; `login` consumes it so the UI opens exactly the
    /// URL the flow listens for. Regenerated per attempt.
    pending_auth: Option<PendingAuth>,
}

struct PlaybackState {
    ready: bool,
    auth_state: AuthState,
    /// OAuth authorize URL for the current/next login attempt; present in
    /// `needs_login` (and `authenticating`) state events. See
    /// [`StateEvent::auth_url`].
    auth_url: Option<String>,
    playing: bool,
    position_ms: u32,
    duration_ms: u32,
    volume: u8,
    shuffle: bool,
    repeat: RepeatMode,
    playback_speed: f32,
    current_index: Option<usize>,
    queue: Vec<TrackRef>,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum PositionSpace {
    Source,
    Transport,
}

pub enum AuthSignal {
    Complete {
        generation: u64,
        result: Result<PlaybackHandles, String>,
    },
    PlayerRebuilt {
        generation: u64,
        normalisation: bool,
        result: Result<PlaybackHandles, String>,
    },
}

pub enum PlayerSignal {
    Event { generation: u64, event: PlayerEvent },
    Closed { generation: u64 },
}

impl Engine {
    pub fn new(
        writer: ProtocolWriter,
        cache: Cache,
        temporary_directory: std::path::PathBuf,
        credentials_file: std::path::PathBuf,
        state_directory: std::path::PathBuf,
        normalisation: bool,
    ) -> Self {
        let history_root = state_directory.clone();
        let track_edits = TrackEditStore::load_or_empty(&state_directory);
        let random_state = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15)
            ^ u64::from(std::process::id());
        Self {
            writer,
            cache,
            temporary_directory,
            credentials_file,
            track_edits,
            state: PlaybackState {
                ready: false,
                auth_state: AuthState::Authenticating,
                auth_url: None,
                playing: false,
                position_ms: 0,
                duration_ms: 0,
                volume: 50,
                shuffle: false,
                repeat: RepeatMode::Off,
                playback_speed: 1.0,
                current_index: None,
                queue: Vec::new(),
                error: None,
            },
            player: None,
            mixer: None,
            session: None,
            play_request_id: None,
            loading_failed: false,
            current_needs_load: false,
            preview_mode: false,
            preview_lease_id: 0,
            resume_after_reconnect: false,
            position_anchor: None,
            last_track_change: None,
            recent_track_changes: VecDeque::new(),
            recent_unavailable: VecDeque::new(),
            next_reconnect: None,
            reconnect_backoff: RECONNECT_BACKOFF_MIN,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            listening_history: ListeningHistory::new(history_root),
            random_state,
            generation: 0,
            seek_in_flight: false,
            seek_should_play: false,
            // No marker produced before this engine was constructed belongs
            // to its first queue, even when the process-wide audio revision
            // was already advanced by an earlier player instance.
            audio_revision: crate::audio::customization_revision().wrapping_add(1),
            loop_decoder_eof: false,
            loop_jump_pending: false,
            loop_pass: 1,
            auth_running: false,
            normalisation: Arc::new(AtomicBool::new(normalisation)),
            pending_auth: None,
        }
    }

    pub fn history(&self) -> Result<Vec<HistoryItem>, String> {
        self.listening_history.snapshot()
    }

    pub fn clear_history(&mut self) -> Result<bool, String> {
        self.listening_history.clear()?;
        Ok(true)
    }
    pub fn writer(&self) -> &ProtocolWriter {
        &self.writer
    }
    pub fn track_edit_status(&self, track_id: &str, playlist_id: Option<&str>) -> TrackEditStatus {
        self.track_edits.status(track_id, playlist_id)
    }

    pub fn save_track_edit(
        &mut self,
        track_id: String,
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
    ) -> Result<TrackEditDefinition, String> {
        self.track_edits
            .save_definition(track_id, duration_ms, cuts, loop_range)
    }

    pub fn delete_track_edit(&mut self, track_id: &str) -> Result<(), String> {
        self.track_edits.delete_definition(track_id)
    }

    pub fn set_playlist_track_edit_enabled(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.track_edits.set_enabled(playlist_id, track_id, enabled)
    }

    pub fn emit_state(&self) -> Result<(), String> {
        let current_uri = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .map(|track| track.uri.as_str());
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        self.writer.send(&StateEvent {
            kind: "state",
            ready: self.state.ready,
            auth_state: self.state.auth_state,
            auth_url: self.state.auth_url.as_deref(),
            playing: self.state.playing,
            preview: self.preview_mode,
            username: self.session.as_ref().map(|session| session.username()),
            position_ms,
            duration_ms,
            volume: self.state.volume,
            shuffle: self.state.shuffle,
            repeat: self.state.repeat,
            playback_speed: self.state.playback_speed,
            current_index: self.state.current_index,
            current_uri,
            queue: &self.state.queue,
            error: self.state.error.as_deref(),
        })
    }

    /// Serializes only the playhead scalars the frontend projects and clamps
    /// against — never the queue — for the 2-second position heartbeat.
    /// [`Engine::emit_state`] stays reserved for real changes (track, queue,
    /// volume, shuffle, repeat, duration, play/pause), so the steady-state
    /// heartbeat cost is O(1) in queue length. The heartbeat is emitted only
    /// while playing: paused positions are static and project from the last
    /// full state.
    pub fn emit_position(&self) -> Result<(), String> {
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        self.writer.send(&PositionEvent {
            kind: "position",
            position_ms,
            duration_ms,
        })
    }

    fn current_timeline(&self) -> EditTimeline<'_> {
        let cuts = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.as_ref())
            .map(|edit| edit.cuts.as_slice())
            .unwrap_or_default();
        EditTimeline::new(self.state.duration_ms, cuts)
    }

    fn transport_position_and_duration(&self) -> (u32, u32) {
        let timeline = self.current_timeline();
        (
            timeline.source_to_compiled(self.state.position_ms),
            timeline.compiled_duration_ms(),
        )
    }

    fn transport_to_source(&self, position_ms: u32) -> u32 {
        self.current_timeline().compiled_to_source(position_ms)
    }

    fn update_transport_position(&mut self, position_ms: u32) {
        let source_position_ms = self.transport_to_source(position_ms);
        self.update_position(source_position_ms);
    }

    /// Records an authoritative playback position and re-anchors the drift
    /// projection at the current wall clock. Called on every player event and
    /// command that establishes a position (track change, play/pause, seek,
    /// position correction) so `tick_position` projects from the newest truth.
    fn update_position(&mut self, position_ms: u32) {
        self.state.position_ms = position_ms.min(self.state.duration_ms);
        self.position_anchor = Some((self.state.position_ms, Instant::now()));
    }

    /// Advances the reported position from the latest anchor and reports
    /// whether a position heartbeat should be emitted (at most once per
    /// call). While paused the position is static and no heartbeat is
    /// produced: the frontend projects the frozen position from the last
    /// full state.
    pub fn tick_position(&mut self) -> bool {
        if !self.state.playing {
            return false;
        }
        let Some((anchor_position_ms, anchor_time)) = self.position_anchor else {
            return false;
        };
        let elapsed_ms =
            (anchor_time.elapsed().as_secs_f64() * 1_000.0 * f64::from(self.state.playback_speed))
                .round()
                .min(f64::from(u32::MAX)) as u32;
        let source_position_ms = {
            let timeline = self.current_timeline();
            let compiled_anchor_ms = timeline.source_to_compiled(anchor_position_ms);
            let compiled_position_ms = compiled_anchor_ms
                .saturating_add(elapsed_ms)
                .min(timeline.compiled_duration_ms());
            timeline.compiled_to_source(compiled_position_ms)
        };
        self.state.position_ms = source_position_ms;
        true
    }

    /// Re-checks the download mark on the only tracks that can have changed.
    ///
    /// A track's audio reaches the cache because librespot streamed it, so the
    /// set that can newly become cached while you are looking at a list is the
    /// one playing and the one queued behind it — not the other two hundred
    /// rows. That is what makes this affordable on a heartbeat: two lookups,
    /// each a path join and one file-attribute call, rather than a walk of the
    /// queue or a directory scan.
    ///
    /// Returns whether anything changed, so a quiet tick still emits nothing.
    pub fn refresh_cached_marks(&mut self) -> bool {
        let Some(current) = self.state.current_index else {
            return false;
        };
        let ids: Vec<String> = [current, current + 1]
            .into_iter()
            .filter_map(|index| self.state.queue.get(index))
            .filter(|track| !track.cached)
            .map(|track| track.id.clone())
            .collect();
        if ids.is_empty() {
            return false;
        }
        let now_cached = crate::browse::cached_track_ids(&ids, Some(&self.cache));
        if now_cached.is_empty() {
            return false;
        }
        let mut changed = false;
        for track in &mut self.state.queue {
            if !track.cached && now_cached.contains(&track.id) {
                track.cached = true;
                changed = true;
            }
        }
        changed
    }

    /// Notices a session librespot has invalidated underneath us and rebuilds
    /// it, with backoff. Driven from the same heartbeat that advances the
    /// playhead, so no extra timer is needed; the check is one `RwLock` read.
    ///
    /// Returns whether the engine's state changed and should be emitted.
    ///
    /// The failure this recovers from is silent by construction — see
    /// [`RECONNECT_BACKOFF_MIN`]. Reconnecting also re-arms `Ready`, so the
    /// frontend's existing "not ready" handling covers the gap without needing
    /// to know a reconnect happened.
    pub fn tick_session_health(&mut self, sender: &mpsc::UnboundedSender<AuthSignal>) -> bool {
        if self.auth_running {
            return false;
        }
        let dead = self
            .session
            .as_ref()
            .is_some_and(librespot_core::Session::is_invalid);
        if !dead {
            // A healthy session ends any backoff a previous outage built up.
            self.next_reconnect = None;
            self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
            return false;
        }

        let now = Instant::now();
        let Some(due) = self.next_reconnect else {
            // First tick that sees the corpse: schedule, and tell the frontend
            // playback is down rather than letting loads fail one by one.
            self.next_reconnect = Some(now + self.reconnect_backoff);
            self.resume_after_reconnect = self.state.playing;
            self.state.ready = false;
            self.state.playing = false;
            // The active history timer is wall-clock based. Stop it at the
            // first observed disconnect so reconnect backoff is not counted as
            // audible listening before shutdown_playback finalizes the row.
            self.pause_listening();
            self.state.error = Some("the Spotify connection dropped; reconnecting".to_owned());
            eprintln!("Spotify session went invalid; reconnecting");
            return true;
        };
        if now < due {
            return false;
        }

        self.reconnect_backoff = (self.reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
        self.next_reconnect = Some(now + self.reconnect_backoff);
        // `start_authentication` tears down the dead player and session, bumps
        // the generation so their in-flight events are ignored, and reconnects
        // from cached credentials.
        self.start_authentication(sender.clone());
        true
    }

    /// Enables or disables track-gain volume normalisation.
    ///
    /// A live change constructs only a new player against the existing
    /// authenticated session. Authentication in flight shares the atomic
    /// preference and therefore builds its first player with the latest value.
    pub fn set_normalisation(
        &mut self,
        enabled: bool,
        sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> bool {
        if self.normalisation.swap(enabled, Ordering::AcqRel) == enabled {
            return false;
        }
        let Some(session) = self.session.clone() else {
            return true;
        };
        let cache = self.cache.clone();
        let sender = sender.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let result = create_playback(session, cache, enabled).await;
            let _ = sender.send(AuthSignal::PlayerRebuilt {
                generation,
                normalisation: enabled,
                result,
            });
        });
        true
    }

    pub fn start_authentication(&mut self, sender: mpsc::UnboundedSender<AuthSignal>) {
        if self.auth_running {
            return;
        }
        self.shutdown_playback();
        self.auth_running = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.state.ready = false;
        self.state.auth_state = AuthState::Authenticating;
        self.state.playing = false;
        self.state.error = None;
        if self.cache.credentials().is_none() {
            // No cached credentials: wait for an explicit login command so the
            // UI can present the authorize URL (a browser flow must never be
            // started behind the user's back or left waiting unattended).
            self.auth_running = false;
            self.enter_needs_login();
            return;
        }
        let cache = self.cache.clone();
        let temporary_directory = self.temporary_directory.clone();
        let normalisation = Arc::clone(&self.normalisation);
        tokio::spawn(async move {
            let result = connect_cached(cache, temporary_directory, normalisation).await;
            let _ = sender.send(AuthSignal::Complete { generation, result });
        });
    }

    /// Transitions to `NeedsLogin`: tears down playback, clears the playback
    /// state, and prepares a fresh OAuth attempt whose authorize URL is
    /// published in state events for the UI's Log in button. The URL is
    /// regenerated every time the engine enters this state.
    fn enter_needs_login(&mut self) {
        self.shutdown_playback();
        self.state.ready = false;
        self.state.auth_state = AuthState::NeedsLogin;
        self.state.playing = false;
        self.state.position_ms = 0;
        self.state.duration_ms = 0;
        self.state.current_index = None;
        self.state.queue.clear();
        self.current_needs_load = false;
        self.preview_mode = false;
        self.preview_lease_id = 0;
        self.resume_after_reconnect = false;
        self.shuffle_pool.clear();
        self.history.clear();
        self.state.error = None;
        match prepare_oauth() {
            Ok(pending) => {
                self.state.auth_url = Some(pending.auth_url.clone());
                self.pending_auth = Some(pending);
            }
            Err(error) => {
                self.state.auth_url = None;
                self.state.error = Some(error);
            }
        }
    }

    /// Starts the OAuth flow on demand, consuming the prepared attempt so the
    /// flow listens for exactly the authorize URL the UI opened. No-op while a
    /// session is live (`Ready`) or a flow is already running.
    pub fn login(
        &mut self,
        auth_sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> Result<bool, String> {
        if self.state.auth_state == AuthState::Ready {
            return Ok(true);
        }
        if self.auth_running {
            return Ok(true);
        }
        let pending = self.begin_login_flow()?;
        let generation = self.generation;
        let auth_sender = auth_sender.clone();
        let cache = self.cache.clone();
        let temporary_directory = self.temporary_directory.clone();
        let normalisation = Arc::clone(&self.normalisation);
        tokio::spawn(async move {
            let result = complete_oauth(cache, temporary_directory, pending, normalisation).await;
            let _ = auth_sender.send(AuthSignal::Complete { generation, result });
        });
        Ok(true)
    }

    /// State half of [`Engine::login`]: consumes (or prepares) the OAuth
    /// attempt and marks the engine `Authenticating`. Split out so the
    /// transition is unit-testable without spawning a live flow.
    fn begin_login_flow(&mut self) -> Result<PendingAuth, String> {
        let pending = match self.pending_auth.take() {
            Some(pending) => pending,
            None => prepare_oauth()?,
        };
        self.shutdown_playback();
        self.auth_running = true;
        self.generation = self.generation.wrapping_add(1);
        self.state.ready = false;
        self.state.auth_state = AuthState::Authenticating;
        self.state.playing = false;
        self.state.error = None;
        self.state.auth_url = Some(pending.auth_url.clone());
        Ok(pending)
    }

    /// Clears the cached credentials and tears the session down; the state
    /// flips to `NeedsLogin` with a fresh authorize URL so re-login works
    /// without a restart. Idempotent: safe when no session or credentials
    /// exist.
    pub fn logout(&mut self) -> Result<bool, String> {
        // Invalidate any in-flight authentication attempt: its completion
        // signal must not resurrect a session after an explicit logout.
        self.auth_running = false;
        self.generation = self.generation.wrapping_add(1);
        crate::browse::clear_canvas_cache();
        self.enter_needs_login();
        if let Err(error) = self.clear_cached_credentials() {
            eprintln!("could not clear cached Spotify credentials: {error}");
            self.state.error = Some(format!("could not clear cached credentials: {error}"));
        }
        Ok(true)
    }

    /// Removes the app-owned `credentials.json`; a missing file is already
    /// logged out. librespot's `Cache` has no removal API, so the file is
    /// removed directly.
    fn clear_cached_credentials(&self) -> Result<(), String> {
        let path = &self.credentials_file;
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn on_auth_signal(
        &mut self,
        signal: AuthSignal,
        player_sender: mpsc::UnboundedSender<PlayerSignal>,
    ) -> bool {
        match signal {
            AuthSignal::Complete { generation, result } => {
                if generation != self.generation {
                    return false;
                }
                self.auth_running = false;
                match result {
                    Ok(handles) => {
                        let had_queue = self.state.current_index.is_some();
                        let restored_volume = self.state.volume;
                        let resume = had_queue && self.resume_after_reconnect;
                        self.resume_after_reconnect = false;
                        self.state.ready = true;
                        self.state.auth_state = AuthState::Ready;
                        self.state.volume = if had_queue {
                            restored_volume
                        } else {
                            handles.volume_percent
                        };
                        self.state.error = None;
                        self.state.auth_url = None;
                        self.pending_auth = None;
                        self.player = Some(handles.player);
                        self.mixer = Some(handles.mixer);
                        self.session = Some(handles.session);
                        if had_queue {
                            let volume = percent_to_volume(restored_volume);
                            if let Some(mixer) = &self.mixer {
                                mixer.set_volume(volume);
                            }
                            crate::audio::set_sink_volume(volume);
                            self.current_needs_load = true;
                            self.state.playing = resume;
                            if resume {
                                if let Err(error) = self.load_current(true) {
                                    self.state.playing = false;
                                    self.state.error = Some(error);
                                }
                            }
                        }
                        self.next_reconnect = None;
                        self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
                        Self::forward_player_events(handles.events, generation, player_sender);
                        true
                    }
                    Err(error) => {
                        eprintln!("Spotify playback authentication failed: {error}");
                        self.enter_needs_login();
                        self.state.error = Some(error);
                        true
                    }
                }
            }
            AuthSignal::PlayerRebuilt {
                generation,
                normalisation,
                result,
            } => {
                if generation != self.generation
                    || self.normalisation.load(Ordering::Acquire) != normalisation
                {
                    return false;
                }
                let handles = match result {
                    Ok(handles) => handles,
                    Err(error) => {
                        self.state.error =
                            Some(format!("could not rebuild the audio player: {error}"));
                        return true;
                    }
                };

                let was_playing = self.state.playing;
                if let Some(player) = self.player.take() {
                    player.stop();
                }
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                let volume = percent_to_volume(self.state.volume);
                handles.mixer.set_volume(volume);
                crate::audio::set_sink_volume(volume);
                self.player = Some(handles.player);
                self.mixer = Some(handles.mixer);
                self.current_needs_load = self.state.current_index.is_some();
                self.play_request_id = None;
                self.state.error = None;
                if was_playing {
                    if let Err(error) = self.load_current(true) {
                        self.state.playing = false;
                        self.state.error = Some(error);
                    }
                }
                Self::forward_player_events(handles.events, generation, player_sender);
                true
            }
        }
    }

    fn forward_player_events(
        mut events: librespot_playback::player::PlayerEventChannel,
        generation: u64,
        player_sender: mpsc::UnboundedSender<PlayerSignal>,
    ) {
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if player_sender
                    .send(PlayerSignal::Event { generation, event })
                    .is_err()
                {
                    return;
                }
            }
            let _ = player_sender.send(PlayerSignal::Closed { generation });
        });
    }

    /// Removes timestamps older than a burst window without treating a clock
    /// adjustment backwards as an expiration.
    ///
    /// The actual pacing contract is documented on [`Self::pace_track_change`].
    fn prune_recent_times(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        loop {
            let expired = match times.front() {
                Some(when) => now
                    .checked_duration_since(*when)
                    .is_some_and(|elapsed| elapsed > window),
                None => false,
            };
            if !expired {
                break;
            }
            times.pop_front();
        }
    }

    /// Records a real player load rather than merely a button press. Keeping
    /// this history lets the first failure in an already-observable rapid
    /// load burst avoid destructive cache cleanup.
    fn note_track_change(&mut self, now: Instant) {
        Self::prune_recent_times(
            &mut self.recent_track_changes,
            now,
            TRACK_CHANGE_BURST_WINDOW,
        );
        self.recent_track_changes.push_back(now);
    }

    /// Returns whether this failure belongs to a transient burst. A second
    /// failure within the failure window is clustered even when the same
    /// track was retried; two or more recent load starts also protect the
    /// first failure observed after rapid clicks.
    fn unavailable_is_clustered(&mut self, now: Instant) -> bool {
        Self::prune_recent_times(
            &mut self.recent_track_changes,
            now,
            TRACK_CHANGE_BURST_WINDOW,
        );
        Self::prune_recent_times(&mut self.recent_unavailable, now, UNAVAILABLE_BURST_WINDOW);
        let clustered = self.recent_unavailable.len() >= 1
            || self.recent_track_changes.len() >= TRACK_CHANGE_BURST_MINIMUM;
        self.recent_unavailable.push_back(now);
        clustered
    }

    /// Successful playback ends the current failure burst. A later isolated
    /// failure should again be eligible for corrupt-cache cleanup.
    fn clear_unavailable_burst(&mut self) {
        self.recent_unavailable.clear();
    }

    /// Waits out any remaining [`TRACK_CHANGE_MIN_INTERVAL`] since the last
    /// command-driven track change, then re-arms the interval. Called before
    /// dispatching PlayQueue/Next/Previous so rapid presses advance at a
    /// bounded rate instead of bursting audio-key requests at Spotify's key
    /// service. Delays the command loop briefly; presses are never dropped.
    async fn pace_track_change(&mut self) {
        let wait = track_change_wait(
            self.last_track_change,
            Instant::now(),
            TRACK_CHANGE_MIN_INTERVAL,
        );
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.last_track_change = Some(Instant::now());
    }

    pub async fn process_command(
        &mut self,
        command: Command,
        auth_sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> Result<bool, String> {
        if matches!(&command, Command::Status) {
            if self.state.auth_state == AuthState::Error {
                self.start_authentication(auth_sender.clone());
            }
            return Ok(true);
        }
        if matches!(&command, Command::GetHistory) {
            return Ok(true);
        }
        if matches!(&command, Command::ClearHistory) {
            return self.clear_history();
        }
        if matches!(&command, Command::Login) {
            return self.login(auth_sender);
        }
        if matches!(&command, Command::Logout) {
            return self.logout();
        }
        if let Command::SetNormalisation { enabled } = command {
            return Ok(self.set_normalisation(enabled, auth_sender));
        }
        // A guarded editor teardown is allowed to lose a race with a real
        // queue command even while the player is unavailable. Treat that
        // stale request as an unchanged no-op before readiness checks. The
        // lease must be nonzero and still own the installed preview.
        if let Command::RestoreQueue {
            only_if_preview: true,
            preview_lease_id,
            ..
        } = &command
        {
            if !self.preview_mode
                || *preview_lease_id == 0
                || *preview_lease_id != self.preview_lease_id
            {
                return Ok(false);
            }
        }
        self.ensure_ready()?;
        // Pace command-driven track changes so rapid next/prev spam cannot
        // burst audio-key requests (each load of an uncached track fetches
        // its decryption key; the key service rate-limits bursts and
        // playback dies). Only PlayQueue/Next/Previous are paced; other
        // commands may wait a tick — no press is dropped.
        if matches!(
            &command,
            Command::PlayQueue { .. }
                | Command::PreviewTrackEdit { .. }
                | Command::PlayQueueIndex { .. }
                | Command::Next
                | Command::Previous
        ) {
            self.pace_track_change().await;
        }
        match command {
            Command::Status
            | Command::GetHistory
            | Command::ClearHistory
            | Command::Shutdown
            | Command::Login
            | Command::Logout
            | Command::SetNormalisation { .. }
            | Command::BrowsePlaylists { .. }
            | Command::BrowsePlaylist { .. }
            | Command::BrowseRadio { .. }
            | Command::BrowsePlaylistRecommendations { .. }
            | Command::BrowseAlbum { .. }
            | Command::BrowseArtist { .. }
            | Command::BrowseArtistReleases { .. }
            | Command::BrowseArtistCatalogue { .. }
            | Command::BrowseLikedSongs { .. }
            | Command::BrowseLikedUris { .. }
            | Command::BrowseSearch { .. }
            | Command::BrowseTrackCredits { .. }
            | Command::BrowseCanvas { .. }
            | Command::GetTrackWaveform { .. }
            | Command::CancelTrackWaveform { .. }
            | Command::GetTrackEdit { .. }
            | Command::SaveTrackEdit { .. }
            | Command::DeleteTrackEdit { .. }
            | Command::SetPlaylistTrackEditEnabled { .. }
            | Command::EditCreatePlaylist { .. }
            | Command::EditRenamePlaylist { .. }
            | Command::EditDeletePlaylist { .. }
            | Command::EditAddPlaylistTracks { .. }
            | Command::EditRemovePlaylistTracks { .. }
            | Command::EditReorderPlaylistTracks { .. } => unreachable!(),
            Command::PlayQueue {
                queue,
                index,
                position_ms,
                context,
            } => self.play_queue(queue, index, position_ms, context),
            Command::RestoreQueue {
                queue,
                index,
                position_ms,
                context,
                preview_lease_id,
                only_if_preview,
                resume_playing,
            } => self.restore_queue(
                queue,
                index,
                position_ms,
                context,
                preview_lease_id,
                only_if_preview,
                resume_playing,
            ),
            Command::PreviewTrackEdit {
                track,
                cuts,
                loop_range,
                position_ms,
                preview_lease_id,
            } => self.preview_track_edit(track, cuts, loop_range, position_ms, preview_lease_id),
            Command::PlayQueueIndex { index } => self.play_queue_index(index),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Next => self.advance(false),
            Command::Previous => self.previous(),
            Command::Seek { position_ms } => self.seek_transport(position_ms),
            Command::SetVolume { percent } => self.set_volume(percent),
            Command::SetShuffle { enabled } => self.set_shuffle(enabled),
            Command::SetRepeat { mode } => self.set_repeat(mode),
            Command::SetPlaybackSpeed { speed } => self.set_playback_speed(speed),
            Command::AddQueue { track, context } => self.add_queue(track, context),
            Command::AddQueueBatch { tracks, context } => self.add_queue_batch(tracks, context),
            Command::RemoveQueue { index } => self.remove_queue(index),
            Command::MoveQueue { from, to } => self.move_queue(from, to),
        }
    }

    pub fn send_response(
        &self,
        request_id: &str,
        result: &Result<bool, String>,
    ) -> Result<(), String> {
        self.writer.send(&Response {
            kind: "response",
            request_id,
            ok: result.is_ok(),
            error: result.as_ref().err().map(String::as_str),
        })
    }

    /// Sends a typed `browse_*` response: `data` carries the payload on
    /// success, error text only on failure. `kind` must match the command
    /// name so the UI can route the response.
    pub fn send_browse_response<T: Serialize>(
        &self,
        request_id: &str,
        kind: &'static str,
        result: &Result<T, String>,
    ) -> Result<(), String> {
        let (ok, error, data) = match result {
            Ok(data) => (true, None, Some(data)),
            Err(error) => (false, Some(error.as_str()), None),
        };
        self.writer.send(&BrowseResponse {
            kind,
            request_id,
            ok,
            error,
            data,
        })
    }

    /// Sends an `edit_*` response for a void edit: `ok`/`error` only, with
    /// no `data` payload on success (the UI routes these like browse
    /// responses but has nothing to parse).
    pub fn send_edit_response(
        &self,
        request_id: &str,
        kind: &'static str,
        result: &Result<(), String>,
    ) -> Result<(), String> {
        let (ok, error) = match result {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.as_str())),
        };
        self.writer.send(&BrowseResponse::<()> {
            kind,
            request_id,
            ok,
            error,
            data: None,
        })
    }

    /// An owned clone of the live session for browse/edit work: browsing
    /// only needs the authenticated session (unlike playback commands, no
    /// player yet). The clone is handed to spawned browse tasks so the
    /// command loop never blocks on network resolution.
    pub fn browse_session_clone(&self) -> Result<librespot_core::Session, String> {
        self.session
            .clone()
            .ok_or_else(|| match self.state.auth_state {
                AuthState::Authenticating => {
                    "Spotify authentication is still in progress".to_owned()
                }
                AuthState::NeedsLogin => {
                    "Spotify login is required; use the Log in button in Settings".to_owned()
                }
                AuthState::Error => self
                    .state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Spotify authentication failed".to_owned()),
                AuthState::Ready => "the Spotify session is unavailable".to_owned(),
            })
    }

    /// Best-effort eviction of a track's cached audio files, run off the
    /// command loop. A failed load can leave (or find) a corrupt/truncated
    /// cache entry — "end of stream" Symphonia failures — and evicting the
    /// entry makes the next attempt a clean refetch. All file ids the track
    /// exposes are removed (the player may have picked any format).
    fn evict_track_audio_cache(&self, track_uri: SpotifyUri) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(cache) = session.cache().cloned() else {
            return;
        };
        let uri_text = track_uri.to_uri().unwrap_or_default();
        tokio::spawn(async move {
            let Ok(parsed) = SpotifyUri::from_uri(&uri_text) else {
                return;
            };
            let Ok(track) = librespot_metadata::Track::get(&session, &parsed).await else {
                return;
            };
            for file_id in track.files.values() {
                if let Err(error) = cache.remove_file(*file_id) {
                    eprintln!("could not evict cached audio file {file_id}: {error}");
                }
            }
        });
    }

    pub fn on_player_signal(&mut self, signal: PlayerSignal) -> bool {
        match signal {
            PlayerSignal::Event { generation, event } if generation == self.generation => {
                self.on_player_event(event)
            }
            PlayerSignal::Closed { generation } if generation == self.generation => {
                if self.state.ready {
                    self.state.ready = false;
                    self.state.auth_state = AuthState::Error;
                    self.state.playing = false;
                    self.finalize_listening(false);
                    self.state.error = Some(
                        "the local audio player stopped unexpectedly; request status to retry"
                            .to_owned(),
                    );
                    self.play_request_id = None;
                    self.loading_failed = false;
                    self.last_track_change = None;
                    self.recent_track_changes.clear();
                    self.recent_unavailable.clear();
                    self.player = None;
                    self.mixer = None;
                    self.invalidate_audio_signals();
                    if let Some(session) = self.session.take() {
                        session.shutdown();
                    }
                    true
                } else {
                    false
                }
            }
            PlayerSignal::Event { .. } | PlayerSignal::Closed { .. } => false,
        }
    }

    pub fn on_audio_signal(&mut self, signal: AudioSignal) -> bool {
        match signal {
            // Deliberately not gated on `playing`: the boundary was reached,
            // and the jump is what re-arms the pipeline to emit the next one.
            AudioSignal::LoopBoundary {
                position_ms,
                revision,
            } => {
                if revision != self.audio_revision {
                    return false;
                }
                let Some(loop_range) = self.current_loop() else {
                    return false;
                };
                if loop_range.start_ms != position_ms || self.loop_pass >= loop_range.play_count {
                    return false;
                }
                self.loop_pass += 1;
                self.loop_jump_pending = true;
                let result = if self.loop_decoder_eof {
                    let start_playing = self.state.playing;
                    self.update_position(position_ms);
                    self.load_current_at_loop_pass(start_playing)
                } else {
                    self.seek_source_at_loop_pass(position_ms).map(|_| ())
                };
                if let Err(error) = result {
                    self.state.playing = false;
                    self.state.error = Some(error);
                }
                true
            }
        }
    }
    pub fn shutdown(&mut self) {
        self.auth_running = false;
        self.generation = self.generation.wrapping_add(1);
        self.shutdown_playback();
        self.state.ready = false;
        self.state.playing = false;
    }

    fn resolve_queue_edits(&self, queue: &mut [TrackRef]) {
        for track in queue {
            track.effective_edit =
                self.track_edits
                    .resolve(&track.id, track.duration_ms, &track.context);
        }
    }

    fn configure_current_audio_at_loop_pass(&mut self, position_ms: u32, loop_pass: u32) {
        let edit = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.clone());
        self.audio_revision = crate::audio::configure_customization_at_loop_pass(
            edit,
            self.state.playback_speed,
            position_ms,
            loop_pass,
        );
    }

    fn configure_current_audio_after_natural_boundary(&mut self, position_ms: u32) {
        let edit = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.clone());
        self.audio_revision = crate::audio::configure_customization_after_natural_boundary(
            edit,
            self.state.playback_speed,
            position_ms,
        );
    }
    fn current_loop(&self) -> Option<LoopRange> {
        self.state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .and_then(|track| track.effective_edit.as_ref())
            .and_then(|edit| edit.loop_range)
    }

    fn invalidate_audio_signals(&mut self) {
        self.audio_revision = self.audio_revision.wrapping_add(1);
    }

    fn loop_pass_for_position(&self, position_ms: u32) -> u32 {
        self.current_loop()
            .filter(|loop_range| position_ms >= loop_range.end_ms)
            .map_or(1, |loop_range| loop_range.play_count.max(1))
    }

    fn reset_loop_pass_for_position(&mut self, position_ms: u32) {
        self.loop_pass = self.loop_pass_for_position(position_ms);
    }

    /// A speed change rebuilds the customization at the current source
    /// position, but it is not a user seek: an internal repeated pass must
    /// survive that rebuild. A position at or beyond the loop end is always
    /// the completed pass so a missing marker cannot re-enter the loop.
    fn preserve_loop_pass_for_position(&mut self, position_ms: u32) {
        if self
            .current_loop()
            .is_some_and(|loop_range| position_ms >= loop_range.end_ms)
        {
            self.reset_loop_pass_for_position(position_ms);
        } else {
            self.loop_pass = self.loop_pass.max(1);
        }
    }

    fn current_loop_start(&self) -> Option<u32> {
        self.current_loop().map(|range| range.start_ms)
    }

    fn ensure_ready(&self) -> Result<(), String> {
        if self.state.ready && self.player.is_some() {
            Ok(())
        } else {
            Err(match self.state.auth_state {
                AuthState::Authenticating => {
                    "Spotify authentication is still in progress".to_owned()
                }
                AuthState::NeedsLogin => {
                    "Spotify login is required; use the Log in button in Settings".to_owned()
                }
                AuthState::Error => self
                    .state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Spotify authentication failed".to_owned()),
                AuthState::Ready => "the local audio player is unavailable".to_owned(),
            })
        }
    }

    fn player(&self) -> Result<&Arc<Player>, String> {
        self.player
            .as_ref()
            .ok_or_else(|| "the local audio player is unavailable".to_owned())
    }

    fn finalize_listening(&mut self, completed: bool) {
        if self.preview_mode {
            return;
        }
        let _ = self.listening_history.finalize(completed);
    }

    fn pause_listening(&mut self) {
        if !self.preview_mode {
            self.listening_history.pause();
        }
    }

    fn start_listening(&mut self, track: &TrackRef) {
        if !self.preview_mode {
            self.listening_history.start_or_resume(track);
        }
    }
    fn enter_preview_mode(&mut self) {
        if !self.preview_mode {
            self.finalize_listening(false);
        }
        self.preview_mode = true;
    }

    fn leave_preview_mode(&mut self) {
        self.preview_mode = false;
        self.preview_lease_id = 0;
        self.finalize_listening(false);
    }
    fn validate_queue(queue: &[TrackRef], index: usize) -> Result<(), String> {
        if queue.is_empty() {
            return (index == 0)
                .then_some(())
                .ok_or_else(|| "index must be zero for an empty queue".to_owned());
        }
        if index >= queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }
        for track in queue {
            parse_track_uri(track)?;
            if let Some(edit) = &track.effective_edit {
                validate_definition(&track.id, track.duration_ms, &edit.cuts, edit.loop_range)?;
            }
        }
        Ok(())
    }

    fn install_empty_or_unavailable_queue(&mut self, queue: Vec<TrackRef>) -> Result<bool, String> {
        self.finalize_listening(false);
        self.player()?.stop();
        self.invalidate_audio_signals();
        self.state.queue = queue;
        self.state.current_index = None;
        self.state.position_ms = 0;
        self.state.duration_ms = 0;
        self.state.playing = false;
        self.history.clear();
        self.shuffle_pool.clear();
        self.state.error = None;
        self.loading_failed = false;
        self.loop_pass = 1;
        self.current_needs_load = false;
        self.recent_track_changes.clear();
        self.recent_unavailable.clear();
        Ok(true)
    }

    fn play_queue(
        &mut self,
        mut queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
    ) -> Result<bool, String> {
        fill_queue_context(&mut queue, &context);
        self.resolve_queue_edits(&mut queue);
        self.play_resolved_queue(
            queue,
            index,
            position_ms,
            PositionSpace::Transport,
            false,
            0,
        )
    }

    fn preview_track_edit(
        &mut self,
        track: TrackRef,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
        position_ms: u32,
        preview_lease_id: u64,
    ) -> Result<bool, String> {
        if preview_lease_id == 0 {
            return Err("preview lease ID must be nonzero".to_owned());
        }
        // A replacement transfers ownership before draft validation. If this
        // attempt is rejected, the old preview remains restorable by the new
        // editor context rather than by the stale one.
        if self.preview_mode {
            self.preview_lease_id = preview_lease_id;
        }
        let track = with_preview_edit(track, cuts, loop_range)?;
        self.play_resolved_queue(
            vec![track],
            0,
            position_ms,
            PositionSpace::Source,
            true,
            preview_lease_id,
        )
    }
    /// Installs queue rows whose `effective_edit` values are already final.
    /// Preview uses this path so its draft is never resolved against the
    /// persisted edit store.
    fn play_resolved_queue(
        &mut self,
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        position_space: PositionSpace,
        preview: bool,
        preview_lease_id: u64,
    ) -> Result<bool, String> {
        Self::validate_queue(&queue, index)?;
        let Some(playable_index) = first_available_from(&queue, index) else {
            // Do not tear down an installed preview queue when a replacement
            // cannot stop the current player.
            if preview || self.preview_mode {
                self.player()?;
            }
            if preview {
                self.preview_lease_id = preview_lease_id;
                self.enter_preview_mode();
            } else {
                self.leave_preview_mode();
            }
            return self.install_empty_or_unavailable_queue(queue);
        };
        if preview {
            self.preview_lease_id = preview_lease_id;
            self.enter_preview_mode();
        } else {
            self.leave_preview_mode();
        }

        self.state.queue = queue;
        self.state.current_index = Some(playable_index);
        self.state.duration_ms = self.state.queue[playable_index].duration_ms;
        let position_ms = if playable_index == index {
            position_ms
        } else {
            0
        };
        match position_space {
            PositionSpace::Source => self.update_position(position_ms),
            PositionSpace::Transport => self.update_transport_position(position_ms),
        }
        self.state.playing = true;
        self.state.error = None;
        self.history.clear();
        self.rebuild_shuffle_pool();
        // The first load starts a fresh pacing window for subsequent
        // command-driven changes.
        self.last_track_change = Some(Instant::now());
        self.load_current(true)?;
        Ok(true)
    }

    /// Installs a validated queue and frozen playhead without touching the
    /// audio loader. This is the only startup path: there is no paused load
    /// whose decoder/output setup could produce an audible blip.
    fn restore_queue(
        &mut self,
        mut queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
        preview_lease_id: u64,
        only_if_preview: bool,
        resume_playing: bool,
    ) -> Result<bool, String> {
        // A preview teardown can race with a real queue command. Once the
        // latter wins, this stale restore must not even validate or resolve
        // its snapshot against the live engine state.
        if only_if_preview
            && (!self.preview_mode
                || preview_lease_id == 0
                || self.preview_lease_id != preview_lease_id)
        {
            return Ok(false);
        }

        fill_queue_context(&mut queue, &context);
        // The snapshot carries whatever edits were resolved when it was
        // written, which may since have been deleted or disabled. Every other
        // queue install re-resolves against the store; this one must too, or a
        // restart replays an edit the store no longer holds.
        self.resolve_queue_edits(&mut queue);
        // Validate every input before stopping the current player or changing
        // any engine-owned queue/playhead state. A malformed stale snapshot
        // therefore cannot damage a live preview either.
        Self::validate_queue(&queue, index)?;

        self.leave_preview_mode();
        if let Some(player) = &self.player {
            player.stop();
        }
        self.invalidate_audio_signals();
        let playable_index = if queue.is_empty() {
            None
        } else {
            first_available_wrapping(&queue, index)
        };
        self.state.queue = queue;
        self.state.current_index = playable_index;
        self.state.duration_ms = playable_index
            .map(|current| self.state.queue[current].duration_ms)
            .unwrap_or(0);
        if playable_index == Some(index) {
            self.update_transport_position(position_ms);
        } else {
            self.update_transport_position(0);
        }
        self.state.playing = false;
        self.state.error = None;
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.play_request_id = None;
        self.seek_in_flight = false;
        self.seek_should_play = false;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        self.reset_loop_pass_for_position(self.state.position_ms);
        self.loading_failed = false;
        self.current_needs_load = playable_index.is_some();
        self.recent_track_changes.clear();
        self.recent_unavailable.clear();

        if resume_playing {
            // Keep restore and resume inside one command. The command loop
            // will therefore emit one authoritative restored-playing state,
            // rather than publishing an intermediate paused restore first.
            if let Err(error) = self.play() {
                // Queue installation already left preview mode. Do not put a
                // failed resume back into the draft preview; expose the
                // installed real queue as paused with its failure visible.
                self.preview_mode = false;
                self.preview_lease_id = 0;
                self.state.playing = false;
                self.state.error = Some(error);
            }
        }
        Ok(true)
    }

    fn play_queue_index(&mut self, index: usize) -> Result<bool, String> {
        let (duration_ms, unavailable_reason) = {
            let track = self
                .state
                .queue
                .get(index)
                .ok_or_else(|| format!("queue index {index} is out of range"))?;
            (
                track.duration_ms,
                track.unavailable.then(|| {
                    track
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "this track is permanently unavailable".to_owned())
                }),
            )
        };
        if let Some(reason) = unavailable_reason {
            return Err(reason);
        }
        self.leave_preview_mode();
        if let Some(current) = self.state.current_index {
            if current != index {
                self.history.push(current);
            }
        }
        self.state.current_index = Some(index);
        self.state.duration_ms = duration_ms;
        self.update_transport_position(0);
        self.state.playing = true;
        self.state.error = None;
        self.rebuild_shuffle_pool();
        self.last_track_change = Some(Instant::now());
        self.load_current(true)?;
        Ok(true)
    }

    fn play(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        // A user play supersedes an in-flight seek transition. Keep the
        // guard until its Playing event arrives so a queued Paused event from
        // the seek cannot briefly undo the optimistic play.
        if self.seek_in_flight {
            self.seek_should_play = true;
        }
        if self.loop_decoder_eof
            || self.current_needs_load
            || self.loading_failed
            || (self.state.duration_ms > 0 && self.state.position_ms >= self.state.duration_ms)
        {
            if !self.loading_failed && !self.current_needs_load {
                self.update_transport_position(0);
            }
            self.load_current(true)?;
        } else {
            self.player()?.play();
        }
        self.state.playing = true;
        self.update_position(self.state.position_ms);
        self.state.error = None;
        Ok(true)
    }

    fn pause(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        // A user pause supersedes any in-flight seek transition: its own
        // Paused event must be delivered, not suppressed.
        self.seek_in_flight = false;
        self.seek_should_play = false;
        // `Unavailable` and a decoder EOF stop the current loader below, so
        // a later pause must not send `pause` to librespot's terminal
        // Loading/EndOfTrack state. Play will issue a fresh load when
        // requested.
        if !self.loading_failed && !self.current_needs_load && !self.loop_decoder_eof {
            self.player()?.pause();
        }
        self.state.playing = false;
        self.update_position(self.state.position_ms);
        self.pause_listening();
        self.state.error = None;
        Ok(true)
    }

    fn seek_transport(&mut self, position_ms: u32) -> Result<bool, String> {
        let source_position_ms = self.transport_to_source(position_ms);
        self.seek_source(source_position_ms)
    }

    fn seek_source(&mut self, position_ms: u32) -> Result<bool, String> {
        let position = position_ms.min(self.state.duration_ms);
        self.reset_loop_pass_for_position(position);
        self.loop_jump_pending = false;
        self.seek_source_at_loop_pass(position)
    }

    fn seek_source_at_loop_pass(&mut self, position_ms: u32) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        let position = position_ms.min(self.state.duration_ms);
        if self.loop_decoder_eof {
            let start_playing = self.state.playing;
            self.update_position(position);
            self.load_current_at_loop_pass(start_playing)?;
            self.state.error = None;
            return Ok(true);
        }
        self.configure_current_audio_at_loop_pass(position, self.loop_pass);
        if self.current_needs_load {
            self.update_position(position);
            self.state.error = None;
            return Ok(true);
        }
        if self.loading_failed {
            // The failed loader was stopped by the Unavailable handler. A
            // seek is also a valid recovery request: load at the new offset
            // while preserving the current play/pause intent.
            let start_playing = self.state.playing;
            self.update_position(position);
            self.load_current_at_loop_pass(start_playing)?;
            self.state.error = None;
            return Ok(true);
        }
        // Pause clears the rodio output queue instantly (the custom sink's
        // stop), seek while paused skips librespot's full read-ahead wait
        // (preload_data_before_playback is a no-op in the Paused state).
        // Capture the intent before pausing: a paused seek must remain paused,
        // while a playing seek resumes at the target without a UI blip.
        let was_playing = self.state.playing;
        self.seek_in_flight = true;
        self.seek_should_play = was_playing;
        let player = self.player()?;
        player.pause();
        player.seek(position);
        if was_playing {
            // Its blocking read fetches the target range itself, so the jump
            // lands in roughly one network round trip instead of a drain of
            // buffered audio plus a full 3-second-window fetch.
            player.play();
        }
        self.update_position(position);
        self.state.error = None;
        Ok(true)
    }

    fn previous(&mut self) -> Result<bool, String> {
        let current = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        if let Some(index) = self.previous_index() {
            self.leave_preview_mode();
            if self.state.shuffle && !self.shuffle_pool.contains(&current) {
                self.shuffle_pool.push(current);
            }
            self.state.current_index = Some(index);
            self.state.duration_ms = self.state.queue[index].duration_ms;
            self.update_transport_position(0);
            self.state.error = None;
            let start_playing = self.state.playing;
            self.load_current(start_playing)?;
            Ok(true)
        } else {
            // No earlier track (first track, or already restarting): seek the
            // current track back to its beginning instead of erroring.
            self.seek_transport(0)
        }
    }

    /// Decides which track `previous` should switch to. `Some(index)` names a
    /// valid queue index; `None` means "restart the current track". The
    /// position restart threshold matches the UI's 3-second restart window, so
    /// the optimistic flip and the engine agree. History entries that no longer
    /// index the queue (stale after a mutation) are dropped instead of
    /// panicking.
    fn previous_index(&mut self) -> Option<usize> {
        if self
            .current_timeline()
            .source_to_compiled(self.state.position_ms)
            > PREVIOUS_RESTART_THRESHOLD_MS
        {
            return None;
        }
        while let Some(index) = self.history.pop() {
            if self
                .state
                .queue
                .get(index)
                .is_some_and(|track| !track.unavailable)
            {
                return Some(index);
            }
        }
        let current = self.state.current_index?;
        if !self.state.shuffle {
            return (0..current)
                .rev()
                .find(|index| !self.state.queue[*index].unavailable);
        }
        None
    }

    fn advance(&mut self, at_end: bool) -> Result<bool, String> {
        if at_end {
            // Flush delayed speed/cut output before changing configuration.
            // The queue itself remains live so the tail can drain audibly.
            crate::audio::finish_natural_boundary()?;
            self.last_track_change = Some(Instant::now());
        }
        let current = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let next = self.take_next_index(at_end);
        if self.preview_mode && next.is_some_and(|index| index != current) {
            self.leave_preview_mode();
        }
        self.finalize_listening(at_end);
        match next {
            Some(index) => {
                if index != current {
                    self.history.push(current);
                }
                self.state.current_index = Some(index);
                self.state.duration_ms = self.state.queue[index].duration_ms;
                self.update_transport_position(0);
                self.state.error = None;
                if at_end {
                    self.load_current_after_natural_boundary(true)?;
                } else {
                    self.load_current(true)?;
                }
            }
            None => {
                // At natural EOF librespot is already in EndOfTrack. Calling
                // stop here would clear correctly queued audio before the
                // device drains it; explicit queue exhaustion still stops.
                if !at_end {
                    self.player()?.stop();
                    self.invalidate_audio_signals();
                }
                self.state.playing = false;
                self.update_position(self.state.duration_ms);
            }
        }
        Ok(true)
    }

    fn set_playback_speed(&mut self, speed: f32) -> Result<bool, String> {
        if !speed.is_finite() || !(0.5..=2.0).contains(&speed) {
            return Err("playback speed must be between 0.5 and 2.0".to_owned());
        }
        if self.state.playback_speed == speed {
            return Ok(false);
        }
        self.state.playback_speed = speed;
        if self.state.current_index.is_some() {
            let position = self.state.position_ms;
            self.preserve_loop_pass_for_position(position);
            self.loop_jump_pending = false;
            if self.current_needs_load {
                self.configure_current_audio_at_loop_pass(position, self.loop_pass);
            } else {
                self.seek_source_at_loop_pass(position)?;
            }
        } else {
            self.audio_revision = crate::audio::configure_customization(None, speed, 0);
        }
        Ok(true)
    }

    fn set_volume(&mut self, percent: u8) -> Result<bool, String> {
        if percent > 100 {
            return Err("volume percent must be between 0 and 100".to_owned());
        }
        if self.state.volume == percent {
            return Ok(false);
        }
        let volume = percent_to_volume(percent);
        self.mixer
            .as_ref()
            .ok_or_else(|| "the software mixer is unavailable".to_owned())?
            .set_volume(volume);
        self.cache.save_volume(volume);
        // The audible volume lives on the rodio sink (per-packet attenuation
        // is disabled); apply it there so the change is heard immediately.
        crate::audio::set_sink_volume(volume);
        self.state.volume = percent;
        self.state.error = None;
        Ok(true)
    }

    fn set_shuffle(&mut self, enabled: bool) -> Result<bool, String> {
        if self.state.shuffle == enabled {
            return Ok(false);
        }
        self.state.shuffle = enabled;
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn set_repeat(&mut self, mode: RepeatMode) -> Result<bool, String> {
        if self.state.repeat == mode {
            return Ok(false);
        }
        self.state.repeat = mode;
        self.preload_next();
        Ok(true)
    }
    fn add_queue(&mut self, mut track: TrackRef, context: String) -> Result<bool, String> {
        fill_queue_context(std::slice::from_mut(&mut track), &context);
        self.resolve_queue_edits(std::slice::from_mut(&mut track));
        parse_track_uri(&track)?;
        self.state.queue.push(track);
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn add_queue_batch(
        &mut self,
        mut tracks: Vec<TrackRef>,
        context: String,
    ) -> Result<bool, String> {
        fill_queue_context(&mut tracks, &context);
        self.resolve_queue_edits(&mut tracks);
        for track in &tracks {
            parse_track_uri(track)?;
        }
        if tracks.is_empty() {
            return Ok(true);
        }
        self.state.queue.extend(tracks);
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn remove_queue(&mut self, index: usize) -> Result<bool, String> {
        if index >= self.state.queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }
        let current = self.state.current_index;
        let was_playing = self.state.playing;
        if self.state.current_index == Some(index) {
            self.leave_preview_mode();
        }
        self.state.queue.remove(index);
        self.history.clear();
        let mut reload = false;

        match current {
            None => {}
            Some(_) if self.state.queue.is_empty() => {
                self.player()?.stop();
                self.invalidate_audio_signals();
                self.state.current_index = None;
                self.state.duration_ms = 0;
                self.state.playing = false;
                self.update_position(0);
                self.play_request_id = None;
            }
            Some(current) if index == current => {
                let start = current.min(self.state.queue.len() - 1);
                let replacement = first_available_wrapping(&self.state.queue, start);
                self.state.current_index = replacement;
                self.state.duration_ms = replacement
                    .map(|replacement| self.state.queue[replacement].duration_ms)
                    .unwrap_or(0);
                self.update_transport_position(0);
                if replacement.is_some() {
                    reload = true;
                } else {
                    self.player()?.stop();
                    self.state.playing = false;
                    self.invalidate_audio_signals();
                    self.play_request_id = None;
                    self.current_needs_load = false;
                }
            }
            Some(current) if index < current => self.state.current_index = Some(current - 1),
            Some(_) => {}
        }
        self.rebuild_shuffle_pool();
        if reload {
            self.load_current(was_playing)?;
        } else {
            self.preload_next();
        }
        Ok(true)
    }

    fn move_queue(&mut self, from: usize, to: usize) -> Result<bool, String> {
        let length = self.state.queue.len();
        if from >= length || to >= length {
            return Err(format!("queue move {from} to {to} is out of range"));
        }
        if from == to {
            return Ok(true);
        }
        let track = self.state.queue.remove(from);
        self.state.queue.insert(to, track);
        if let Some(current) = self.state.current_index {
            self.state.current_index = Some(remap_current_index_after_move(current, from, to));
        }
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn load_current(&mut self, start_playing: bool) -> Result<(), String> {
        self.reset_loop_pass_for_position(self.state.position_ms);
        self.load_current_at_loop_pass(start_playing)
    }

    fn load_current_at_loop_pass(&mut self, start_playing: bool) -> Result<(), String> {
        self.load_current_with_boundary(start_playing, false)
    }

    fn load_current_after_natural_boundary(&mut self, start_playing: bool) -> Result<(), String> {
        self.loop_pass = 1;
        self.load_current_with_boundary(start_playing, true)
    }

    fn load_current_with_boundary(
        &mut self,
        start_playing: bool,
        natural_boundary: bool,
    ) -> Result<(), String> {
        let index = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let track = self
            .state
            .queue
            .get(index)
            .ok_or_else(|| "the queue has no current track (index out of range)".to_owned())?;
        let uri = playable_track_uri(track)?;
        let position_ms = self.state.position_ms;
        if natural_boundary {
            self.configure_current_audio_after_natural_boundary(position_ms);
        } else {
            self.configure_current_audio_at_loop_pass(position_ms, self.loop_pass);
        }
        let next_uri = self
            .peek_next_index()
            .and_then(|next| playable_track_uri(&self.state.queue[next]).ok());
        self.play_request_id = None;
        self.seek_in_flight = false;
        self.seek_should_play = false;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        let player = Arc::clone(self.player()?);
        self.loading_failed = false;
        self.current_needs_load = false;
        self.note_track_change(Instant::now());
        player.load(uri, start_playing, position_ms);
        if let Some(uri) = next_uri {
            player.preload(uri);
        }
        Ok(())
    }

    fn preload_next(&self) {
        let (Some(player), Some(next)) = (&self.player, self.peek_next_index()) else {
            return;
        };
        if let Ok(uri) = parse_track_uri(&self.state.queue[next]) {
            player.preload(uri);
        }
    }

    fn take_next_index(&mut self, at_end: bool) -> Option<usize> {
        let current = self.state.current_index?;
        if at_end
            && self.state.repeat == RepeatMode::Track
            && self
                .state
                .queue
                .get(current)
                .is_some_and(|track| !track.unavailable)
        {
            return Some(current);
        }
        if self.state.shuffle {
            if self.shuffle_pool.is_empty() && self.state.repeat == RepeatMode::Context {
                self.rebuild_shuffle_pool();
            }
            while let Some(index) = self.shuffle_pool.pop() {
                if self
                    .state
                    .queue
                    .get(index)
                    .is_some_and(|track| !track.unavailable)
                {
                    return Some(index);
                }
            }
            return None;
        }
        sequential_available_index(&self.state.queue, current, self.state.repeat)
    }

    fn peek_next_index(&self) -> Option<usize> {
        let current = self.state.current_index?;
        if self.state.repeat == RepeatMode::Track
            && self
                .state
                .queue
                .get(current)
                .is_some_and(|track| !track.unavailable)
        {
            return Some(current);
        }
        if self.state.shuffle {
            return self.shuffle_pool.iter().rev().copied().find(|index| {
                self.state
                    .queue
                    .get(*index)
                    .is_some_and(|track| !track.unavailable)
            });
        }
        sequential_available_index(&self.state.queue, current, self.state.repeat)
    }

    fn rebuild_shuffle_pool(&mut self) {
        self.shuffle_pool.clear();
        if !self.state.shuffle {
            return;
        }
        let current = self.state.current_index;
        self.shuffle_pool.extend(
            (0..self.state.queue.len())
                .filter(|index| Some(*index) != current && !self.state.queue[*index].unavailable),
        );
        for index in (1..self.shuffle_pool.len()).rev() {
            let swap = (self.next_random() as usize) % (index + 1);
            self.shuffle_pool.swap(index, swap);
        }
    }

    fn next_random(&mut self) -> u64 {
        let mut value = self.random_state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.random_state = value;
        value
    }

    fn on_player_event(&mut self, event: PlayerEvent) -> bool {
        match event {
            PlayerEvent::PlayRequestIdChanged { play_request_id } => {
                self.play_request_id = Some(play_request_id);
                false
            }
            PlayerEvent::Loading {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                // A fresh load (including a retry after Unavailable) puts the
                // engine back into an in-progress state. Keep the requested
                // play/pause intent already held in `state.playing`.
                self.loading_failed = false;
                self.update_position(position_ms);
                self.loop_jump_pending = false;
                self.state.error = None;
                true
            }

            PlayerEvent::Playing {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                // A playing seek's play() completed: its transient pause is
                // over. A stale Playing event after a paused seek must not
                // turn that seek into an optimistic unpause.
                let suppress_playing = self.seek_in_flight && !self.seek_should_play;
                self.seek_in_flight = false;
                self.seek_should_play = false;
                self.loading_failed = false;
                self.loop_jump_pending = false;
                self.clear_unavailable_burst();
                if !suppress_playing {
                    if let Some(index) = self.state.current_index {
                        if let Some(track) = self.state.queue.get(index).cloned() {
                            self.start_listening(&track);
                        }
                    }
                }
                self.state.playing = !suppress_playing;
                self.update_position(position_ms);
                self.state.error = None;
                true
            }
            // The Paused produced by a seek's own pause() is transient: the
            // engine already reported the requested play/pause intent and
            // target position, so dropping it keeps the UI from blipping. A
            // user pause clears seek_in_flight in Engine::pause before its
            // event arrives, so a real pause is never swallowed.
            PlayerEvent::Paused {
                play_request_id,
                track_id,
                ..
            } if self.seek_in_flight && self.is_current_event(play_request_id, &track_id) => false,
            PlayerEvent::Paused {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.loading_failed = false;
                self.clear_unavailable_burst();
                let was_playing = self.state.playing;
                self.state.playing = false;
                if was_playing {
                    // Command-driven pause already stopped and persisted the
                    // timer optimistically. This path is for an unsolicited
                    // player/audio-device pause.
                    self.pause_listening();
                }
                self.update_position(position_ms);
                true
            }
            PlayerEvent::PositionChanged {
                play_request_id,
                track_id,
                position_ms,
            }
            | PlayerEvent::PositionCorrection {
                play_request_id,
                track_id,
                position_ms,
            }
            | PlayerEvent::Seeked {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.update_position(position_ms);
                true
            }
            PlayerEvent::EndOfTrack {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                if self
                    .current_loop()
                    .is_some_and(|loop_range| self.loop_pass < loop_range.play_count)
                {
                    if self.loop_jump_pending {
                        let position_ms = self.current_loop_start().unwrap_or(0);
                        let start_playing = self.state.playing;
                        self.update_position(position_ms);
                        if let Err(error) = self.load_current_at_loop_pass(start_playing) {
                            self.state.playing = false;
                            self.state.error = Some(error);
                        }
                        return true;
                    }
                    // The audible loop marker owns continuation. If it has not
                    // drained yet, remember that seek is no longer legal and
                    // reload the same track when the marker arrives.
                    self.loop_decoder_eof = true;
                    return false;
                }
                if let Err(error) = self.advance(true) {
                    self.state.playing = false;
                    self.state.error = Some(error);
                }
                true
            }
            PlayerEvent::TimeToPreloadNextTrack {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                if let (Some(player), Some(next)) = (&self.player, self.peek_next_index()) {
                    if let Ok(uri) = parse_track_uri(&self.state.queue[next]) {
                        player.preload(uri);
                    }
                }
                false
            }
            PlayerEvent::Unavailable {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                let clustered = self.unavailable_is_clustered(Instant::now());
                self.seek_in_flight = false;
                self.seek_should_play = false;
                self.loading_failed = true;
                self.finalize_listening(false);
                self.state.playing = false;
                self.state.error = Some(format!("Spotify track is unavailable: {track_id}"));

                // librespot leaves the failed loader in PlayerState::Loading
                // after sending Unavailable. Stop it explicitly so a later
                // Play can submit a fresh Load command instead of toggling
                // start_playback on a terminated future.
                if let Some(player) = &self.player {
                    player.stop();
                }
                self.invalidate_audio_signals();

                // An isolated failure can be a corrupt/truncated cache entry;
                // librespot's decoder retry handles one cached format and
                // this removes every format before the next user retry. Once
                // failures cluster, preserve all cache files: key-service or
                // network failures are not evidence of corruption.
                if !clustered {
                    self.evict_track_audio_cache(track_id);
                }
                true
            }
            PlayerEvent::Stopped {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.state.playing = false;
                self.finalize_listening(false);
                true
            }
            _ => false,
        }
    }

    fn is_current_event(&self, play_request_id: u64, uri: &SpotifyUri) -> bool {
        if self.play_request_id != Some(play_request_id) {
            return false;
        }
        let Some(index) = self.state.current_index else {
            return false;
        };
        let Some(track) = self.state.queue.get(index) else {
            return false;
        };
        uri.to_uri().is_ok_and(|value| value == track.uri)
    }

    fn shutdown_playback(&mut self) {
        self.finalize_listening(false);
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.invalidate_audio_signals();
        self.play_request_id = None;
        self.loading_failed = false;
        self.current_needs_load = self.state.current_index.is_some();
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        self.loop_pass = 1;
        self.seek_in_flight = false;
        self.seek_should_play = false;
        self.recent_track_changes.clear();
        self.recent_unavailable.clear();
        self.mixer = None;
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
    }
}

fn fill_queue_context(queue: &mut [TrackRef], fallback: &str) {
    let fallback = fallback.trim();
    if fallback.is_empty() {
        return;
    }
    for track in queue {
        if track.context.trim().is_empty() {
            track.context = fallback.to_owned();
        }
    }
}

fn with_preview_edit(
    mut track: TrackRef,
    cuts: Vec<TimeRange>,
    loop_range: Option<LoopRange>,
) -> Result<TrackRef, String> {
    let edit = TrackEdit { cuts, loop_range };
    validate_definition(&track.id, track.duration_ms, &edit.cuts, edit.loop_range)?;
    track.effective_edit = Some(edit);
    Ok(track)
}

fn parse_track_uri(track: &TrackRef) -> Result<SpotifyUri, String> {
    let uri = SpotifyUri::from_uri(&track.uri)
        .map_err(|error| format!("invalid Spotify track URI '{}': {error}", track.uri))?;
    if !matches!(&uri, SpotifyUri::Track { .. }) {
        return Err(format!(
            "queue item is not a Spotify track URI: {}",
            track.uri
        ));
    }
    Ok(uri)
}

fn playable_track_uri(track: &TrackRef) -> Result<SpotifyUri, String> {
    if track.unavailable {
        return Err(track
            .unavailable_reason
            .clone()
            .unwrap_or_else(|| "this track is permanently unavailable".to_owned()));
    }
    parse_track_uri(track)
}

fn first_available_from(queue: &[TrackRef], start: usize) -> Option<usize> {
    (start..queue.len()).find(|index| !queue[*index].unavailable)
}

fn first_available_wrapping(queue: &[TrackRef], start: usize) -> Option<usize> {
    first_available_from(queue, start)
        .or_else(|| (0..start.min(queue.len())).find(|index| !queue[*index].unavailable))
}

fn sequential_available_index(
    queue: &[TrackRef],
    current: usize,
    repeat: RepeatMode,
) -> Option<usize> {
    first_available_from(queue, current.saturating_add(1)).or_else(|| {
        (repeat == RepeatMode::Context)
            .then(|| {
                (0..=current.min(queue.len().saturating_sub(1)))
                    .find(|index| !queue[*index].unavailable)
            })
            .flatten()
    })
}

#[cfg(test)]
fn sequential_next_index(current: usize, queue_len: usize, repeat: RepeatMode) -> Option<usize> {
    if current + 1 < queue_len {
        Some(current + 1)
    } else if repeat == RepeatMode::Context && queue_len > 0 {
        Some(0)
    } else {
        None
    }
}

fn remap_current_index_after_move(current: usize, from: usize, to: usize) -> usize {
    if current == from {
        to
    } else if from < current && to >= current {
        current - 1
    } else if from > current && to <= current {
        current + 1
    } else {
        current
    }
}

/// The time to wait before the next command-driven track change may start:
/// zero when none is needed (no prior change, or the interval has already
/// elapsed). Pure so pacing is unit-testable without a real timer.
fn track_change_wait(last: Option<Instant>, now: Instant, interval: Duration) -> Duration {
    let Some(t) = last else {
        return Duration::ZERO;
    };
    let elapsed = now.checked_duration_since(t).unwrap_or_default();
    interval.saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{atomic::Ordering, Arc};
    use std::time::{Duration, Instant};

    use super::{
        first_available_from, first_available_wrapping, remap_current_index_after_move,
        sequential_available_index, sequential_next_index, track_change_wait, with_preview_edit,
        AudioSignal, AuthSignal, Engine, PlaybackState, PlayerSignal, RECONNECT_BACKOFF_MAX,
        RECONNECT_BACKOFF_MIN, TRACK_CHANGE_BURST_WINDOW, TRACK_CHANGE_MIN_INTERVAL,
        UNAVAILABLE_BURST_WINDOW,
    };
    use crate::io::ProtocolWriter;
    use librespot_core::SpotifyUri;
    use librespot_playback::player::PlayerEvent;
    use spotify_playback_engine::protocol::{
        LoopRange, RepeatMode, TimeRange, TrackEdit, TrackRef,
    };

    fn test_engine() -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_in(PathBuf::new())
    }

    fn test_engine_in(state_directory: PathBuf) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        let (writer, buffer) = ProtocolWriter::capture();
        let cache = librespot_core::cache::Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache with no paths");
        (
            Engine::new(
                writer,
                cache,
                PathBuf::new(),
                PathBuf::new(),
                state_directory,
                false,
            ),
            buffer,
        )
    }

    fn track_ref() -> TrackRef {
        TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            ..TrackRef::default()
        }
    }

    fn track_uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:0123456789ABCDEFGHIJKL").expect("valid track uri")
    }

    fn playback_state(duration_ms: u32) -> PlaybackState {
        PlaybackState {
            ready: true,
            auth_state: spotify_playback_engine::protocol::AuthState::Ready,
            auth_url: None,
            playing: false,
            position_ms: 0,
            duration_ms,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            playback_speed: 1.0,
            current_index: Some(0),
            queue: vec![TrackRef {
                duration_ms,
                ..track_ref()
            }],
            error: None,
        }
    }

    fn edited_playback_state(
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        loop_range: Option<LoopRange>,
    ) -> PlaybackState {
        let mut state = playback_state(duration_ms);
        state.queue[0].id = "0123456789ABCDEFGHIJKL".to_owned();
        state.queue[0].duration_ms = duration_ms;
        state.queue[0].effective_edit = Some(TrackEdit { cuts, loop_range });
        state
    }

    fn range(start_ms: u32, end_ms: u32) -> TimeRange {
        TimeRange { start_ms, end_ms }
    }

    fn loop_range(start_ms: u32, end_ms: u32, play_count: u32) -> LoopRange {
        LoopRange {
            start_ms,
            end_ms,
            play_count,
        }
    }

    fn playing_engine() -> Engine {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.play_request_id = Some(7);
        engine
    }
    fn preview_history_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "spotify-renderer-engine-preview-history-{}-{ordinal}",
            std::process::id()
        ))
    }

    fn history_playing_engine() -> (Engine, PathBuf) {
        let root = preview_history_root();
        let (mut engine, _) = test_engine_in(root.clone());
        engine.state = playback_state(240_000);
        engine.state.queue[0].id = "0123456789ABCDEFGHIJKL".to_owned();
        engine.state.queue[0].duration_ms = 240_000;
        engine.play_request_id = Some(7);
        (engine, root)
    }

    fn unavailable_track(id: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            duration_ms: 180_000,
            unavailable: true,
            unavailable_reason: Some("not available in your country".to_owned()),
            ..TrackRef::default()
        }
    }

    #[test]
    fn restore_installs_a_paused_unloaded_playhead_and_skips_a_blocked_index() {
        let (mut engine, _) = test_engine();
        engine.state.ready = true;
        let playable = TrackRef {
            uri: "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        let queue = vec![
            unavailable_track("0abcdefghijklmnopqrstu"),
            playable.clone(),
        ];

        assert_eq!(
            engine.restore_queue(queue, 0, 42_000, String::new(), 0, false, false),
            Ok(true)
        );
        assert_eq!(engine.state.current_index, Some(1));
        assert_eq!(
            engine.state.position_ms, 0,
            "a skipped seed cannot keep its seek"
        );
        assert_eq!(engine.state.duration_ms, playable.duration_ms);
        assert!(!engine.state.playing);
        assert!(engine.current_needs_load);
        assert!(engine.play_request_id.is_none());
    }

    #[test]
    fn guarded_restore_is_a_noop_after_real_queue_wins() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        let original_queue = engine.state.queue.clone();

        // The stale snapshot is intentionally invalid: a guarded restore
        // that has already lost the preview race must not even validate it.
        let result = engine.restore_queue(
            vec![TrackRef::default()],
            4,
            12_000,
            String::new(),
            1,
            true,
            true,
        );

        assert_eq!(result, Ok(false));
        assert_eq!(engine.state.queue.len(), original_queue.len());
        assert_eq!(engine.state.queue[0].uri, original_queue[0].uri);
        assert!(engine.state.playing);
        assert!(!engine.preview_mode);
    }

    #[test]
    fn stale_preview_restore_cannot_overwrite_new_preview() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 22;
        let original_uri = engine.state.queue[0].uri.clone();

        let result = engine.restore_queue(
            vec![TrackRef::default()],
            4,
            12_000,
            String::new(),
            11,
            true,
            false,
        );

        assert_eq!(result, Ok(false));
        assert!(engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 22);
        assert_eq!(engine.state.queue[0].uri, original_uri);
        assert!(engine.state.playing);
    }

    #[test]
    fn failed_preview_replacement_transfers_restore_ownership() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let original_track = engine.state.queue[0].clone();

        let error = engine
            .preview_track_edit(
                original_track.clone(),
                vec![range(10_000, 20_000), range(15_000, 25_000)],
                None,
                5_000,
                2,
            )
            .expect_err("overlapping replacement draft must be rejected");
        assert!(error.contains("overlap") || error.contains("sorted"));
        assert!(engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 2);
        assert_eq!(engine.state.queue[0].uri, original_track.uri);

        assert_eq!(
            engine.restore_queue(
                vec![original_track],
                0,
                42_000,
                String::new(),
                2,
                true,
                false,
            ),
            Ok(true)
        );
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
    }

    #[test]
    fn failed_first_preview_does_not_enter_preview_or_claim_lease() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        let original_uri = engine.state.queue[0].uri.clone();
        let track = engine.state.queue[0].clone();

        let error = engine
            .preview_track_edit(track, vec![range(20_000, 10_000)], None, 5_000, 3)
            .expect_err("invalid first draft must be rejected");

        assert!(!error.is_empty());
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
        assert_eq!(engine.state.queue[0].uri, original_uri);
    }

    #[test]
    fn invalid_restore_does_not_leave_preview_or_stop_the_current_queue() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let original_uri = engine.state.queue[0].uri.clone();

        let error = engine
            .restore_queue(
                vec![TrackRef::default()],
                4,
                12_000,
                String::new(),
                1,
                true,
                false,
            )
            .expect_err("out-of-range restore index must be rejected");

        assert!(error.contains("out of range"));
        assert!(engine.preview_mode);
        assert!(engine.state.playing);
        assert_eq!(engine.state.queue[0].uri, original_uri);
    }

    #[test]
    fn failed_preview_resume_leaves_the_restored_queue_real_paused_and_in_error() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.preview_mode = true;
        engine.preview_lease_id = 1;
        let track = engine.state.queue[0].clone();

        // The capture engine has no player. Restore still installs the real
        // queue and reports the failed resume in state instead of reviving
        // the editor preview.
        assert_eq!(
            engine.restore_queue(vec![track], 0, 42_000, String::new(), 1, true, true),
            Ok(true)
        );
        assert!(!engine.preview_mode);
        assert_eq!(engine.preview_lease_id, 0);
        assert!(!engine.state.playing);
        assert!(engine.state.error.is_some());
        assert_eq!(engine.state.current_index, Some(0));
        assert_eq!(engine.state.position_ms, 42_000);
    }

    #[test]
    fn preview_draft_is_validated_frozen_on_the_row_and_visible_in_state() {
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        let cuts = vec![
            TimeRange {
                start_ms: 1_000,
                end_ms: 2_500,
            },
            TimeRange {
                start_ms: 10_000,
                end_ms: 12_000,
            },
        ];
        let loop_range = Some(loop_range(20_000, 25_000, 2));
        let preview =
            with_preview_edit(track.clone(), cuts.clone(), loop_range).expect("valid draft");
        assert_eq!(
            preview.effective_edit,
            Some(spotify_playback_engine::protocol::TrackEdit {
                cuts: cuts.clone(),
                loop_range,
            })
        );

        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(track.duration_ms);
        engine.state.queue = vec![preview];
        engine.emit_state().expect("preview state emits");
        let line: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("valid state JSON")
        };
        assert_eq!(
            line["queue"][0]["effective_edit"],
            serde_json::json!({"cuts": cuts, "loop_range": loop_range})
        );

        assert!(
            with_preview_edit(
                track.clone(),
                vec![
                    TimeRange {
                        start_ms: 10_000,
                        end_ms: 12_000,
                    },
                    TimeRange {
                        start_ms: 11_000,
                        end_ms: 13_000,
                    },
                ],
                None,
            )
            .is_err(),
            "preview must use the same sorted/non-overlapping validation as persistence"
        );
    }

    #[test]
    fn normal_playback_is_finalized_before_preview_and_preview_events_are_ignored() {
        let (mut engine, root) = history_playing_engine();
        let uri = track_uri();
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 5_000,
        }));
        assert_eq!(engine.history().unwrap().len(), 1);

        let track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(track, vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_err(),
            "the capture engine has no player"
        );
        assert!(engine.preview_mode);
        let rows = engine.history().unwrap();
        assert_eq!(rows.len(), 1, "the real row is finalized, not discarded");
        assert_eq!(rows[0].row.track_id, "0123456789ABCDEFGHIJKL");
        assert!(!rows[0].row.completed);

        engine.play_request_id = Some(8);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: uri,
            position_ms: 5_000,
        }));
        assert_eq!(
            engine.history().unwrap().len(),
            1,
            "a preview Playing event cannot append a draft row"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repeated_preview_updates_keep_history_empty_through_pause_and_eof() {
        let (mut engine, root) = history_playing_engine();
        let track = engine.state.queue[0].clone();
        assert!(engine
            .preview_track_edit(track.clone(), vec![range(1_000, 2_000)], None, 5_000, 1)
            .is_err());
        assert!(engine.preview_mode);

        engine.play_request_id = Some(8);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 5_000,
        }));
        engine.current_needs_load = true;
        assert_eq!(engine.pause(), Ok(true));
        engine.play_request_id = Some(8);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 6_000,
        }));

        assert!(engine
            .preview_track_edit(track, vec![range(3_000, 4_000)], None, 7_000, 2)
            .is_err());
        assert!(engine.preview_mode);
        engine.play_request_id = Some(9);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 9,
            track_id: track_uri(),
            position_ms: 7_000,
        }));
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 9,
            track_id: track_uri(),
        }));
        assert!(engine.history().unwrap().is_empty());
        engine.shutdown();
        assert!(engine.history().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normal_queue_replacement_leaves_preview_without_persisting_it() {
        let (mut engine, root) = history_playing_engine();
        let preview_track = engine.state.queue[0].clone();
        assert!(engine
            .preview_track_edit(preview_track, vec![range(1_000, 2_000)], None, 5_000, 1)
            .is_err());
        assert!(engine.preview_mode);

        let normal_track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 240_000,
            ..TrackRef::default()
        };
        assert!(
            engine
                .play_queue(vec![normal_track], 0, 0, "library".to_owned())
                .is_err(),
            "the capture engine has no player"
        );
        assert!(!engine.preview_mode);
        engine.play_request_id = Some(10);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 10,
            track_id: track_uri(),
            position_ms: 0,
        }));
        let rows = engine.history().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.track_id, "0123456789ABCDEFGHIJKL");
        assert_eq!(rows[0].row.context, "library");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unexpected_player_pause_stops_listening_history_wall_clock() {
        let (mut engine, root) = history_playing_engine();
        let uri = track_uri();
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 5_000,
        }));
        assert!(engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri,
            position_ms: 5_000,
        }));
        let paused_ms = engine.history().unwrap()[0].row.ms_played;
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            engine.history().unwrap()[0].row.ms_played,
            paused_ms,
            "audio-device or player pauses must not accrue silent wall time"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unexpected_player_close_finalizes_the_active_history_row() {
        let (mut engine, root) = history_playing_engine();
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 5_000,
        }));
        assert!(engine.on_player_signal(PlayerSignal::Closed { generation: 0 }));

        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("listening_history.json")).unwrap())
                .unwrap();
        assert!(stored["active"].is_null());
        assert_eq!(stored["finalized"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_progression_strategy_skips_permanent_unavailability() {
        let playable = TrackRef {
            uri: "spotify:track:1abcdefghijklmnopqrstu".to_owned(),
            ..TrackRef::default()
        };
        let queue = vec![
            playable.clone(),
            unavailable_track("2abcdefghijklmnopqrstu"),
            unavailable_track("3abcdefghijklmnopqrstu"),
            playable,
        ];
        assert_eq!(first_available_from(&queue, 1), Some(3));
        assert_eq!(first_available_wrapping(&queue, 2), Some(3));
        assert_eq!(
            sequential_available_index(&queue, 0, RepeatMode::Off),
            Some(3)
        );
        assert_eq!(
            sequential_available_index(&queue, 3, RepeatMode::Context),
            Some(0)
        );

        let blocked = vec![
            unavailable_track("4abcdefghijklmnopqrstu"),
            unavailable_track("5abcdefghijklmnopqrstu"),
        ];
        assert_eq!(first_available_from(&blocked, 0), None);
        assert_eq!(
            sequential_available_index(&blocked, 0, RepeatMode::Context),
            None
        );
    }

    #[test]
    fn same_value_setters_are_no_ops() {
        let (mut engine, _) = test_engine();
        engine.history.push(3);
        engine.shuffle_pool.push(4);

        assert_eq!(engine.set_volume(50), Ok(false));
        assert_eq!(engine.set_shuffle(false), Ok(false));
        assert_eq!(engine.set_repeat(RepeatMode::Off), Ok(false));
        assert_eq!(engine.history, vec![3]);
        assert_eq!(engine.shuffle_pool, vec![4]);
    }

    #[test]
    fn changed_shuffle_and_repeat_values_still_update_state() {
        let (mut engine, _) = test_engine();
        engine.history.push(3);

        assert_eq!(engine.set_shuffle(true), Ok(true));
        assert!(engine.state.shuffle);
        assert!(engine.history.is_empty());
        assert_eq!(engine.set_repeat(RepeatMode::Context), Ok(true));
        assert_eq!(engine.state.repeat, RepeatMode::Context);
    }

    #[test]
    fn playing_and_paused_transitions_update_state_immediately() {
        let mut engine = playing_engine();
        let uri = track_uri();
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 5_000,
        }));
        assert!(engine.state.playing);
        assert_eq!(engine.state.position_ms, 5_000);

        assert!(engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 8_000,
        }));
        assert!(!engine.state.playing);
        assert_eq!(engine.state.position_ms, 8_000);
    }

    #[test]
    fn paused_seek_ignores_a_stale_playing_event() {
        let mut engine = playing_engine();
        let uri = track_uri();
        engine.state.playing = false;
        engine.seek_in_flight = true;
        engine.seek_should_play = false;

        assert!(!engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 41_000,
        }));
        assert!(!engine.state.playing, "the seek pause remains paused");

        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri,
            position_ms: 42_000,
        }));
        assert!(!engine.state.playing, "a paused seek must not unpause");
        assert!(!engine.seek_in_flight);
        assert!(!engine.seek_should_play);
    }

    #[test]
    fn playing_seek_accepts_the_playing_event_after_pause() {
        let mut engine = playing_engine();
        let uri = track_uri();
        engine.seek_in_flight = true;
        engine.seek_should_play = true;

        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri,
            position_ms: 42_000,
        }));
        assert!(engine.state.playing);
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(!engine.seek_in_flight);
    }

    #[test]
    fn loop_marker_reloads_when_decoder_eof_arrived_first() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(spotify_playback_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(1_500, 2_000, 2)),
        });
        assert!(!engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert!(engine.loop_decoder_eof);

        assert!(engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 1_500,
            revision: engine.audio_revision,
        }));
        assert_eq!(engine.state.position_ms, 1_500);
        assert!(
            !engine.loop_decoder_eof,
            "the EOF continuation used load_current, not invalid EndOfTrack seek"
        );
    }

    #[test]
    fn loop_eof_after_an_audible_marker_does_not_wait_for_a_second_marker() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(spotify_playback_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(900, 1_000, 2)),
        });
        engine.loop_jump_pending = true;
        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.position_ms, 900);
        assert!(
            !engine.loop_jump_pending,
            "the EOF race immediately reloaded and consumed the pending jump"
        );
    }

    #[test]
    fn fresh_loads_and_seeks_derive_loop_pass_from_source_position() {
        for (position_ms, expected_pass) in
            [(0, 1), (20_000, 1), (39_999, 1), (40_000, 3), (80_000, 3)]
        {
            let (mut engine, _) = test_engine();
            engine.state =
                edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
            engine.state.position_ms = position_ms;
            engine.current_needs_load = true;

            engine
                .seek_source(position_ms)
                .expect("unloaded seek should not need a player");
            assert_eq!(engine.loop_pass, expected_pass);
        }

        for (position_ms, expected_pass) in [(0, 1), (20_000, 1), (40_000, 3), (80_000, 3)] {
            let (mut engine, _) = test_engine();
            engine.state =
                edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
            engine.state.position_ms = position_ms;

            assert!(
                engine.load_current(true).is_err(),
                "the capture engine has no player"
            );
            assert_eq!(engine.loop_pass, expected_pass);
        }
    }

    #[test]
    fn preview_load_at_or_after_loop_end_starts_the_completed_pass() {
        let (mut engine, _) = test_engine();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            ..TrackRef::default()
        };

        assert!(
            engine
                .preview_track_edit(
                    track,
                    Vec::new(),
                    Some(loop_range(20_000, 40_000, 2)),
                    40_000,
                    1,
                )
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(engine.state.position_ms, 40_000);
        assert_eq!(engine.loop_pass, 2);
    }

    #[test]
    fn eof_at_or_after_loop_end_advances_without_waiting_for_a_marker() {
        for position_ms in [40_000, 80_000] {
            let mut engine = playing_engine();
            engine.state.duration_ms = 100_000;
            engine.state.queue[0].duration_ms = 100_000;
            engine.state.queue[0].effective_edit =
                Some(spotify_playback_engine::protocol::TrackEdit {
                    cuts: Vec::new(),
                    loop_range: Some(loop_range(20_000, 40_000, 3)),
                });
            engine.state.position_ms = position_ms;
            engine.reset_loop_pass_for_position(position_ms);

            assert_eq!(engine.loop_pass, 3);
            assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
                play_request_id: 7,
                track_id: track_uri(),
            }));
            assert!(!engine.loop_decoder_eof);
            assert!(!engine.loop_jump_pending);
            assert_eq!(engine.state.position_ms, 100_000);
        }
    }

    #[test]
    fn finite_loop_markers_stop_after_the_requested_total_passes() {
        for play_count in [2, 3] {
            let mut engine = playing_engine();
            engine.state.duration_ms = 100_000;
            engine.state.queue[0].duration_ms = 100_000;
            engine.state.queue[0].effective_edit =
                Some(spotify_playback_engine::protocol::TrackEdit {
                    cuts: Vec::new(),
                    loop_range: Some(loop_range(20_000, 40_000, play_count)),
                });
            engine.state.playing = true;
            engine.current_needs_load = true;
            let mut markers = 0;

            for pass in 2..=play_count {
                assert!(engine.on_audio_signal(AudioSignal::LoopBoundary {
                    position_ms: 20_000,
                    revision: engine.audio_revision,
                }));
                assert_eq!(engine.loop_pass, pass);
                markers += 1;
            }
            assert_eq!(markers, play_count - 1);
            assert!(!engine.on_audio_signal(AudioSignal::LoopBoundary {
                position_ms: 20_000,
                revision: engine.audio_revision,
            }));
            assert_eq!(engine.loop_pass, play_count);
        }
    }

    #[test]
    fn stale_loop_marker_from_replaced_audio_is_ignored() {
        let mut engine = playing_engine();
        engine.state.queue[0].effective_edit = Some(spotify_playback_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(20_000, 40_000, 2)),
        });
        let stale_revision = engine.audio_revision;
        engine.invalidate_audio_signals();

        assert!(!engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 20_000,
            revision: stale_revision,
        }));
        assert_eq!(engine.loop_pass, 1);
    }

    #[test]
    fn speed_reconfiguration_preserves_internal_pass_and_finishes_at_loop_end() {
        let (mut engine, _) = test_engine();
        engine.state =
            edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
        engine.current_needs_load = true;
        engine.state.position_ms = 25_000;
        engine.loop_pass = 2;

        assert_eq!(engine.set_playback_speed(1.5), Ok(true));
        assert_eq!(engine.loop_pass, 2);

        engine.state.position_ms = 40_000;

        assert_eq!(engine.set_playback_speed(2.0), Ok(true));
        assert_eq!(engine.loop_pass, 3);

        let (mut loaded_engine, _) = test_engine();
        loaded_engine.state =
            edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 3)));
        loaded_engine.state.position_ms = 25_000;
        loaded_engine.loop_pass = 2;
        loaded_engine.loop_decoder_eof = true;
        assert!(loaded_engine.set_playback_speed(1.5).is_err());
        assert_eq!(loaded_engine.loop_pass, 2);
    }

    #[test]
    fn play_reloads_after_decoder_eof_before_a_loop_marker() {
        let (mut engine, _) = test_engine();
        engine.state =
            edited_playback_state(100_000, Vec::new(), Some(loop_range(20_000, 40_000, 2)));
        engine.state.position_ms = 25_000;
        engine.loop_pass = 1;
        engine.loop_decoder_eof = true;

        assert!(engine.play().is_err(), "the capture engine has no player");
        assert!(
            !engine.loop_decoder_eof,
            "play must re-arm a loader instead of calling play on EndOfTrack"
        );
    }

    #[test]
    fn unavailable_recovery_rearms_a_fresh_loading_request() {
        let mut engine = playing_engine();
        let uri = track_uri();

        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: uri.clone(),
        }));
        assert!(!engine.state.playing);
        assert!(engine.loading_failed);
        assert!(engine.state.error.is_some());
        assert!(
            !engine.state.queue[0].unavailable,
            "a runtime PlayerEvent::Unavailable must never poison metadata"
        );

        // A retry gets a new player request id, then Loading clears the
        // failed-loader marker before playback begins.
        assert!(!engine.on_player_event(PlayerEvent::PlayRequestIdChanged { play_request_id: 8 }));
        assert!(engine.on_player_event(PlayerEvent::Loading {
            play_request_id: 8,
            track_id: uri.clone(),
            position_ms: 0,
        }));
        assert!(!engine.loading_failed);
        assert!(engine.state.error.is_none());
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: uri,
            position_ms: 0,
        }));
        assert!(engine.state.playing);
    }

    /// A dead session invalidated by librespot is otherwise silent, so the
    /// heartbeat has to be the thing that notices. It must also not turn a
    /// Spotify outage into a reconnect storm at heartbeat rate.
    #[tokio::test(flavor = "current_thread")]
    async fn a_session_librespot_invalidated_is_noticed_and_reconnected_with_backoff() {
        let (mut engine, _) = test_engine();
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        // Exactly what librespot does to itself when the access point drops.
        session.shutdown();
        assert!(session.is_invalid());
        engine.session = Some(session);
        engine.state.ready = true;
        let generation = engine.generation;
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        // First sight of the corpse reports it, rather than letting the user
        // discover it one failed track at a time.
        assert!(engine.tick_session_health(&sender));
        assert!(!engine.state.ready);
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|error| error.contains("reconnecting")),
            "the dropped connection must be surfaced, got {:?}",
            engine.state.error
        );
        assert_eq!(
            engine.generation, generation,
            "the first tick schedules a reconnect; it does not fire one"
        );

        // Heartbeats before the backoff expires must not attempt anything.
        assert!(!engine.tick_session_health(&sender));
        assert_eq!(engine.generation, generation);

        // Once it is due, the reconnect runs. `start_authentication` bumps the
        // generation so the dead session's in-flight events are ignored.
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_session_health(&sender));
        assert_ne!(engine.generation, generation, "the reconnect must fire");

        // ...and the wait grows, so an outage is not hammered every 2 s.
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
        assert!(engine.reconnect_backoff <= RECONNECT_BACKOFF_MAX);
    }

    /// A healthy session must cost nothing and must clear any backoff a past
    /// outage built up, so the next drop is recovered from just as quickly.
    #[tokio::test(flavor = "current_thread")]
    async fn a_healthy_session_schedules_no_reconnect() {
        let (mut engine, _) = test_engine();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        engine.next_reconnect = Some(Instant::now());
        engine.reconnect_backoff = RECONNECT_BACKOFF_MAX;

        // No session at all is the pre-auth state, not a dead session.
        assert!(!engine.tick_session_health(&sender));
        assert!(engine.next_reconnect.is_none());
        assert_eq!(engine.reconnect_backoff, RECONNECT_BACKOFF_MIN);

        engine.session = Some(librespot_core::Session::new(
            librespot_core::SessionConfig::default(),
            None,
        ));
        engine.next_reconnect = Some(Instant::now());
        assert!(!engine.tick_session_health(&sender));
        assert!(
            engine.next_reconnect.is_none(),
            "a live session is not a corpse"
        );
    }

    /// The regression that made the classifier inert: it was sized against how
    /// fast a user clicks, but it is fed by how fast loads *fail*, which is
    /// seconds slower. Measured gaps from a real dead-session episode were
    /// 6.5 s and 8.9 s; at the old 2 s window every one of these looked
    /// isolated and evicted the cache for a track that was fine.
    #[test]
    fn failures_at_the_observed_cadence_stay_clustered() {
        let mut engine = playing_engine();
        let start = Instant::now();

        assert!(
            !engine.unavailable_is_clustered(start),
            "the first failure in a quiet period stays eligible for cleanup"
        );
        let second = start + Duration::from_millis(8_900);
        assert!(
            engine.unavailable_is_clustered(second),
            "a failure 8.9 s later is the same outage, not a corrupt cache file"
        );
        assert!(
            engine.unavailable_is_clustered(second + Duration::from_millis(6_500)),
            "and so is the one after that"
        );
    }

    #[test]
    fn unavailable_burst_classifier_keeps_isolated_cleanup_eligible() {
        let mut engine = playing_engine();
        let start = Instant::now();

        // One quiet load failure is eligible for the corrupt-cache cleanup.
        engine.note_track_change(start);
        assert!(!engine.unavailable_is_clustered(start + Duration::from_millis(10)));

        // A second failure shortly afterward is clustered, so valid cache
        // files are preserved while the key/network burst settles.
        assert!(engine.unavailable_is_clustered(start + Duration::from_millis(20)));

        // After both windows expire, cleanup is eligible again.
        let quiet = start
            + TRACK_CHANGE_BURST_WINDOW.max(UNAVAILABLE_BURST_WINDOW)
            + Duration::from_millis(25);
        assert!(!engine.unavailable_is_clustered(quiet));

        // Two paced load starts protect even the first failure observed after
        // a rapid-click burst.
        engine.note_track_change(quiet);
        engine.note_track_change(quiet + Duration::from_millis(100));
        assert!(engine.unavailable_is_clustered(quiet + Duration::from_millis(200)));
    }

    #[test]
    fn transition_events_emit_state_lines_with_fresh_positions() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.play_request_id = Some(7);
        let uri = track_uri();
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri,
            position_ms: 12_345,
        }));
        engine.emit_state().expect("state emits");

        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["type"], "state");
        assert_eq!(value["playing"], true);
        assert_eq!(value["position_ms"], 12_345);
    }

    #[test]
    fn drift_tick_reports_position_only_while_playing_with_an_anchor() {
        let mut engine = playing_engine();
        engine.state.playing = false;
        assert!(!engine.tick_position());

        engine.state.playing = true;
        assert!(!engine.tick_position(), "no anchor yet");

        engine.update_position(10_000);
        assert!(engine.tick_position());
        assert!(engine.state.position_ms >= 10_000);
        assert!(engine.state.position_ms < 11_000);

        engine.state.playing = false;
        assert!(!engine.tick_position(), "paused positions are static");
    }

    #[test]
    fn drift_tick_clamps_at_the_track_end() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.duration_ms = 30_000;
        engine.update_position(29_950);
        std::thread::sleep(Duration::from_millis(120));
        assert!(engine.tick_position());
        assert_eq!(engine.state.position_ms, 30_000);
    }

    #[test]
    fn heartbeat_emits_a_scalar_position_line_not_a_full_state() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.update_position(15_000);
        assert!(engine.tick_position());
        engine.emit_position().expect("position emits");

        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("position json");
        let object = value.as_object().expect("position is an object");
        // `type` plus the two playhead scalars: a heartbeat never serializes
        // the queue, so the steady-state cost is O(1) in queue length.
        assert_eq!(object.len(), 3, "only type, position_ms and duration_ms");
        assert_eq!(value["type"], "position");
        assert_eq!(value["duration_ms"], 240_000);
        let position_ms = value["position_ms"].as_u64().expect("scalar position");
        assert!(
            (15_000..16_000).contains(&position_ms),
            "position projects from the anchor: {position_ms}"
        );
        assert!(
            !object.contains_key("queue"),
            "a heartbeat never carries the queue"
        );
        assert!(
            !object.contains_key("playing"),
            "a heartbeat carries no flags"
        );
    }

    #[test]
    fn edited_state_and_heartbeat_serialize_compiled_transport_scalars() {
        let (mut engine, buffer) = test_engine();
        engine.state = edited_playback_state(100_000, vec![range(10_000, 60_000)], None);
        engine.update_position(65_000);

        engine.emit_state().expect("edited state emits");
        let state: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("state json")
        };
        assert_eq!(state["position_ms"], 15_000);
        assert_eq!(state["duration_ms"], 50_000);
        assert_eq!(
            state["queue"][0]["duration_ms"], 100_000,
            "queue metadata stays in source coordinates"
        );

        engine.update_position(100_000);
        engine.emit_position().expect("edited heartbeat emits");
        let heartbeat: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("heartbeat json")
        };
        assert_eq!(heartbeat["position_ms"], 50_000);
        assert_eq!(heartbeat["duration_ms"], 50_000);
    }

    #[test]
    fn unedited_state_serialization_keeps_source_scalars_unchanged() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(100_000);
        engine.update_position(65_000);
        engine.emit_state().expect("unedited state emits");
        let state: serde_json::Value = {
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice(&std::mem::take(&mut *bytes)).expect("state json")
        };
        assert_eq!(state["position_ms"], 65_000);
        assert_eq!(state["duration_ms"], 100_000);
    }

    #[test]
    fn edited_tick_crosses_a_long_cut_continuously_at_every_speed() {
        for (speed, expected_compiled_ms, expected_source_ms) in [
            (0.5, 10_500_u32, 60_500_u32),
            (1.0, 11_500_u32, 61_500_u32),
            (2.0, 13_500_u32, 63_500_u32),
        ] {
            let (mut engine, _) = test_engine();
            engine.state = edited_playback_state(100_000, vec![range(10_000, 60_000)], None);
            engine.state.playing = true;
            engine.state.playback_speed = speed;
            engine.position_anchor = Some((9_500, Instant::now() - Duration::from_secs(2)));

            assert!(engine.tick_position());
            let (compiled_ms, duration_ms) = engine.transport_position_and_duration();
            assert_eq!(duration_ms, 50_000);
            assert!(
                compiled_ms.abs_diff(expected_compiled_ms) <= 5,
                "{speed}x projected to {compiled_ms}, expected {expected_compiled_ms}"
            );
            assert!(
                engine.state.position_ms.abs_diff(expected_source_ms) <= 5,
                "{speed}x source projection did not skip the cut"
            );
        }
    }

    #[test]
    fn compiled_seek_canonicalizes_every_cut_seam_to_audible_source() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(
            10_000,
            vec![range(0, 1_000), range(3_000, 6_000), range(9_000, 10_000)],
            None,
        );
        engine.current_needs_load = true;

        engine.seek_transport(0).expect("seek at starting seam");
        assert_eq!(engine.state.position_ms, 1_000);
        engine.seek_transport(2_000).expect("seek at middle seam");
        assert_eq!(engine.state.position_ms, 6_000);
        engine
            .seek_transport(u32::MAX)
            .expect("seek clamps at compiled EOF");
        assert_eq!(engine.state.position_ms, 10_000);
        assert!(!engine.state.queue[0]
            .effective_edit
            .as_ref()
            .unwrap()
            .cuts
            .iter()
            .any(|cut| {
                cut.start_ms <= engine.state.position_ms && engine.state.position_ms < cut.end_ms
            }));
    }

    #[test]
    fn preview_and_internal_loop_seek_keep_source_coordinates() {
        let (mut preview_engine, _) = test_engine();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            ..TrackRef::default()
        };
        let error = preview_engine
            .preview_track_edit(track, vec![range(10_000, 60_000)], None, 40_000, 1)
            .expect_err("the test engine has no player");
        assert!(error.contains("player"));
        assert_eq!(
            preview_engine.state.position_ms, 40_000,
            "editor preview position remains in the source timeline"
        );

        let (mut loop_engine, _) = test_engine();
        loop_engine.state = edited_playback_state(
            100_000,
            vec![range(10_000, 60_000)],
            Some(loop_range(70_000, 80_000, 3)),
        );
        loop_engine.state.playing = true;
        loop_engine.current_needs_load = true;
        assert!(loop_engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 70_000,
            revision: loop_engine.audio_revision,
        }));
        assert!(loop_engine.on_audio_signal(AudioSignal::LoopBoundary {
            position_ms: 70_000,
            revision: loop_engine.audio_revision,
        }));
        assert_eq!(loop_engine.loop_pass, 3);
        assert_eq!(loop_engine.state.position_ms, 70_000);
        assert_eq!(
            loop_engine.transport_position_and_duration(),
            (20_000, 50_000),
            "the visible playhead wraps to the mapped loop start"
        );
    }

    #[test]
    fn restore_maps_the_compiled_snapshot_after_resolving_live_edits() {
        let root = std::env::temp_dir().join(format!(
            "spotify-renderer-compiled-restore-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (mut engine, _) = test_engine_in(root.clone());
        engine
            .track_edits
            .save_definition(
                "0123456789ABCDEFGHIJKL".to_owned(),
                100_000,
                vec![range(10_000, 60_000)],
                None,
            )
            .unwrap();
        engine
            .track_edits
            .set_enabled("playlist", "0123456789ABCDEFGHIJKL", true)
            .unwrap();
        let track = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 100_000,
            context: "playlist:playlist".to_owned(),
            ..TrackRef::default()
        };

        engine
            .restore_queue(vec![track], 0, 10_000, String::new(), 0, false, false)
            .expect("restore");
        assert_eq!(
            engine.state.position_ms, 60_000,
            "the exact compiled seam restores after the cut"
        );
        assert!(!engine.state.playing);
        assert!(engine.current_needs_load);
        assert_eq!(engine.transport_position_and_duration(), (10_000, 50_000));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn previous_restart_uses_the_compiled_beginning() {
        let (mut engine, _) = test_engine();
        engine.state = edited_playback_state(10_000, vec![range(0, 1_000)], None);
        engine.state.position_ms = 5_000;
        engine.current_needs_load = true;

        engine.previous().expect("restart");
        assert_eq!(engine.state.position_ms, 1_000);
        assert_eq!(engine.transport_position_and_duration().0, 0);
    }

    #[test]
    fn stale_generation_events_do_not_emit() {
        let mut engine = playing_engine();
        let uri = track_uri();
        engine.generation = 9;
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation: 8,
            event: PlayerEvent::Playing {
                play_request_id: 7,
                track_id: uri,
                position_ms: 5_000,
            },
        }));
    }

    #[test]
    fn previous_within_restart_window_restarts_the_current_track() {
        // At the first track with no history, previous restarts in place:
        // the engine reports no switch target (None) and never panics.
        let mut engine = playing_engine();
        engine.state.position_ms = 0;
        assert_eq!(engine.previous_index(), None);

        // Past the restart window the current track restarts too.
        engine.state.position_ms = 5_000;
        assert_eq!(engine.previous_index(), None);

        // A track near its start but not first in the queue goes backwards.
        engine.state.position_ms = 500;
        engine.state.current_index = Some(1);
        assert_eq!(engine.previous_index(), Some(0));
    }

    #[test]
    fn previous_pops_history_with_bounds_guard() {
        let mut engine = playing_engine();
        engine.state.current_index = Some(1);
        engine.history.push(0);
        assert_eq!(engine.previous_index(), Some(0));

        // Stale history entries (indices that no longer index the queue) are
        // dropped instead of panicking; the fallback still applies.
        engine.history.clear();
        engine.history.push(99);
        assert_eq!(
            engine.previous_index(),
            Some(0),
            "falls back to current - 1"
        );
        engine.state.current_index = Some(0);
        engine.history.push(99);
        assert_eq!(
            engine.previous_index(),
            None,
            "first track: restart in place"
        );
    }

    #[test]
    fn previous_without_a_player_errors_gracefully() {
        let mut engine = playing_engine();
        engine.state.position_ms = 0;
        // No previous target -> transport seek to zero -> no player attached:
        // fail with an error, never panic or unwrap an empty queue.
        assert!(engine.previous().is_err());
        engine.state.current_index = None;
        assert!(engine.previous().is_err());
    }

    #[test]
    fn empty_queue_transport_commands_error_without_panicking() {
        let mut engine = playing_engine();
        engine.state.queue.clear();
        engine.state.current_index = Some(0); // inconsistent state a bug could create
        assert!(engine.previous().is_err());
        assert!(engine.advance(false).is_err());
        assert!(engine.load_current(true).is_err());
        assert!(
            !engine.on_player_signal(PlayerSignal::Event {
                generation: engine.generation,
                event: PlayerEvent::Playing {
                    play_request_id: engine.play_request_id.unwrap_or(7),
                    track_id: track_uri(),
                    position_ms: 0,
                },
            }),
            "events for a missing queue index are ignored, not panicked"
        );
        engine.state.current_index = None;
        assert!(engine.previous().is_err());
        assert!(engine.advance(false).is_err());
        assert!(engine.load_current(true).is_err());
    }

    #[test]
    fn sequential_queue_stops_or_wraps_at_the_end() {
        assert_eq!(sequential_next_index(0, 3, RepeatMode::Off), Some(1));
        assert_eq!(sequential_next_index(2, 3, RepeatMode::Off), None);
        assert_eq!(sequential_next_index(2, 3, RepeatMode::Context), Some(0));
        assert_eq!(sequential_next_index(0, 0, RepeatMode::Context), None);
    }

    #[test]
    fn track_change_pacing_waits_out_the_minimum_interval() {
        let start = Instant::now();
        let interval = TRACK_CHANGE_MIN_INTERVAL;
        // No prior change: proceed immediately.
        assert_eq!(track_change_wait(None, start, interval), Duration::ZERO);
        // Change made right now: the full interval applies.
        assert_eq!(track_change_wait(Some(start), start, interval), interval);
        // One millisecond before the interval elapses: one millisecond left.
        assert_eq!(
            track_change_wait(
                Some(start),
                start + interval - Duration::from_millis(1),
                interval
            ),
            Duration::from_millis(1)
        );
        // Interval elapsed exactly, or beyond: no wait.
        assert_eq!(
            track_change_wait(Some(start), start + interval, interval),
            Duration::ZERO
        );
        assert_eq!(
            track_change_wait(
                Some(start),
                start + interval + Duration::from_millis(50),
                interval
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn moving_queue_items_preserves_the_current_track() {
        assert_eq!(remap_current_index_after_move(2, 2, 0), 0);
        assert_eq!(remap_current_index_after_move(2, 0, 3), 1);
        assert_eq!(remap_current_index_after_move(2, 4, 1), 3);
        assert_eq!(remap_current_index_after_move(2, 0, 1), 2);
        assert_eq!(remap_current_index_after_move(2, 3, 4), 2);
    }

    /// A scratch state directory with a `credentials.json` already written
    /// (any content: logout only removes the file). Removed on drop.
    struct CredentialsFixture {
        directory: PathBuf,
    }

    impl CredentialsFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "sr_engine_logout_test_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            let credentials_dir = directory.join("credentials");
            std::fs::create_dir_all(&credentials_dir).expect("fixture credentials dir");
            std::fs::write(
                credentials_dir.join("credentials.json"),
                "{\"username\":\"test-user\"}",
            )
            .expect("fixture credentials file");
            Self { directory }
        }

        fn credentials_file(&self) -> PathBuf {
            self.directory.join("credentials").join("credentials.json")
        }

        fn credentials_exist(&self) -> bool {
            self.credentials_file().exists()
        }
    }

    impl Drop for CredentialsFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn logout_clears_cached_credentials_and_emits_needs_login_with_auth_url() {
        let fixture = CredentialsFixture::new();
        let (mut engine, buffer) = {
            let (writer, buffer) = ProtocolWriter::capture();
            let cache = librespot_core::cache::Cache::new(
                Some(fixture.directory.join("credentials")),
                Some(fixture.directory.join("volume")),
                None::<PathBuf>,
                None,
            )
            .expect("cache with credentials path");
            (
                Engine::new(
                    writer,
                    cache,
                    PathBuf::new(),
                    fixture.credentials_file(),
                    fixture.directory.clone(),
                    false,
                ),
                buffer,
            )
        };
        // A live session: playback state that logout must tear down.
        engine.state = playback_state(240_000);
        engine.state.playing = true;

        assert!(engine.logout().expect("logout succeeds"));
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin
        );
        assert!(!engine.state.ready);
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());
        let published_url = engine.state.auth_url.clone().expect("auth url published");
        assert!(published_url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(
            !fixture.credentials_exist(),
            "credentials file must be removed"
        );

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert_eq!(value["ready"], false);
        assert_eq!(value["auth_url"], published_url);
    }

    #[test]
    fn logout_is_idempotent_when_credentials_are_already_cleared() {
        let fixture = CredentialsFixture::new();
        let (writer, _) = ProtocolWriter::capture();
        let cache = librespot_core::cache::Cache::new(
            Some(fixture.directory.join("credentials")),
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache");
        let mut engine = Engine::new(
            writer,
            cache,
            PathBuf::new(),
            fixture.credentials_file(),
            fixture.directory.clone(),
            false,
        );
        assert!(engine.logout().expect("first logout"));
        assert!(!fixture.credentials_exist());
        assert!(engine.logout().expect("second logout is a no-op"));
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin
        );
    }

    #[test]
    fn startup_without_cached_credentials_enters_needs_login_without_a_flow() {
        let (mut engine, buffer) = test_engine(); // cache has no credentials
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.start_authentication(sender);
        assert!(!engine.auth_running, "no implicit flow may start");
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin
        );
        assert!(engine.state.auth_url.is_some());

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert!(value["auth_url"]
            .as_str()
            .is_some_and(|url| { url.starts_with("https://accounts.spotify.com/authorize?") }));
    }

    #[test]
    fn login_is_a_noop_while_a_session_is_live() {
        let mut engine = playing_engine(); // ready with a player-less state
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::Ready;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine
            .login(&sender)
            .expect("login no-ops when authenticated"));
        assert!(!engine.auth_running);
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::Ready);
    }

    #[test]
    fn login_is_a_noop_while_a_flow_is_already_running() {
        let mut engine = test_engine().0;
        engine.auth_running = true;
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?running".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine
            .login(&sender)
            .expect("login no-ops while authenticating"));
        assert!(engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some("https://accounts.spotify.com/authorize?running"),
            "an in-flight attempt keeps its URL"
        );
    }

    #[test]
    fn normalisation_change_during_auth_updates_shared_preference_without_reconnect() {
        let mut engine = test_engine().0;
        assert!(!engine.normalisation.load(Ordering::Acquire));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        engine.auth_running = true;
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::Authenticating;

        assert!(engine.set_normalisation(true, &sender));
        assert!(engine.normalisation.load(Ordering::Acquire));
        assert!(
            engine.auth_running,
            "the existing authentication keeps running"
        );
        assert!(
            receiver.try_recv().is_err(),
            "no reconnect or parallel player build is scheduled without a live session"
        );
        assert!(
            !engine.tick_session_health(&sender),
            "the health tick has no normalisation rebuild machinery"
        );
    }

    #[test]
    fn normalisation_without_a_session_waits_for_the_next_login() {
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.set_normalisation(true, &sender));
        assert!(engine.normalisation.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            engine.state.auth_state,
            spotify_playback_engine::protocol::AuthState::NeedsLogin,
            "the login flow itself must not be disturbed"
        );
    }

    #[test]
    fn begin_login_flow_consumes_the_published_attempt_and_marks_authenticating() {
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let published = engine
            .state
            .auth_url
            .clone()
            .expect("needs_login publishes a url");
        let pending = engine.begin_login_flow().expect("flow begins");
        assert_eq!(
            pending.auth_url, published,
            "the flow must use the published URL"
        );
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::Authenticating
        );
        assert!(engine.auth_running);
        assert_eq!(engine.state.auth_url.as_deref(), Some(published.as_str()));
        assert!(engine.pending_auth.is_none());
    }

    #[test]
    fn begin_login_flow_prepares_a_fresh_attempt_when_none_is_pending() {
        let mut engine = test_engine().0;
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::NeedsLogin;
        let pending = engine.begin_login_flow().expect("flow begins");
        assert!(pending
            .auth_url
            .starts_with("https://accounts.spotify.com/authorize?"));
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::Authenticating
        );
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some(pending.auth_url.as_str())
        );
    }

    #[test]
    fn failed_auth_signal_returns_to_needs_login_with_a_fresh_url() {
        let mut engine = test_engine().0;
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?first".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err("Spotify authentication failed: test".to_owned()),
            },
            sender,
        ));
        assert!(
            engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin
        );
        assert!(!engine.state.ready);
        assert!(!engine.auth_running);
        let retry_url = engine.state.auth_url.clone().expect("retry url published");
        assert_ne!(
            retry_url, "https://accounts.spotify.com/authorize?first",
            "a retry must regenerate the URL"
        );
        assert!(engine
            .state
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("Spotify authentication failed") }));
    }

    #[test]
    fn state_events_omit_auth_url_when_no_attempt_is_pending() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.auth_url = None;
        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert!(value.get("auth_url").is_none(), "auth_url must be omitted");
        assert_eq!(value["auth_state"], "ready");
    }
}
