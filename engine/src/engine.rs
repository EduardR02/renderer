use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_metadata::Metadata;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::audio::AudioSignal;
use crate::auth::{
    AuthFailure, PendingAuth, PlaybackHandles, complete_oauth, connect_cached, create_playback,
    percent_to_volume, prepare_oauth,
};
use crate::customization::{EditTimeline, TrackEditStore, validate_definition};
use crate::history::ListeningHistory;
use crate::io::ProtocolWriter;
use renderer_engine::protocol::{
    AuthState, BrowseResponse, Command, HistoryItem, LoopRange, PositionEvent, RepeatMode,
    Response, StateEvent, TimeRange, TrackEdit, TrackEditDefinition, TrackEditStatus, TrackRef,
};
use serde::Serialize;
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
    /// Playback intent captured when an authenticated session dies. While
    /// true, cached reauthentication keeps the old player alive until the
    /// replacement handles are ready; normal startup and paused reconnects
    /// remain cold.
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
    /// Set when a cached-auth attempt failed on transport rather than refusal.
    /// It shuts its own session down, so there is no dead session for the
    /// health tick to find, and "no session" already means the pre-auth state.
    awaiting_transport_retry: bool,
    shuffle_pool: Vec<usize>,
    /// Queue navigation history used by Previous; local listening rows live
    /// in `listening_history`.
    history: Vec<usize>,
    listening_history: ListeningHistory,
    random_state: u64,
    generation: u64,
    /// A seek issued while playing is mid-transition: [`Engine::seek_source`]
    /// pauses librespot, seeks, and plays again, and this suppresses the
    /// transient Paused in the middle so the UI does not blip. Only a playing
    /// seek arms it, because only a playing seek has a Playing event coming
    /// that can disarm it again.
    seek_in_flight: bool,
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
        result: Result<PlaybackHandles, AuthFailure>,
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
            awaiting_transport_retry: false,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            listening_history: ListeningHistory::new(history_root),
            random_state,
            generation: 0,
            seek_in_flight: false,
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
    pub fn playlist_excluded_track_ids(&self, playlist_id: &str) -> Result<Vec<String>, String> {
        self.track_edits.list_excluded_track_ids(playlist_id)
    }

    pub fn set_playlist_track_excluded(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        excluded: bool,
    ) -> Result<(), String> {
        self.track_edits
            .set_excluded(playlist_id, track_id, excluded)?;
        // Exclusion changes never stop or reload the current track. They do
        // change every future choice, including a pending shuffle/preload
        // decision, so rebuild those cheap plans immediately.
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(())
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
    /// [`RECONNECT_BACKOFF_MIN`]. An active player remains logically playing
    /// during the reconnect so its projected playhead and listening row reach
    /// the handover without an artificial gap.
    pub fn tick_session_health(&mut self, sender: &mpsc::UnboundedSender<AuthSignal>) -> bool {
        if self.auth_running {
            return false;
        }
        let dead = self
            .session
            .as_ref()
            .is_some_and(librespot_core::Session::is_invalid);
        // A cached-auth attempt that failed on transport shut its session down
        // and left only the armed retry, so there is no corpse to notice. That
        // is the same outage as a dead session and recovers the same way.
        let retry_armed = self.awaiting_transport_retry && self.next_reconnect.is_some();
        if !dead && !retry_armed {
            // A healthy session ends any backoff a previous outage built up.
            self.next_reconnect = None;
            self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
            return false;
        }

        let now = Instant::now();
        let Some(due) = self.next_reconnect else {
            // First sight of the corpse schedules the reconnect. Preserve an
            // active queue's playback intent: the old player may still have
            // buffered audio, and its playhead remains the best handover
            // position until replacement handles exist.
            self.next_reconnect = Some(now + self.reconnect_backoff);
            self.resume_after_reconnect = self.state.playing && self.state.current_index.is_some();
            self.state.ready = false;
            self.state.error = Some("the Spotify connection dropped; reconnecting".to_owned());
            eprintln!("Spotify session went invalid; reconnecting");
            return true;
        };
        if now < due {
            return false;
        }

        self.reconnect_backoff = (self.reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
        self.next_reconnect = Some(now + self.reconnect_backoff);
        // Reauthentication bumps the generation before its asynchronous work,
        // making every event from the preserved player stale. Paused and
        // queue-less reconnects take the normal cold teardown path.
        self.start_cached_authentication(sender.clone(), self.resume_after_reconnect);
        true
    }

    /// Enables or disables track-gain volume normalisation.
    ///
    /// A live change constructs only a new player against the existing
    /// authenticated session. While playback is unavailable, the atomic
    /// preference is consumed by the authentication already in flight.
    pub fn set_normalisation(
        &mut self,
        enabled: bool,
        sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> bool {
        if self.normalisation.swap(enabled, Ordering::AcqRel) == enabled {
            return false;
        }
        if !self.state.ready {
            return true;
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
        self.start_cached_authentication(sender, false);
    }

    fn start_cached_authentication(
        &mut self,
        sender: mpsc::UnboundedSender<AuthSignal>,
        preserve_active_playback: bool,
    ) {
        if self.auth_running {
            return;
        }
        let generation = self.begin_cached_authentication(preserve_active_playback);
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

    /// Applies the synchronous half of cached authentication. Split from the
    /// task spawn so the two teardown modes remain an explicit state
    /// transition and can be tested without connecting to Spotify.
    fn begin_cached_authentication(&mut self, preserve_active_playback: bool) -> u64 {
        if !preserve_active_playback {
            self.resume_after_reconnect = false;
            self.shutdown_playback();
        }
        self.auth_running = true;
        self.generation = self.generation.wrapping_add(1);
        self.state.ready = false;
        self.state.auth_state = AuthState::Authenticating;
        if !preserve_active_playback {
            self.state.playing = false;
        }
        self.state.error = None;
        self.generation
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
        self.next_reconnect = None;
        self.awaiting_transport_retry = false;
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
            let _ = auth_sender.send(AuthSignal::Complete {
                generation,
                result: result.map_err(AuthFailure::Rejected),
            });
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
                        // Whatever the previous generation still owns goes away
                        // before the replacement is installed. Assigning over a
                        // live player would leave it feeding the audio device
                        // while every transport command reached its
                        // replacement: audible playback that pause, seek and
                        // next no longer touch, until the app is restarted.
                        if self.resume_after_reconnect {
                            self.stop_playback_for_reconnect_handover();
                        } else {
                            self.shutdown_playback();
                        }
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
                        self.awaiting_transport_retry = false;
                        Self::forward_player_events(handles.events, generation, player_sender);
                        true
                    }
                    Err(failure) => {
                        eprintln!("Spotify playback authentication failed: {failure}");
                        match failure {
                            AuthFailure::Rejected(message) => {
                                self.enter_needs_login();
                                self.state.error = Some(message);
                            }
                            AuthFailure::Unreachable(message) => {
                                // The credentials are fine, the network is not.
                                // Keep both them and the queue and arm another
                                // attempt: resuming from sleep regularly beats
                                // Windows' resolver to the first connect, and
                                // answering that with a login prompt discards a
                                // session that was never refused.
                                self.state.ready = false;
                                self.state.auth_state = AuthState::Authenticating;
                                self.state.error = Some(message);
                                self.reconnect_backoff =
                                    (self.reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                                self.next_reconnect = Some(Instant::now() + self.reconnect_backoff);
                                self.awaiting_transport_retry = true;
                            }
                        }
                        true
                    }
                }
            }
            AuthSignal::PlayerRebuilt {
                generation,
                normalisation,
                result,
            } => {
                if !self.player_rebuild_is_current(generation, normalisation) {
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
    fn player_rebuild_is_current(&self, generation: u64, normalisation: bool) -> bool {
        self.state.ready
            && generation == self.generation
            && self.normalisation.load(Ordering::Acquire) == normalisation
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
            | Command::BrowseArtistSongwriter { .. }
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
            | Command::SetPlaylistTrackExcluded { .. }
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
                automatic_start,
            } => self.play_queue_with_automatic_start(
                queue,
                index,
                position_ms,
                context,
                automatic_start,
            ),
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
    /// Eligibility is intentionally derived from the live row context and
    /// store on every transition. Queue snapshots carry no exclusion bit, so
    /// changing a preference immediately affects future choices without
    /// rewriting or reloading the queue.
    fn automatic_track_eligible(&self, index: usize) -> bool {
        self.state.queue.get(index).is_some_and(|track| {
            // Editor previews are a separate direct playback surface. Their
            // source row may retain a playlist context, but playlist
            // exclusions must never interfere with preview transport.
            if self.preview_mode {
                !track.unavailable
            } else {
                automatic_track_eligible(&self.track_edits, track)
            }
        })
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

    #[cfg(test)]
    fn play_queue(
        &mut self,
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
    ) -> Result<bool, String> {
        self.play_queue_with_automatic_start(queue, index, position_ms, context, false)
    }

    fn play_queue_with_automatic_start(
        &mut self,
        mut queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        context: String,
        automatic_start: bool,
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
            automatic_start,
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
            false,
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
        automatic_start: bool,
    ) -> Result<bool, String> {
        Self::validate_queue(&queue, index)?;
        let candidate = if automatic_start {
            first_automatic_wrapping(&queue, index, &self.track_edits)
        } else {
            first_available_from(&queue, index)
        };
        let Some(playable_index) = candidate else {
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
            let result = self.install_empty_or_unavailable_queue(queue);
            if automatic_start && result.is_ok() {
                self.state.error =
                    Some("no eligible tracks remain for automatic playback".to_owned());
            }
            return result;
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
        let was_playing = self.state.playing;
        // There is no live load to resume when one is pending, has failed, or
        // has run off the end of the track, so those all start a fresh one.
        let reload = self.loop_decoder_eof
            || self.current_needs_load
            || self.loading_failed
            || (self.state.duration_ms > 0 && self.state.position_ms >= self.state.duration_ms);
        if reload {
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
        // Transport commands are otherwise invisible in the log: a resume
        // prints nothing, and neither does a load librespot satisfies from the
        // track it already holds. A session where the engine and librespot
        // disagreed about `playing` could not be told apart afterwards from one
        // where no command ever arrived, because neither left a record.
        eprintln!(
            "transport: play at {} ms (engine was {}); librespot told to {}",
            self.state.position_ms,
            if was_playing { "playing" } else { "paused" },
            if reload { "load" } else { "resume" },
        );
        Ok(true)
    }

    fn pause(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        let was_playing = self.state.playing;
        // The engine may not call itself paused before librespot has been told
        // to stop, so the command goes out first and a failure to reach the
        // player aborts the transition rather than declaring a pause that never
        // happened. This used to be skipped whenever a load was pending or had
        // failed, on the theory that pause is invalid in the state librespot is
        // then in — but `current_needs_load` records an intent to load, not
        // that librespot stopped, and the two come apart the instant anything
        // sets the flag while audio is still running. librespot makes the
        // unconditional call safe from every direction: it clears the pending
        // start of a Loading player (precisely what a pause during a load
        // should do), is idempotent and silent on an already paused one, and
        // costs one log line and no state change on a stopped or finished one.
        // Skipping it costs audio that keeps playing with no transport control
        // left that can reach it.
        self.player()?.pause();
        // Only now, with the stop actually issued: a user pause supersedes any
        // in-flight seek transition, so its own Paused event must be delivered
        // rather than suppressed.
        self.seek_in_flight = false;
        self.state.playing = false;
        self.update_position(self.state.position_ms);
        self.pause_listening();
        self.state.error = None;
        // See the note in `play`. A pause arriving at an already paused engine
        // is the specific signature of a stale UI, and it used to leave no
        // trace at all.
        eprintln!(
            "transport: pause at {} ms (engine was {})",
            self.state.position_ms,
            if was_playing { "playing" } else { "paused" },
        );
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
        // Only a playing seek has a transition to hold open. A paused seek's
        // pause is silent (librespot sends no Paused event when it is already
        // paused) and no play follows it, so arming the guard here would leave
        // it set for the rest of the queue — swallowing every genuine Paused
        // and inverting the next genuine Playing into a pause.
        self.seek_in_flight = was_playing;
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
            if self.state.shuffle
                && self.automatic_track_eligible(current)
                && !self.shuffle_pool.contains(&current)
            {
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
            if self.automatic_track_eligible(index) {
                return Some(index);
            }
        }
        let current = self.state.current_index?;
        if !self.state.shuffle {
            return (0..current)
                .rev()
                .find(|index| self.automatic_track_eligible(*index));
        }
        None
    }

    fn advance(&mut self, at_end: bool) -> Result<bool, String> {
        self.advance_with_current_skip(at_end, false)
    }

    fn advance_with_current_skip(
        &mut self,
        at_end: bool,
        skip_current_for_repeat: bool,
    ) -> Result<bool, String> {
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
        let next = self.take_next_index_with_skip(at_end, skip_current_for_repeat);
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
                // A track change carries the transport intent across, exactly
                // as `previous` does: a paused queue does not start playing
                // because the user skipped or a track failed, and a playing one
                // does not fall silent. Loading with a fixed `true` made the
                // engine call itself paused while librespot played the new
                // track, leaving the two to be reconciled by whichever event
                // happened to arrive next.
                let start_playing = self.state.playing;
                if at_end {
                    self.load_current_after_natural_boundary(start_playing)?;
                } else {
                    self.load_current(start_playing)?;
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
                eprintln!("transport: no eligible track follows queue index {current}; stopping");
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
                let replacement =
                    first_automatic_wrapping(&self.state.queue, start, &self.track_edits);
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
                    self.state.error =
                        Some("no eligible tracks remain for automatic playback".to_owned());
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
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        let player = Arc::clone(self.player()?);
        self.loading_failed = false;
        self.current_needs_load = false;
        // What the engine reports and what it asks librespot for are one
        // statement, made once, here. Callers used to set `playing` beside the
        // call, which let the two say different things — a track change loading
        // with a hardcoded `true` while the engine still called itself paused,
        // with nothing but the next incoming event to settle the argument.
        self.state.playing = start_playing;
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

    fn take_next_index_with_skip(
        &mut self,
        at_end: bool,
        skip_current_for_repeat: bool,
    ) -> Option<usize> {
        let current = self.state.current_index?;
        if at_end
            && !skip_current_for_repeat
            && self.state.repeat == RepeatMode::Track
            && self
                .state
                .queue
                .get(current)
                .is_some_and(|track| !track.unavailable)
        {
            // Repeat-one is a deliberate direct continuation: an exclusion
            // toggled while this row is playing must not interrupt it.
            return Some(current);
        }
        if self.state.shuffle {
            if self.shuffle_pool.is_empty() && self.state.repeat == RepeatMode::Context {
                self.rebuild_shuffle_pool();
            }
            while let Some(index) = self.shuffle_pool.pop() {
                if self.automatic_track_eligible(index) {
                    return Some(index);
                }
            }
            return None;
        }
        sequential_automatic_index(
            &self.state.queue,
            current,
            self.state.repeat,
            &self.track_edits,
        )
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
            // See `take_next_index_with_skip`: preload for repeat-one is
            // allowed to point at the excluded current row.
            return Some(current);
        }
        if self.state.shuffle {
            return self.shuffle_pool.iter().rev().copied().find(|index| {
                self.state
                    .queue
                    .get(*index)
                    .is_some_and(|_| self.automatic_track_eligible(*index))
            });
        }
        sequential_automatic_index(
            &self.state.queue,
            current,
            self.state.repeat,
            &self.track_edits,
        )
    }
    fn rebuild_shuffle_pool(&mut self) {
        self.shuffle_pool.clear();
        if !self.state.shuffle {
            return;
        }
        let current = self.state.current_index;
        let queue = &self.state.queue;
        let track_edits = &self.track_edits;
        let preview_mode = self.preview_mode;
        for (index, track) in queue.iter().enumerate() {
            if Some(index) == current {
                continue;
            }
            let eligible = if preview_mode {
                !track.unavailable
            } else {
                automatic_track_eligible(track_edits, track)
            };
            if eligible {
                self.shuffle_pool.push(index);
            }
        }
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
                // over.
                self.seek_in_flight = false;
                self.loading_failed = false;
                self.loop_jump_pending = false;
                self.clear_unavailable_burst();
                if let Some(index) = self.state.current_index {
                    if let Some(track) = self.state.queue.get(index).cloned() {
                        self.start_listening(&track);
                    }
                }
                self.state.playing = true;
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
                self.loading_failed = true;
                self.finalize_listening(false);
                // `state.playing` is left alone deliberately: it is the intent
                // the skip below has to carry onto the replacement track. The
                // two branches of that skip both settle it — a successor loads
                // with this intent, and an exhausted queue stops.
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
                // A runtime failure is an automatic progression opportunity:
                // continue with the next eligible row when one exists. The
                // `skip_current_for_repeat` guard prevents repeat-one from
                // retrying the same failed loader forever. With no candidate,
                // the branch below simply leaves this failed row stopped.
                if let Err(error) = self.advance_with_current_skip(false, true) {
                    self.state.playing = false;
                    self.state.error = Some(error);
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
            // Every arm above is gated on `is_current_event`, so a transport
            // event landing here is one the engine refused: its play request or
            // its current row no longer describes what librespot is doing. That
            // is the difference between "the engine went deaf to its player"
            // and "no command ever arrived", which the log could not previously
            // tell apart — both look like silence.
            event @ (PlayerEvent::Playing { .. }
            | PlayerEvent::Paused { .. }
            | PlayerEvent::EndOfTrack { .. }
            | PlayerEvent::Stopped { .. }
            | PlayerEvent::Unavailable { .. }) => {
                eprintln!(
                    "transport: ignored {event:?}; engine holds play request {:?}",
                    self.play_request_id
                );
                false
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
        self.stop_playback_handles();
    }

    /// Projects the logical playhead to the exact replacement instant, then
    /// stops the old generation without finalizing its uninterrupted
    /// listening-history row.
    fn stop_playback_for_reconnect_handover(&mut self) {
        self.tick_position();
        self.stop_playback_handles();
    }

    fn stop_playback_handles(&mut self) {
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

fn automatic_playlist_id(context: &str) -> Option<&str> {
    let playlist_id = context.strip_prefix("playlist:")?;
    (!playlist_id.trim().is_empty()).then_some(playlist_id)
}

fn automatic_track_eligible(store: &TrackEditStore, track: &TrackRef) -> bool {
    if track.unavailable {
        return false;
    }
    let Some(playlist_id) = automatic_playlist_id(&track.context) else {
        return true;
    };
    !store
        .is_excluded(playlist_id, &track.id)
        .is_ok_and(|excluded| excluded)
}

fn first_automatic_from(queue: &[TrackRef], start: usize, store: &TrackEditStore) -> Option<usize> {
    (start..queue.len()).find(|index| automatic_track_eligible(store, &queue[*index]))
}

fn first_automatic_wrapping(
    queue: &[TrackRef],
    start: usize,
    store: &TrackEditStore,
) -> Option<usize> {
    first_automatic_from(queue, start, store).or_else(|| {
        (0..start.min(queue.len())).find(|index| automatic_track_eligible(store, &queue[*index]))
    })
}

fn sequential_automatic_index(
    queue: &[TrackRef],
    current: usize,
    repeat: RepeatMode,
    store: &TrackEditStore,
) -> Option<usize> {
    first_automatic_from(queue, current.saturating_add(1), store).or_else(|| {
        (repeat == RepeatMode::Context)
            .then(|| {
                (0..=current.min(queue.len().saturating_sub(1)))
                    .find(|index| automatic_track_eligible(store, &queue[*index]))
            })
            .flatten()
    })
}

fn first_available_from(queue: &[TrackRef], start: usize) -> Option<usize> {
    (start..queue.len()).find(|index| !queue[*index].unavailable)
}

fn first_available_wrapping(queue: &[TrackRef], start: usize) -> Option<usize> {
    first_available_from(queue, start)
        .or_else(|| (0..start.min(queue.len())).find(|index| !queue[*index].unavailable))
}

#[cfg(test)]
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    };
    use std::time::{Duration, Instant};

    use super::{
        AudioSignal, AuthFailure, AuthSignal, Engine, PlaybackHandles, PlaybackState, PlayerSignal,
        RECONNECT_BACKOFF_MAX, RECONNECT_BACKOFF_MIN, TRACK_CHANGE_BURST_WINDOW,
        TRACK_CHANGE_MIN_INTERVAL, UNAVAILABLE_BURST_WINDOW, automatic_track_eligible,
        first_automatic_from, first_automatic_wrapping, first_available_from,
        first_available_wrapping, remap_current_index_after_move, sequential_automatic_index,
        sequential_available_index, sequential_next_index, track_change_wait, with_preview_edit,
    };
    use crate::customization::TrackEditStore;
    use crate::io::ProtocolWriter;
    use librespot_core::SpotifyUri;
    use librespot_playback::audio_backend::{Sink, SinkResult};
    use librespot_playback::config::PlayerConfig;
    use librespot_playback::convert::Converter;
    use librespot_playback::decoder::AudioPacket;
    use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume, softmixer::SoftMixer};
    use librespot_playback::player::{Player, PlayerEvent};
    use renderer_engine::protocol::{LoopRange, RepeatMode, TimeRange, TrackEdit, TrackRef};

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

    /// Watches one librespot player from the outside. The sink is destroyed
    /// with the player thread, so its liveness is the observable form of "that
    /// player is really gone" rather than "the engine no longer names it".
    struct SinkProbe {
        alive: Arc<AtomicBool>,
    }

    struct ProbeSink {
        alive: Arc<AtomicBool>,
    }

    impl SinkProbe {
        fn new() -> Self {
            Self {
                alive: Arc::new(AtomicBool::new(true)),
            }
        }

        fn sink(&self) -> ProbeSink {
            ProbeSink {
                alive: Arc::clone(&self.alive),
            }
        }

        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::Acquire)
        }
    }

    impl Sink for ProbeSink {
        fn start(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn stop(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn write(&mut self, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            Ok(())
        }
    }

    impl Drop for ProbeSink {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Release);
        }
    }

    /// A real librespot player with no audio device behind it, so the lifetime
    /// under test is librespot's own: the thread ends when the last `Arc` goes,
    /// and `Drop for Player` joins it before returning.
    fn probe_player(probe: &SinkProbe) -> (Arc<Player>, librespot_core::Session) {
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        let sink = probe.sink();
        let player = Player::new(
            PlayerConfig::default(),
            session.clone(),
            Box::new(NoOpVolume),
            move || Box::new(sink),
        );
        (player, session)
    }

    fn playback_handles(player: Arc<Player>, session: librespot_core::Session) -> PlaybackHandles {
        let events = player.get_player_event_channel();
        PlaybackHandles {
            player,
            events,
            mixer: Arc::new(SoftMixer::open(MixerConfig::default()).expect("software mixer")),
            session,
            volume_percent: 50,
        }
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
            auth_state: renderer_engine::protocol::AuthState::Ready,
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

    fn two_track_state() -> PlaybackState {
        let mut state = playback_state(240_000);
        state.queue.push(TrackRef {
            id: "0123456789ABCDEFGHIJKM".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKM".to_owned(),
            duration_ms: 180_000,
            ..TrackRef::default()
        });
        state
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

    /// A playing engine holding a real librespot player. Transport commands
    /// then reach a live command channel instead of failing on a missing
    /// player, which is what makes the pause and seek paths reachable at all.
    /// The probe is returned so the caller keeps the sink alive.
    fn engine_with_player() -> (Engine, SinkProbe) {
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let mut engine = playing_engine();
        engine.player = Some(player);
        engine.session = Some(session);
        (engine, probe)
    }
    fn preview_history_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "renderer-engine-preview-history-{}-{ordinal}",
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

    struct TempStateDirectory(PathBuf);

    impl TempStateDirectory {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "renderer-engine-exclusion-{}-{ordinal}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempStateDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store_with_exclusions(exclusions: &[(&str, &str)]) -> (TempStateDirectory, TrackEditStore) {
        let directory = TempStateDirectory::new();
        let mut store = TrackEditStore::load(directory.path()).expect("empty edit store");
        for &(playlist_id, track_id) in exclusions {
            store
                .set_excluded(playlist_id, track_id, true)
                .expect("persist exclusion");
        }
        (directory, store)
    }

    fn contextual_track(id: &str, context: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            duration_ms: 180_000,
            context: context.to_owned(),
            ..TrackRef::default()
        }
    }

    fn engine_with_queue(store: TrackEditStore, queue: Vec<TrackRef>, current: usize) -> Engine {
        let (mut engine, _) = test_engine();
        let duration_ms = queue[current].duration_ms;
        engine.track_edits = store;
        engine.state.queue = queue;
        engine.state.current_index = Some(current);
        engine.state.duration_ms = duration_ms;
        engine.state.position_ms = 0;
        engine.state.playing = true;
        engine
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
            Some(renderer_engine::protocol::TrackEdit {
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

    #[tokio::test(flavor = "current_thread")]
    async fn repeated_preview_updates_keep_history_empty_through_pause_and_eof() {
        let (mut engine, root) = history_playing_engine();
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        engine.player = Some(player);
        engine.session = Some(session);
        let track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(track.clone(), vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_ok()
        );
        assert!(engine.preview_mode);

        engine.play_request_id = Some(8);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 5_000,
        }));
        assert_eq!(engine.pause(), Ok(true));
        engine.play_request_id = Some(8);
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 8,
            track_id: track_uri(),
            position_ms: 6_000,
        }));

        assert!(
            engine
                .preview_track_edit(track, vec![range(3_000, 4_000)], None, 7_000, 2)
                .is_ok()
        );
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
        assert!(
            engine
                .preview_track_edit(preview_track, vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_err()
        );
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
    fn automatic_start_skips_excluded_but_direct_play_and_index_accept_it() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        drop(store);

        let (mut automatic_engine, _) = test_engine_in(directory.path().to_path_buf());
        assert!(
            automatic_engine
                .play_queue_with_automatic_start(
                    queue.clone(),
                    0,
                    12_000,
                    "playlist:playlist".to_owned(),
                    true,
                )
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            automatic_engine.state.current_index,
            Some(1),
            "automatic starts skip the excluded requested row"
        );

        let (mut direct_engine, _) = test_engine_in(directory.path().to_path_buf());
        assert!(
            direct_engine
                .play_queue(queue.clone(), 0, 12_000, "playlist:playlist".to_owned())
                .is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            direct_engine.state.current_index,
            Some(0),
            "direct queue playback accepts the excluded requested row"
        );

        let (mut index_engine, _) = test_engine_in(directory.path().to_path_buf());
        index_engine.state.queue = queue;
        index_engine.state.current_index = Some(1);
        index_engine.state.duration_ms = 180_000;
        assert!(
            index_engine.play_queue_index(0).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            index_engine.state.current_index,
            Some(0),
            "direct index playback accepts the excluded row"
        );
    }

    #[test]
    fn ordered_next_and_eof_skip_excluded_rows() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        assert_eq!(
            sequential_automatic_index(
                &engine.state.queue,
                0,
                RepeatMode::Off,
                &engine.track_edits,
            ),
            Some(2)
        );

        assert!(
            engine.advance(false).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            engine.state.current_index,
            Some(2),
            "Next skips excluded rows"
        );

        engine.state.current_index = Some(0);
        engine.state.duration_ms = 180_000;
        engine.state.position_ms = 0;
        engine.state.playing = true;
        engine.history.clear();
        assert!(
            engine.advance(true).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(
            engine.state.current_index,
            Some(2),
            "natural EOF skips excluded rows"
        );
        drop(directory);
    }

    #[test]
    fn repeat_context_wraps_only_to_eligible_rows() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 2);
        engine.state.repeat = RepeatMode::Context;

        assert_eq!(
            sequential_automatic_index(
                &engine.state.queue,
                2,
                RepeatMode::Context,
                &engine.track_edits,
            ),
            Some(1)
        );
        assert_eq!(engine.take_next_index_with_skip(false, false), Some(1));
        drop(directory);
    }

    #[test]
    fn shuffle_pool_excludes_playlist_preferences() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.shuffle = true;
        engine.rebuild_shuffle_pool();

        assert_eq!(
            engine.shuffle_pool,
            vec![2],
            "shuffle only contains non-current eligible rows"
        );
        drop(directory);
    }

    #[test]
    fn previous_history_and_ordered_fallback_skip_excluded_rows() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 2);

        // History is newest-first in the vector's pop order here: it first
        // offers the excluded row, then the eligible earlier row.
        engine.history = vec![1, 0];
        assert_eq!(engine.previous_index(), Some(1));
        engine.history.clear();
        assert_eq!(
            engine.previous_index(),
            Some(1),
            "ordered fallback also skips excluded rows"
        );
        drop(directory);
    }

    #[test]
    fn preload_peek_skips_excluded_rows() {
        let excluded_id = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("0abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let engine = engine_with_queue(store, queue, 0);

        assert_eq!(
            engine.peek_next_index(),
            Some(2),
            "peek selects the same eligible target used by preload"
        );
        engine.preload_next();
        drop(directory);
    }

    #[test]
    fn repeat_one_continues_a_manually_started_excluded_current() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![contextual_track(excluded_id, "playlist:playlist")];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.repeat = RepeatMode::Track;

        assert_eq!(
            engine.take_next_index_with_skip(true, false),
            Some(0),
            "repeat-one preserves a manually started excluded current"
        );
        assert_eq!(engine.peek_next_index(), Some(0));
        drop(directory);
    }

    #[test]
    fn toggling_current_exclusion_does_not_stop_playback() {
        let track_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(track_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.position_ms = 7_500;

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", track_id, true),
            Ok(())
        );
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.state.playing, "toggling a preference must not pause");
        assert_eq!(engine.state.position_ms, 7_500);
        assert!(
            engine
                .track_edit_status(track_id, Some("playlist"))
                .excluded_from_automatic_playback
        );
        drop(directory);
    }

    #[test]
    fn all_excluded_automatic_queues_have_no_target() {
        let first = "0abcdefghijklmnopqrstu";
        let second = "1abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(first, "playlist:playlist"),
            contextual_track(second, "playlist:playlist"),
        ];
        let (directory, store) =
            store_with_exclusions(&[("playlist", first), ("playlist", second)]);
        assert_eq!(first_automatic_from(&queue, 0, &store), None);
        assert_eq!(first_automatic_wrapping(&queue, 1, &store), None);
        assert_eq!(
            sequential_automatic_index(&queue, 0, RepeatMode::Context, &store),
            None
        );
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.repeat = RepeatMode::Context;
        assert_eq!(
            engine.take_next_index_with_skip(false, false),
            None,
            "automatic progression has no target when every row is excluded"
        );
        drop(directory);
    }

    #[test]
    fn playlist_exclusions_are_isolated_from_album_and_other_playlists() {
        let track_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(track_id, "playlist:one"),
            contextual_track(track_id, "album:one"),
            contextual_track(track_id, "playlist:two"),
        ];
        let (directory, store) = store_with_exclusions(&[("one", track_id)]);

        assert!(!automatic_track_eligible(&store, &queue[0]));
        assert!(automatic_track_eligible(&store, &queue[1]));
        assert!(automatic_track_eligible(&store, &queue[2]));
        assert_eq!(first_automatic_from(&queue, 0, &store), Some(1));
        drop(directory);
    }

    #[test]
    fn restore_preserves_an_excluded_requested_row() {
        let excluded_id = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track(excluded_id, "playlist:playlist"),
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        drop(store);
        let (mut engine, _) = test_engine_in(directory.path().to_path_buf());

        assert_eq!(
            engine.restore_queue(queue, 0, 42_000, String::new(), 0, false, false),
            Ok(true)
        );
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "restore is not automatic playback and keeps the requested row"
        );
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(engine.current_needs_load);
        drop(directory);
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

    /// A paused seek issues no play, so nothing will ever arrive to disarm a
    /// guard set here. Leaving one armed made the engine deaf for the rest of
    /// the queue: every genuine Paused was swallowed and the next genuine
    /// Playing was inverted into a pause with no listening row.
    #[tokio::test(flavor = "current_thread")]
    async fn a_paused_seek_leaves_no_guard_behind() {
        let (mut engine, _probe) = engine_with_player();
        let uri = track_uri();
        engine.state.playing = false;

        assert_eq!(engine.seek_transport(41_000), Ok(true));
        assert!(
            !engine.seek_in_flight,
            "a paused seek has nothing in flight"
        );

        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri.clone(),
            position_ms: 41_000,
        }));
        assert!(
            engine.state.playing,
            "a genuine Playing event is believed, not inverted"
        );

        assert!(engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri,
            position_ms: 42_000,
        }));
        assert!(!engine.state.playing, "a genuine Paused event is delivered");
    }

    #[test]
    fn playing_seek_accepts_the_playing_event_after_pause() {
        let mut engine = playing_engine();
        let uri = track_uri();
        engine.seek_in_flight = true;

        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: uri,
            position_ms: 42_000,
        }));
        assert!(engine.state.playing);
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(!engine.seek_in_flight);
    }

    /// The three load flags that used to suppress the librespot pause, each on
    /// its own. They say "a load is pending, has failed, or ran off the end" —
    /// none of them says librespot stopped, and when that inference was wrong
    /// the audio kept playing while the engine, and therefore the UI, was
    /// certain it had stopped. Nothing afterwards reconciled the two, so the
    /// only way out was a restart. A pause now performs the stop before it
    /// reports one, and fails outright when it cannot reach the player.
    #[tokio::test(flavor = "current_thread")]
    async fn a_pause_is_reported_only_once_librespot_has_been_told_to_stop() {
        let arms: [(&str, fn(&mut Engine)); 3] = [
            ("a pending load", |engine| engine.current_needs_load = true),
            ("a failed load", |engine| engine.loading_failed = true),
            ("a decoder EOF", |engine| engine.loop_decoder_eof = true),
        ];

        for (arm, set_flag) in arms {
            let mut unreachable = playing_engine();
            unreachable.state.playing = true;
            set_flag(&mut unreachable);
            assert!(
                unreachable.pause().is_err(),
                "{arm}: a pause with no player to command must fail"
            );
            assert!(
                unreachable.state.playing,
                "{arm}: the engine must not report a pause it never performed"
            );

            let (mut engine, _probe) = engine_with_player();
            engine.state.playing = true;
            set_flag(&mut engine);
            assert_eq!(
                engine.pause(),
                Ok(true),
                "{arm}: the pause reaches the player instead of being skipped"
            );
            assert!(!engine.state.playing, "{arm}");
        }
    }

    /// A track change carries the transport intent across, the way `previous`
    /// always has. Loading with a fixed `true` started audible playback the
    /// engine still called paused, and left the disagreement to be settled by
    /// whichever player event happened to arrive next.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_change_carries_the_transport_intent_across() {
        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert_eq!(paused.advance(false), Ok(true));
        assert_eq!(paused.state.current_index, Some(1));
        assert!(
            !paused.state.playing,
            "skipping ahead in a paused queue must not start playback"
        );

        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert_eq!(playing.advance(false), Ok(true));
        assert_eq!(playing.state.current_index, Some(1));
        assert!(playing.state.playing);
    }

    /// The same rule under a runtime failure: skipping past an unavailable
    /// track resumes a queue that was playing and leaves a paused one paused.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_track_hands_its_transport_intent_to_the_replacement() {
        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert!(playing.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(playing.state.current_index, Some(1));
        assert!(
            playing.state.playing,
            "a failure mid-playback continues with the next track"
        );

        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert!(paused.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(paused.state.current_index, Some(1));
        assert!(
            !paused.state.playing,
            "a load that failed while paused must not start the replacement"
        );
    }

    #[test]
    fn loop_marker_reloads_when_decoder_eof_arrived_first() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
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
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
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
            engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
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
            engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
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
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
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

    /// The first invalid-session tick must not create the audible stall: an
    /// active queue keeps projecting until replacement handles are ready.
    #[tokio::test(flavor = "current_thread")]
    async fn active_session_reconnect_preserves_playback_until_handover() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.position_anchor = Some((10_000, Instant::now() - Duration::from_secs(2)));
        engine.play_request_id = Some(7);
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        session.shutdown();
        engine.session = Some(session);
        let generation = engine.generation;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine.player_rebuild_is_current(generation, false),
            "a rebuild started while the session was ready is initially current"
        );

        assert!(engine.tick_session_health(&sender));
        assert!(
            !engine.player_rebuild_is_current(generation, false),
            "a queued rebuild on the dead session must be rejected during backoff"
        );
        assert!(!engine.state.ready);
        assert!(engine.state.playing, "the logical transport must not blip");
        assert!(engine.resume_after_reconnect);
        assert!(
            engine.tick_position(),
            "the dead session does not freeze projection"
        );
        assert!(
            (11_900..12_500).contains(&engine.state.position_ms),
            "position must still project from the pre-drop anchor: {}",
            engine.state.position_ms
        );
        assert_eq!(
            engine.generation, generation,
            "scheduling alone does not stale events"
        );
        assert!(engine.set_normalisation(true, &sender));
        assert!(
            receiver.try_recv().is_err(),
            "backoff must not rebuild a player on the dead session"
        );

        // Exercise the synchronous transition used when the reconnect becomes
        // due without spawning a network task.
        let reconnect_generation = engine.begin_cached_authentication(true);
        assert_ne!(reconnect_generation, generation);
        assert!(engine.auth_running);
        assert!(engine.state.playing);
        assert!(
            engine.session.is_some(),
            "old handles survive authentication"
        );
        assert!(engine.set_normalisation(false, &sender));
        assert!(
            receiver.try_recv().is_err(),
            "authentication must consume the shared preference without a competing rebuild"
        );
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation,
            event: PlayerEvent::Playing {
                play_request_id: 7,
                track_id: track_uri(),
                position_ms: 1,
            },
        }));

        let before_handover = engine.state.position_ms;
        engine.stop_playback_for_reconnect_handover();
        assert!(engine.state.position_ms >= before_handover);
        assert!(
            engine.state.playing,
            "handover preserves captured resume intent"
        );
        assert!(
            engine.session.is_none(),
            "the old session is stopped at handover"
        );
    }

    /// The whole point of the preserved handover is that the overlap ends the
    /// instant the replacement arrives. An old player left alive would keep
    /// feeding the audio device while pause, next and seek all reached the new
    /// one — audible playback no transport control can touch, until the app is
    /// restarted. Assert the old handles are gone, not merely forgotten.
    #[tokio::test(flavor = "current_thread")]
    async fn preserved_handover_leaves_exactly_one_live_player() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.play_request_id = Some(7);

        let old_sink = SinkProbe::new();
        let (old_player, old_session) = probe_player(&old_sink);
        let old_player_handle = Arc::downgrade(&old_player);
        engine.player = Some(old_player);
        engine.session = Some(old_session.clone());
        old_session.shutdown();

        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine.tick_session_health(&auth_sender),
            "the invalid session is noticed"
        );
        assert!(engine.resume_after_reconnect);
        let generation = engine.begin_cached_authentication(true);
        assert!(
            old_player_handle.upgrade().is_some(),
            "the old player plays on while the replacement is built"
        );

        let new_sink = SinkProbe::new();
        let (new_player, new_session) = probe_player(&new_sink);
        let new_player_handle = Arc::downgrade(&new_player);
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation,
                result: Ok(playback_handles(new_player, new_session)),
            },
            player_sender,
        ));

        assert!(
            old_player_handle.upgrade().is_none(),
            "the old player is dropped at handover, not merely forgotten"
        );
        assert!(
            !old_sink.is_alive(),
            "the old player's audio sink dies with it"
        );
        let installed = engine
            .player
            .as_ref()
            .expect("the replacement is installed");
        assert!(
            new_player_handle
                .upgrade()
                .is_some_and(|new| Arc::ptr_eq(installed, &new)),
            "the installed player is the replacement"
        );
        assert_eq!(
            Arc::strong_count(installed),
            1,
            "the engine owns the only reference to the live player"
        );
        assert!(engine.state.playing, "the resume intent survived");
        assert!(new_sink.is_alive());
    }

    /// The same teardown has to run when the attempt began preserved but the
    /// resume intent did not survive it: assigning the replacement over a live
    /// player would silently leave the old one playing and the old session
    /// unshut.
    #[tokio::test(flavor = "current_thread")]
    async fn a_completion_without_resume_intent_still_tears_the_old_player_down() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);

        let old_sink = SinkProbe::new();
        let (old_player, old_session) = probe_player(&old_sink);
        let old_player_handle = Arc::downgrade(&old_player);
        engine.player = Some(old_player);
        engine.session = Some(old_session.clone());
        let generation = engine.begin_cached_authentication(true);
        engine.resume_after_reconnect = false;

        let new_sink = SinkProbe::new();
        let (new_player, new_session) = probe_player(&new_sink);
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation,
                result: Ok(playback_handles(new_player, new_session)),
            },
            player_sender,
        ));

        assert!(
            old_player_handle.upgrade().is_none(),
            "the old player is dropped rather than assigned over"
        );
        assert!(!old_sink.is_alive());
        assert!(
            old_session.is_invalid(),
            "the old session is shut down, not merely dropped: a clone held by \
             an in-flight browse would otherwise keep it connected"
        );
        assert!(!engine.state.playing);
        assert_eq!(
            Arc::strong_count(
                engine
                    .player
                    .as_ref()
                    .expect("the replacement is installed")
            ),
            1,
        );
    }

    /// Paused reconnects retain the existing cold teardown and exponential
    /// retry schedule; no cached credentials means the normal login state.
    #[tokio::test(flavor = "current_thread")]
    async fn paused_session_reconnect_remains_cold_and_uses_backoff() {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        session.shutdown();
        engine.session = Some(session);
        let generation = engine.generation;
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.tick_session_health(&sender));
        assert!(!engine.resume_after_reconnect);
        assert!(!engine.tick_session_health(&sender));
        assert_eq!(engine.generation, generation);

        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_session_health(&sender));
        assert_ne!(engine.generation, generation, "the due reconnect must fire");
        assert!(!engine.auth_running, "no implicit OAuth flow may start");
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.session.is_none());
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
        assert!(
            !engine.state.queue[0]
                .effective_edit
                .as_ref()
                .unwrap()
                .cuts
                .iter()
                .any(|cut| {
                    cut.start_ms <= engine.state.position_ms
                        && engine.state.position_ms < cut.end_ms
                })
        );
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
            "renderer-compiled-restore-test-{}-{}",
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
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
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
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
    }

    #[test]
    fn startup_without_cached_credentials_enters_needs_login_without_a_flow() {
        let (mut engine, buffer) = test_engine(); // cache has no credentials
        // Even if restored state carries stale play intent, normal startup is
        // deliberately cold rather than using reconnect preservation.
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.resume_after_reconnect = true;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.start_authentication(sender);
        assert!(!engine.auth_running, "no implicit flow may start");
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
        assert!(engine.state.auth_url.is_some());
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());
        assert!(!engine.resume_after_reconnect);

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert!(
            value["auth_url"]
                .as_str()
                .is_some_and(|url| { url.starts_with("https://accounts.spotify.com/authorize?") })
        );
    }

    #[test]
    fn login_is_a_noop_while_a_session_is_live() {
        let mut engine = playing_engine(); // ready with a player-less state
        engine.state.auth_state = renderer_engine::protocol::AuthState::Ready;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine
                .login(&sender)
                .expect("login no-ops when authenticated")
        );
        assert!(!engine.auth_running);
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Ready);
    }

    #[test]
    fn login_is_a_noop_while_a_flow_is_already_running() {
        let mut engine = test_engine().0;
        engine.auth_running = true;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?running".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            engine
                .login(&sender)
                .expect("login no-ops while authenticating")
        );
        assert!(engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some("https://accounts.spotify.com/authorize?running"),
            "an in-flight attempt keeps its URL"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalisation_change_during_auth_updates_shared_preference_without_reconnect() {
        let mut engine = test_engine().0;
        assert!(!engine.normalisation.load(Ordering::Acquire));
        let stale_session =
            librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        stale_session.shutdown();
        engine.session = Some(stale_session);
        engine.state.ready = false;
        engine.auth_running = true;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        let generation = engine.generation;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.set_normalisation(true, &sender));
        assert!(engine.normalisation.load(Ordering::Acquire));
        assert!(
            engine.auth_running,
            "the existing authentication keeps running"
        );
        assert_eq!(
            engine.generation, generation,
            "changing the pending preference must not stale authentication"
        );
        assert!(
            receiver.try_recv().is_err(),
            "an unavailable session must not start a competing player rebuild"
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
            renderer_engine::protocol::AuthState::NeedsLogin,
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
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating);
        assert!(engine.auth_running);
        assert_eq!(engine.state.auth_url.as_deref(), Some(published.as_str()));
        assert!(engine.pending_auth.is_none());
    }

    #[test]
    fn begin_login_flow_prepares_a_fresh_attempt_when_none_is_pending() {
        let mut engine = test_engine().0;
        engine.state.auth_state = renderer_engine::protocol::AuthState::NeedsLogin;
        let pending = engine.begin_login_flow().expect("flow begins");
        assert!(
            pending
                .auth_url
                .starts_with("https://accounts.spotify.com/authorize?")
        );
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some(pending.auth_url.as_str())
        );
    }

    #[test]
    fn failed_auth_signal_returns_to_needs_login_with_a_fresh_url() {
        let mut engine = test_engine().0;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?first".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err(AuthFailure::Rejected(
                    "Spotify authentication failed: test".to_owned(),
                )),
            },
            sender,
        ));
        assert!(engine.state.auth_state == renderer_engine::protocol::AuthState::NeedsLogin);
        assert!(!engine.state.ready);
        assert!(!engine.auth_running);
        let retry_url = engine.state.auth_url.clone().expect("retry url published");
        assert_ne!(
            retry_url, "https://accounts.spotify.com/authorize?first",
            "a retry must regenerate the URL"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|error| { error.contains("Spotify authentication failed") })
        );
    }

    /// Waking from sleep reaches the first connect before Windows has a
    /// resolver, and librespot reports that as a plain `Unknown` error around
    /// `no such host is known`. Answering it with a login prompt threw away a
    /// session that Spotify had never refused — the credentials were still
    /// good, which is why restarting the app signed the user straight back in.
    #[test]
    fn an_unreachable_spotify_keeps_the_session_and_arms_a_retry() {
        let mut engine = test_engine().0;
        engine.state = playback_state(180_000);
        engine.state.ready = false;
        engine.state.auth_state = renderer_engine::protocol::AuthState::Authenticating;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();

        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err(AuthFailure::Unreachable(
                    "could not reach Spotify: no such host is known".to_owned(),
                )),
            },
            sender,
        ));

        assert!(
            engine.state.auth_state == renderer_engine::protocol::AuthState::Authenticating,
            "a network failure must not present as a logged-out session"
        );
        assert!(engine.state.auth_url.is_none(), "no login is being asked for");
        assert!(!engine.state.ready);
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "the queue survives an outage"
        );
        assert!(
            engine.next_reconnect.is_some(),
            "another attempt must be armed, or nothing would retry"
        );
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
    }

    /// The retry above leaves no session behind, so the health tick cannot look
    /// for a dead one; it has to notice the armed retry instead.
    #[test]
    fn the_health_tick_retries_when_only_a_retry_is_armed() {
        let mut engine = test_engine().0;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.session = None;
        engine.next_reconnect = Some(Instant::now() - Duration::from_millis(1));

        engine.awaiting_transport_retry = true;
        let generation = engine.generation;
        assert!(
            engine.tick_session_health(&sender),
            "a due retry with no session must still fire"
        );
        assert_ne!(engine.generation, generation, "the attempt actually started");
        assert!(engine.reconnect_backoff > RECONNECT_BACKOFF_MIN);
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
