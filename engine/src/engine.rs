use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_metadata::Metadata;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::audio::{self, AudioSignal};
use crate::auth::{
    AUDIO_START_TIMEOUT, AuthFailure, ConnectedSession, OauthListener, PendingAuth, PlaybackError,
    PlaybackHandles, bind_oauth_listener, complete_oauth, connect_cached, create_playback,
    percent_to_volume, prepare_oauth, stored_volume_percent,
};
use crate::customization::{EditTimeline, TrackEditStore, validate_definition};
use crate::history::ListeningHistory;
use crate::io::ProtocolWriter;
use renderer_engine::protocol::{
    AuthState, BrowseResponse, Command, HistoryPage, HistoryQuery, LoopRange, PositionEvent,
    RepeatMode, Response, StateEvent, TimeRange, TrackEdit, TrackEditDefinition, TrackEditStatus,
    TrackRef,
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
/// How many current-track load failures inside [`UNAVAILABLE_BURST_WINDOW`]
/// stop playback outright instead of skipping onward.
///
/// One failure is a track: skip it, which is what the engine has always done
/// and what a genuinely dead row still deserves. A run of them is not a queue
/// full of dead rows, it is the service refusing to serve this client at all —
/// Spotify's key service answers `error audio key 0 2`, librespot warns that it
/// is "continuing without decryption", and the decoder times out on the
/// resulting garbage about three seconds later. Skipping through that empties
/// the whole queue in silence at a key request per track, feeding the very
/// throttle that caused it, and it outlives the process because the throttle
/// is server-side. Three is the smallest limit that still lets two genuinely
/// dead rows sit next to each other in an otherwise working queue.
const UNAVAILABLE_STOP_LIMIT: usize = 3;

/// How long a failed row waits before the engine loads it one more time.
///
/// The refusal this recovers from is transient by nature. Across five separate
/// episodes in the diagnostic log the key service answered `error audio key
/// 0 2` in tight bursts of a few minutes and then went back to working
/// perfectly; no track is individually cursed, and the same track that failed
/// plays on the next attempt once the bucket refills. Skipping straight past a
/// row on its first failure therefore throws away a track that was never
/// broken, and does it at the cost of another key request for the row after.
///
/// One retry, not a ladder of them. A longer backoff would ride out more of a
/// burst, but a queue that sits visibly dead for half a minute and then starts
/// is worse than one that says so: the honest answer to a burst is
/// [`UNAVAILABLE_STOP_LIMIT`], and the retry's job is only to separate a
/// momentary refusal from one. It also makes the breaker reachable without
/// spending the queue — the first row's two failures plus the next row's one
/// reach the limit having skipped a single track.
const LOAD_RETRY_BACKOFF: Duration = Duration::from_secs(3);

/// How much of a track may be left unplayed for its end to still be the end.
///
/// librespot reports `EndOfTrack` for a track that finished and for a track
/// whose decoder collapsed mid-stream (`player.rs`: "Skipping to next track,
/// unable to decode samples" and "...unable to get next packet" both send
/// `EndOfTrack`), and nothing in the event tells them apart. Position does:
/// a track that ended is at its end. The allowance covers the output buffer
/// the engine's projected playhead trails the decoder by, which is
/// [`crate::audio`]'s write-ahead plus the device's own queue — milliseconds,
/// not seconds — and is set an order of magnitude above that because the
/// failure it has to separate from leaves minutes unplayed, not seconds.
const ABNORMAL_END_REMAINDER_MS: u32 = 5_000;

/// How little of the current track may remain before its successor is worth
/// an audio key.
///
/// A preload costs exactly one key request, the same as a play, and it is the
/// one request the user did not ask for. Issuing it on every track change —
/// which is what this engine used to do, from seven call sites — doubled the
/// draw on Spotify's key service, and doubled it hardest during the behaviour
/// that provokes the service in the first place. The diagnostic log shows the
/// shape plainly: seven track changes in 57 seconds costing fourteen loads,
/// with the first `error audio key 0 2` arriving on the fourteenth, and every
/// one of those five key-error episodes preceded by minutes of load rates ten
/// to twenty times the 1-2/min baseline. Each click also threw away the
/// preload the previous click had just paid for.
///
/// A watermark makes churn free without measuring it. A gapless preload exists
/// to smooth a track that is about to *finish*; a track the user started two
/// seconds ago is not about to finish, and if they are skipping it never will,
/// so the watermark is simply never reached and no request is made. Nothing is
/// rate-estimated, debounced, or reverse-engineered from the burst data — the
/// data does not support a threshold, and none is needed.
///
/// Thirty seconds is librespot's own answer (`player.rs`:
/// `PRELOAD_NEXT_TRACK_BEFORE_END_DURATION_MS`), which is what its preload
/// path is built to work with: resolve the file, fetch the key, download and
/// parse enough to construct a decoder, then hold it. Two local reasons argue
/// for that end of "tens of seconds" rather than a tighter one. The playhead
/// this is measured against is a projection that trails the decoder by the
/// output buffer, and playback speed rescales the whole thing — at 2x, thirty
/// seconds of track is fifteen seconds of wall clock to get the work done in.
const PRELOAD_WATERMARK_MS: u32 = 30_000;

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

/// How long the heartbeat waits before trying the output device again after a
/// failed open, and the ceiling that wait doubles up to.
///
/// These are short because this wait is the delay between the owner plugging
/// the dongle back in and hearing music, and a probe costs one device
/// enumeration when nothing has changed — no network, no audio key, no track
/// load. The first retry is a heartbeat or two after the device would have
/// landed; the ceiling keeps a machine that has been without audio since boot
/// from enumerating WASAPI every tick, and is low enough that a dongle Windows
/// recognises late still plays without anyone pressing anything.
const AUDIO_PROBE_BACKOFF_MIN: Duration = Duration::from_secs(2);
const AUDIO_PROBE_BACKOFF_MAX: Duration = Duration::from_secs(10);

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
    /// tracks between the failures. Its length is also the circuit breaker's
    /// count (see [`UNAVAILABLE_STOP_LIMIT`]) and, while it is non-empty, the
    /// reason next-track preloading is held back.
    recent_unavailable: VecDeque<Instant>,
    /// Whether the current load has been heard from — see
    /// [`AudioSignal::Output`]. Cleared by every load, set by the first packet
    /// the decoder produces for it. This is the engine's only truthful answer
    /// to "is this track actually playing", and the difference between a track
    /// that ended and a track that never started.
    current_load_produced_audio: bool,
    /// When the current row should be loaded once more after failing, and
    /// whether the load now in flight already is that retry. See
    /// [`LOAD_RETRY_BACKOFF`]. Both are cleared by any load, so a user action
    /// that moves the playhead cancels a pending retry by construction.
    retry_current_at: Option<Instant>,
    current_load_is_retry: bool,
    /// Whether librespot has said the current track is close enough to its end
    /// for the next one to be worth fetching. See [`Engine::preload_next`].
    preload_armed: bool,
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
    /// The output device every player this engine builds opens. Held rather
    /// than reached for, so a machine with no output device is a state a test
    /// can put the engine in (see [`audio::SinkOpener`]) — real hardware
    /// cannot be asked to boot without one.
    audio_device: audio::SinkOpener,
    /// Set while the machine has no output device to open. See
    /// [`AudioUnavailable`].
    audio_unavailable: Option<AudioUnavailable>,
    /// How long the heartbeat waits before probing for an output device
    /// again, doubling per failed probe up to [`AUDIO_PROBE_BACKOFF_MAX`].
    audio_probe_backoff: Duration,
}

/// The machine has no output device the engine can open.
///
/// This is deliberately not an error *state*: `state.ready` stays true, the
/// session stays connected, and the only thing missing is the player. The
/// device is a property of the machine that the user changes without telling
/// the engine, so the engine asks again on its heartbeat until one answers.
struct AudioUnavailable {
    /// What the user is shown: the cause and the way out.
    message: String,
    /// The next moment the heartbeat may open a device. A probe already running
    /// holds this at the instant its own open must have finished by
    /// ([`AUDIO_START_TIMEOUT`]), so one runs at a time — and a probe whose
    /// answer is dropped, because the session was replaced while it ran, is
    /// still replaced by another instead of leaving the device untried for
    /// good.
    retry_at: Instant,
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

/// What one current-track load failure says about the run it belongs to. The
/// two answers are kept apart because they protect different things and
/// disagree on purpose: the second failure in a burst already suppresses cache
/// eviction, long before enough of them have piled up to stop playback.
struct UnavailableBurst {
    /// Whether cached audio for the failed track must be preserved: a
    /// clustered failure is evidence about the service, not about the file.
    clustered: bool,
    /// Failures in the current run, this one included. Reset by the first
    /// load that succeeds (see [`Engine::clear_unavailable_burst`]) and by the
    /// burst window expiring.
    consecutive: usize,
}

pub enum AuthSignal {
    Complete {
        generation: u64,
        result: Result<ConnectedSession, AuthFailure>,
    },
    PlayerRebuilt {
        generation: u64,
        normalisation: bool,
        result: Result<PlaybackHandles, PlaybackError>,
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
        audio_device: audio::SinkOpener,
    ) -> Self {
        let history_root = state_directory.clone();
        let track_edits = TrackEditStore::load_or_empty(&state_directory);
        let random_state = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15)
            ^ u64::from(std::process::id());
        let volume = stored_volume_percent(&cache);
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
                volume,
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
            current_load_produced_audio: false,
            retry_current_at: None,
            current_load_is_retry: false,
            preload_armed: false,
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
            audio_device,
            audio_unavailable: None,
            audio_probe_backoff: AUDIO_PROBE_BACKOFF_MIN,
        }
    }

    pub fn history(&self, request: &HistoryQuery) -> Result<HistoryPage, String> {
        self.listening_history.page(request)
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
        // decision, so fix up those cheap plans immediately.
        self.repair_shuffle_pool_for_eligibility();
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
            // Intent is playing but no packet has come out of the decoder for
            // this load yet, or the load has failed and the row is being
            // retried. Both are the same thing to the listener: the playhead in
            // this message is where playback will start, not a position
            // anything has reached, and the UI must therefore hold its local
            // projection still instead of counting on from it. A paused row is
            // not buffering — its position is simply frozen, which the UI
            // already handles from `playing`.
            buffering: self.state.playing
                && (self.loading_failed || !self.current_load_produced_audio),
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
            upcoming: self.upcoming_indices(),
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
    ///
    /// A load that has produced no audio yet is the same case for the same
    /// reason, and it is the one that matters most: nothing is playing until
    /// the decoder has handed the pipeline its first packet, so nothing may
    /// advance the playhead. The projection is what the retry, a later Play,
    /// and every reported position read, and it clamps at the row's own
    /// duration — so a load that fails silently (a refused audio key, a
    /// truncated fetch) had already been walked to the end of a short row by
    /// the time the failure arrived. The engine then called that a finished
    /// track, or retried the row by asking librespot to load it at its own
    /// end: no audio either time, a second failure the row never earned, and a
    /// healthy one-second track skipped as unavailable. A run of them is
    /// exactly the queue that appeared to march through itself in silence.
    /// Freezing until the first packet keeps the projection on the last thing
    /// that was actually true — the position the load was asked for — and
    /// [`Engine::on_audio_signal`] re-anchors it there when audio really
    /// starts.
    ///
    /// A load that *failed* stays frozen on the same principle: the row is not
    /// playing, and for a decoder that died mid-track the frozen position is
    /// the point it died at, which is where the retry should resume.
    pub fn tick_position(&mut self) -> bool {
        if !self.state.playing || self.loading_failed || !self.current_load_produced_audio {
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

    /// Probes for an output device while the machine has none, on the same
    /// heartbeat that advances the playhead.
    ///
    /// Returns whether the engine's state changed and should be emitted.
    ///
    /// A probe is one device enumeration, and the rebuild it eventually starts
    /// is the same one a normalisation change performs; the only unusual thing
    /// about it is that it runs while a session is already up. Its own answer
    /// re-arms the clock — success clears the state, failure reschedules with
    /// the next backoff — so nothing here needs a timer of its own.
    pub fn tick_audio_device(&mut self, sender: &mpsc::UnboundedSender<AuthSignal>) -> bool {
        let Some(unavailable) = self.audio_unavailable.as_ref() else {
            return false;
        };
        if Instant::now() < unavailable.retry_at {
            return false;
        }
        let Some(session) = self.session.clone() else {
            // Nothing to build a player against; the session's own retry
            // decides when that changes.
            return false;
        };
        // The probe now owns the clock for as long as its open may take, which
        // is what keeps a second one from starting beside it.
        if let Some(unavailable) = self.audio_unavailable.as_mut() {
            unavailable.retry_at = Instant::now() + AUDIO_START_TIMEOUT;
        }
        let generation = self.generation;
        let cache = self.cache.clone();
        let normalisation = Arc::clone(&self.normalisation);
        let audio = Arc::clone(&self.audio_device);
        let sender = sender.clone();
        tokio::spawn(async move {
            let enabled = normalisation.load(Ordering::Acquire);
            let result = create_playback(session, cache, enabled, audio).await;
            let _ = sender.send(AuthSignal::PlayerRebuilt {
                generation,
                normalisation: enabled,
                result,
            });
        });
        false
    }

    /// Enters the recoverable no-output-device state: no player, the session
    /// untouched, and the queue and playhead exactly as they were.
    ///
    /// Playback intent survives — `state.playing` is deliberately not touched —
    /// so a track that was playing when the device went away plays again when
    /// one comes back, which is the whole point of not treating this as a
    /// failure of the session.
    fn enter_audio_unavailable(&mut self, message: String) {
        self.detach_player();
        // Events still in flight from the detached player — the pause its own
        // stalled write caused, and the close that follows the player thread's
        // end — are about a player this engine has already let go. Without
        // this, the close would be read as the player dying and would take the
        // session down, which is the very thing this state exists to undo.
        self.generation = self.generation.wrapping_add(1);
        self.pause_listening();
        eprintln!("audio: {message}");
        self.state.error = Some(message.clone());
        self.audio_unavailable = Some(AudioUnavailable {
            message,
            retry_at: Instant::now() + self.audio_probe_backoff,
        });
        self.audio_probe_backoff = (self.audio_probe_backoff * 2).min(AUDIO_PROBE_BACKOFF_MAX);
    }

    /// A player exists, so whatever was wrong with the output device is over:
    /// drop the message and start the next outage at the short backoff.
    fn clear_audio_unavailable(&mut self) {
        self.audio_unavailable = None;
        self.audio_probe_backoff = AUDIO_PROBE_BACKOFF_MIN;
    }

    /// Whether `command` can be applied while the machine has no output device.
    ///
    /// The queue, the volume, the shuffle mode and the play/pause intent are
    /// engine state that a player is built *from*; everything else needs a
    /// player to act on and is answered with the device's own message instead.
    /// Refusing these too would leave a machine that booted without audio with
    /// a player bar that cannot be filled and a startup restore the shell
    /// reports as failed — for a device that may appear a second later.
    ///
    /// `state.ready` is part of the question rather than an oversight: the
    /// exemption is from the *device* check, not from the session's. An engine
    /// without a session has nothing to fill in and answers with the reason it
    /// has no session.
    fn runs_without_output_device(&self, command: &Command) -> bool {
        self.state.ready
            && self.audio_unavailable.is_some()
            && matches!(
                command,
                Command::Play
                    | Command::Pause
                    | Command::SetVolume { .. }
                    | Command::SetShuffle { .. }
                    | Command::SetRepeat { .. }
                    | Command::RestoreQueue { .. }
            )
    }

    /// Clears the last error, unless the machine has no output device: nothing
    /// a command does brings the audio back, so a command that succeeds while
    /// it is in force has not made the silence go away. The message is released
    /// where the condition is — by the probe that opens a device, in
    /// [`Engine::clear_audio_unavailable`].
    fn clear_error(&mut self) {
        if self.audio_unavailable.is_none() {
            self.state.error = None;
        }
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
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = create_playback(session, cache, enabled, audio).await;
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
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = connect_cached(cache, temporary_directory, normalisation, audio).await;
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
        // The device state belongs to a live session: a machine that is being
        // asked to log in again is not also missing its output device as far as
        // the user or the next connection is concerned.
        self.clear_audio_unavailable();
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
    ///
    /// Returning `Ok` is a promise the caller relies on: the loopback callback
    /// port is bound by the time this returns, so the UI may open the browser
    /// the moment the command answers. Nothing about that ordering is left to
    /// the spawned task — it is handed a listener that is already listening.
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
        let (pending, listener) = self.begin_login_flow()?;
        let generation = self.generation;
        let auth_sender = auth_sender.clone();
        let cache = self.cache.clone();
        let temporary_directory = self.temporary_directory.clone();
        let normalisation = Arc::clone(&self.normalisation);
        let audio = Arc::clone(&self.audio_device);
        tokio::spawn(async move {
            let result = complete_oauth(
                cache,
                temporary_directory,
                pending,
                listener,
                normalisation,
                audio,
            )
            .await;
            let _ = auth_sender.send(AuthSignal::Complete {
                generation,
                result: result.map_err(AuthFailure::Rejected),
            });
        });
        Ok(true)
    }

    /// Synchronous half of [`Engine::login`]: binds the callback port, then
    /// consumes (or prepares) the OAuth attempt and marks the engine
    /// `Authenticating`. Split out so the transition is unit-testable without
    /// spawning a live flow.
    ///
    /// The bind comes first and its failure returns before anything is
    /// mutated. A port this application cannot have means the attempt is
    /// impossible, and the user must be told while still looking at the Log in
    /// button — not left in `Authenticating` waiting on a callback that has
    /// nowhere to land. Failing here also preserves the prepared attempt and
    /// the published authorize URL, so the next click is a clean retry.
    fn begin_login_flow(&mut self) -> Result<(PendingAuth, OauthListener), String> {
        let listener = bind_oauth_listener()?;
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
        Ok((pending, listener))
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
                    Ok(connected) => {
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
                        self.state.auth_url = None;
                        self.pending_auth = None;
                        // The session is live in both arms below, and stays
                        // live: a machine with no output device can browse,
                        // search and edit playlists, and the player is the only
                        // thing it cannot have yet.
                        self.session = Some(connected.session);
                        self.next_reconnect = None;
                        self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
                        self.awaiting_transport_retry = false;
                        match connected.playback {
                            Ok(handles) => {
                                self.clear_audio_unavailable();
                                self.state.volume = if had_queue {
                                    restored_volume
                                } else {
                                    handles.volume_percent
                                };
                                self.state.error = None;
                                self.player = Some(handles.player);
                                self.mixer = Some(handles.mixer);
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
                                Self::forward_player_events(
                                    handles.events,
                                    generation,
                                    player_sender,
                                );
                            }
                            Err(message) => {
                                // No output device. Everything a session is
                                // for still happened — the queue, the volume,
                                // the auth state — and the player arrives from
                                // the heartbeat once the machine has one.
                                self.enter_audio_unavailable(message);
                            }
                        }
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
                    // This is the device probe's answer, or a normalisation
                    // rebuild that ran into the same machine: either way the
                    // session is fine and only the device is missing, so the
                    // engine stays exactly where it was and asks again later.
                    Err(PlaybackError::NoOutputDevice(message)) => {
                        self.enter_audio_unavailable(message);
                        return true;
                    }
                    Err(error) => {
                        self.state.error = Some(format!(
                            "could not rebuild the audio player: {}",
                            error.message()
                        ));
                        return true;
                    }
                };
                self.clear_audio_unavailable();

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

    /// Records a current-track load failure and reports the two independent
    /// things the engine decides with. A second failure within the failure
    /// window is clustered even when the same track was retried; two or more
    /// recent load starts also protect the first failure observed after rapid
    /// clicks.
    fn record_unavailable(&mut self, now: Instant) -> UnavailableBurst {
        Self::prune_recent_times(
            &mut self.recent_track_changes,
            now,
            TRACK_CHANGE_BURST_WINDOW,
        );
        Self::prune_recent_times(&mut self.recent_unavailable, now, UNAVAILABLE_BURST_WINDOW);
        let clustered = self.recent_unavailable.len() >= 1
            || self.recent_track_changes.len() >= TRACK_CHANGE_BURST_MINIMUM;
        self.recent_unavailable.push_back(now);
        UnavailableBurst {
            clustered,
            consecutive: self.recent_unavailable.len(),
        }
    }

    /// A load that succeeded ends the current failure burst: a later isolated
    /// failure is again eligible for corrupt-cache cleanup, the
    /// [`UNAVAILABLE_STOP_LIMIT`] count starts over, and preloading resumes.
    ///
    /// "Succeeded" means audio came out, and nothing weaker. librespot's
    /// `Playing` used to be taken as the proof, and it is not one: a track
    /// loaded without its audio key reaches `Playing` exactly like a working
    /// track and is silent for the three seconds it survives. Resetting the
    /// count on that made the breaker count to one, forever, while the queue
    /// emptied itself.
    fn clear_unavailable_burst(&mut self) {
        self.recent_unavailable.clear();
    }

    /// Whether the `EndOfTrack` librespot just reported is a track that
    /// finished rather than one that broke.
    ///
    /// librespot sends the same event for both (`player.rs`: the decode-error
    /// and packet-error arms send `EndOfTrack`, as does real EOF), so the
    /// answer has to come from what the engine itself observed. Two things
    /// separate them, and a track has to pass both:
    ///
    /// - it produced audio at all, which a keyless track never does; and
    /// - it is at its end, which a decoder that collapsed mid-stream is not.
    ///
    /// The second test runs in the compiled timeline, so an edit that cuts the
    /// tail still ends where the listener hears it end, and is skipped
    /// entirely while a loop is active — a loop rewinds the source playhead on
    /// purpose, so its position at decoder EOF means nothing.
    fn end_of_track_is_genuine(&mut self) -> bool {
        if !self.current_load_produced_audio {
            return false;
        }
        if self.current_loop().is_some() {
            return true;
        }
        self.tick_position();
        let timeline = self.current_timeline();
        let played_ms = timeline.source_to_compiled(self.state.position_ms);
        timeline.compiled_duration_ms().saturating_sub(played_ms) <= ABNORMAL_END_REMAINDER_MS
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
        if matches!(&command, Command::GetHistory { .. }) {
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
        // A machine with no output device can still be *told* what to do: see
        // [`Engine::runs_without_output_device`]. Everything else needs a
        // player to act on — and gets the device's own message, not a generic
        // "player unavailable", when there is none.
        if !self.runs_without_output_device(&command) {
            self.ensure_ready()?;
        }
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
            | Command::GetHistory { .. }
            | Command::ClearHistory
            | Command::Shutdown
            | Command::Login
            | Command::Logout
            | Command::SetNormalisation { .. }
            | Command::BrowsePlaylists { .. }
            | Command::BrowsePlaylist { .. }
            | Command::BrowseRadio { .. }
            | Command::BrowsePlaylistRecommendations { .. }
            | Command::BrowseTrack { .. }
            | Command::BrowseAlbum { .. }
            | Command::BrowseArtist { .. }
            | Command::BrowseArtistSongwriter { .. }
            | Command::BrowseArtistCatalogue { .. }
            | Command::BrowseLikedSongs { .. }
            | Command::BrowseLikedUris { .. }
            | Command::BrowseSearch { .. }
            | Command::BrowseTrackCredits { .. }
            | Command::BrowseCanvas { .. }
            | Command::BrowseFollowedArtists
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
            Command::Next => self.advance_with_current_skip(false, false, true),
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
            // The one fact librespot's transport events cannot supply: this
            // load is real. Everything that has to tell a playing track from a
            // silent one hangs off it. Revision-gated like every audio signal,
            // so a packet from the pipeline a previous load left behind cannot
            // vouch for this one.
            AudioSignal::Output { revision } => {
                if revision != self.audio_revision {
                    return false;
                }
                if self.current_load_produced_audio {
                    return false;
                }
                self.current_load_produced_audio = true;
                // Audio starts here, so this is where the playhead starts
                // moving from: the position it holds is the offset the load was
                // asked for (or the last position an event reported for it),
                // because nothing has been allowed to project past it while
                // the load was silent. Re-anchoring is what makes that offset
                // the base of the drift rather than an instant that has already
                // expired, and the state change is what tells the UI its
                // buffering hold is over — the local projection may resume from
                // exactly this position.
                self.update_position(self.state.position_ms);
                true
            }
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
            // The output stopped draining mid-write: nothing has consumed the
            // ring for the whole write-drain timeout, which is what an
            // unplugged device looks like from inside the player thread.
            // librespot turns the resulting write error into a pause and says
            // nothing about the device, so this is the engine's only chance to
            // put the transport somewhere it can recover from.
            AudioSignal::OutputStalled { revision } => {
                if revision != self.audio_revision {
                    // A player this engine has already let go, reporting two
                    // seconds after the fact. Acting on it would tear down the
                    // player that replaced it.
                    return false;
                }
                if self.audio_unavailable.is_some() {
                    return false;
                }
                // A stalled write means librespot had audio in flight when the
                // device stopped taking it, so this track was playing. Read
                // that intent now rather than later: the pause librespot emits
                // for the same failure travels a different channel, the two can
                // be handled in either order, and a pause that got here first
                // would otherwise have cleared the very thing playback has to
                // resume from.
                let was_playing = self.state.current_index.is_some()
                    && (self.state.playing || self.current_load_produced_audio);
                self.state.playing = was_playing;
                self.enter_audio_unavailable(audio_stalled_message());
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
        if !self.state.ready {
            return Err(match self.state.auth_state {
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
            });
        }
        if self.player.is_some() {
            return Ok(());
        }
        // Ready with no player has exactly one cause now, and the user has to
        // act on it: say what it is and what fixes it.
        Err(self.audio_unavailable.as_ref().map_or_else(
            || "the local audio player is unavailable".to_owned(),
            |unavailable| unavailable.message.clone(),
        ))
    }

    fn player(&self) -> Result<&Arc<Player>, String> {
        self.player.as_ref().ok_or_else(|| {
            self.audio_unavailable.as_ref().map_or_else(
                || "the local audio player is unavailable".to_owned(),
                |unavailable| unavailable.message.clone(),
            )
        })
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
        // Not `state.error = None` directly: a startup restore on a machine
        // with no output device installs the queue perfectly well, and must not
        // take the message that says why there is no sound with it.
        self.clear_error();
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
        // Clicking a row asks to hear that row, not to be handed a different
        // random ordering of everything behind it, so the drawn plan survives
        // minus the row that just became current. The row being left is not put
        // back: `previous` owns that direction and pushes it there itself.
        self.repair_shuffle_pool(|pooled| Some(pooled));
        self.last_track_change = Some(Instant::now());
        self.load_current(true)?;
        Ok(true)
    }

    fn play(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        if self.audio_unavailable.is_some() {
            // There is no player to send this to, but the request itself is
            // worth keeping: `state.playing` is what the device probe starts
            // from when it finds one, so pressing play into a mute machine is
            // answered by music as soon as the machine can make any. The row is
            // marked as not loaded for the same reason a teardown marks it.
            self.state.playing = true;
            self.current_needs_load = true;
            eprintln!(
                "transport: play at {} ms with no output device; it starts when one appears",
                self.state.position_ms,
            );
            return Ok(true);
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
        if self.audio_unavailable.is_some() {
            // Nothing is audible, so there is nothing to stop — but the intent
            // still has to be cleared here, or the device probe would start
            // playing something the user asked to stop.
            let was_playing = self.state.playing;
            self.state.playing = false;
            self.pause_listening();
            self.update_position(self.state.position_ms);
            eprintln!(
                "transport: pause at {} ms with no output device (engine was {})",
                self.state.position_ms,
                if was_playing { "playing" } else { "paused" },
            );
            return Ok(true);
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
            // Symmetric with Next: the press is the request to hear it.
            self.load_current(true)?;
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
        self.advance_with_current_skip(at_end, false, false)
    }

    /// `resume` is what separates a press of Next from the two advances the
    /// engine makes on its own. Pressing Next asks to *hear* the next track, so
    /// it starts a paused queue playing; a natural boundary and an unavailable
    /// track carry the transport intent across instead, because neither is a
    /// request to start playback.
    fn advance_with_current_skip(
        &mut self,
        at_end: bool,
        skip_current_for_repeat: bool,
        resume: bool,
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
                // Either way the load and `state.playing` agree, which is the
                // part that matters: loading with a fixed `true` used to leave
                // the engine calling itself paused while librespot played the
                // new track, to be reconciled by whichever event arrived next.
                let start_playing = resume || self.state.playing;
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
        // A machine with no output device has no mixer and no sink to apply
        // this to, but the volume is still a real setting: it is persisted
        // here, reported in the state the UI reads, and applied by the player
        // the device probe builds.
        if let Some(mixer) = &self.mixer {
            mixer.set_volume(volume);
        }
        self.cache.save_volume(volume);
        // The audible volume lives on the rodio sink (per-packet attenuation
        // is disabled); apply it there so the change is heard immediately.
        crate::audio::set_sink_volume(volume);
        self.state.volume = percent;
        self.clear_error();
        Ok(true)
    }

    fn set_shuffle(&mut self, enabled: bool) -> Result<bool, String> {
        if self.state.shuffle == enabled {
            return Ok(false);
        }
        self.state.shuffle = enabled;
        self.history.clear();
        // Switching shuffle on is the one place a full draw is the point: there
        // is no earlier plan to preserve. Switching it off empties the bag by
        // the same call.
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
        self.splice_new_rows_into_shuffle_pool(self.state.queue.len() - 1);
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
        let first_new = self.state.queue.len();
        self.state.queue.extend(tracks);
        self.history.clear();
        self.splice_new_rows_into_shuffle_pool(first_new);
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
        // The removed row leaves the bag and everything above it slides down
        // one. Removing the current row also promotes a replacement that the
        // bag may already hold; `repair_shuffle_pool` drops whatever is current
        // by the time it runs, which is why this is called after the match.
        self.repair_shuffle_pool(|pooled| {
            if pooled == index {
                None
            } else if pooled > index {
                Some(pooled - 1)
            } else {
                Some(pooled)
            }
        });
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
        // A move is a permutation of the queue, so the bag is remapped through
        // the very function that just moved `current_index`. Writing the
        // arithmetic out a second time here would be an opportunity for the two
        // to disagree, and a bag that disagrees about where the current row
        // went will happily hand it back as the next track.
        self.repair_shuffle_pool(|pooled| Some(remap_current_index_after_move(pooled, from, to)));
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
        self.play_request_id = None;
        self.seek_in_flight = false;
        self.loop_decoder_eof = false;
        self.loop_jump_pending = false;
        let player = Arc::clone(self.player()?);
        self.loading_failed = false;
        self.current_needs_load = false;
        // This load is the newest thing the engine asked for, so it owns every
        // judgement the previous one left behind: nothing has been heard from
        // it yet, no preload is due for a track that just started, and any
        // retry armed for the row being replaced is moot. Clearing them here
        // rather than at each caller is what makes a user action — a click, a
        // seek, a press of Next — cancel a pending retry without anyone having
        // to remember to.
        self.current_load_produced_audio = false;
        self.preload_armed = false;
        self.retry_current_at = None;
        self.current_load_is_retry = false;
        // What the engine reports and what it asks librespot for are one
        // statement, made once, here. Callers used to set `playing` beside the
        // call, which let the two say different things — a track change loading
        // with a hardcoded `true` while the engine still called itself paused,
        // with nothing but the next incoming event to settle the argument.
        self.state.playing = start_playing;
        // The load starts playing at the offset it was asked for, so that
        // offset is the playhead's truth from this instant. Every drift
        // projection measures from the newest authoritative position, and
        // without this one it would keep measuring from the previous load's
        // anchor — seconds old when a retry follows a failure, which is
        // precisely the case where the two positions differ. Callers that set a
        // position do so because they are changing the answer, not because the
        // load handler demands one, so this cannot disagree with them.
        self.update_position(position_ms);
        self.note_track_change(Instant::now());
        player.load(uri, start_playing, position_ms);
        Ok(())
    }

    /// Asks librespot to fetch the next track, if now is a moment at which
    /// that is worth an audio key.
    ///
    /// Two independent gates, each closing on a different reason not to spend
    /// the request. [`PRELOAD_WATERMARK_MS`] holds it back until the current
    /// track is nearly over, which is what makes a user clicking through the
    /// queue generate no preloads at all. The failure burst holds it back
    /// while the key service is refusing this client, which is precisely when
    /// a second request per load is the one not to make; it lifts the moment a
    /// load is heard producing audio.
    ///
    /// Every queue mutation still calls this, so an edit made during the last
    /// thirty seconds of a track re-points the preload at whatever now plays
    /// next. Outside that window the call is free.
    fn preload_next(&self) {
        let (Some(player), Some(uri)) = (&self.player, self.preload_target()) else {
            return;
        };
        player.preload(uri);
    }

    /// The track a preload would ask for right now, and `None` when it should
    /// not ask at all. Split out from [`Engine::preload_next`] so both gates
    /// are answerable without a live player.
    fn preload_target(&self) -> Option<SpotifyUri> {
        if !self.preload_armed || !self.recent_unavailable.is_empty() {
            return None;
        }
        let next = self.peek_next_index()?;
        // `playable_track_uri`, not the bare parse: a row already known to be
        // permanently unavailable is not worth an audio key, and this is the
        // path that used to be two — one that checked and one that did not.
        playable_track_uri(&self.state.queue[next]).ok()
    }

    /// Whether the current track is close enough to its end to arm the
    /// preload. Answered in the compiled timeline, because what decides when
    /// the next track has to be ready is when the listener reaches the end of
    /// this one, not where the source file ends: an edit that cuts the tail
    /// brings that moment forward, and playback speed moves it either way.
    ///
    /// The floor is capped at half the track so no length is unpreloadable —
    /// a twenty-second interlude arms at ten seconds in. This mirrors
    /// `play_qualifies` in [`crate::history`], which caps its own threshold
    /// the same way and for the same reason.
    fn at_preload_watermark(&self) -> bool {
        let (position_ms, duration_ms) = self.transport_position_and_duration();
        if duration_ms == 0 {
            return false;
        }
        duration_ms.saturating_sub(position_ms) <= PRELOAD_WATERMARK_MS.min(duration_ms / 2)
    }

    /// The current row's load did not produce a playable track, however
    /// librespot chose to say so. Decides between retrying it, holding it,
    /// skipping it, and stopping, and reports whether state changed.
    ///
    /// Every caller reaches here with a different event — `Unavailable` for a
    /// load that gave up, `EndOfTrack` for one that lied about starting — and
    /// the same situation. Keeping the decision in one place is what stops the
    /// second kind from being handled, as it was, as a track that finished.
    fn fail_current_load(&mut self, track_id: SpotifyUri) -> bool {
        let burst = self.record_unavailable(Instant::now());
        self.seek_in_flight = false;
        self.loading_failed = true;
        self.finalize_listening(false);
        // `state.playing` is left alone deliberately: it is the intent the
        // retry or skip below has to carry onto whatever loads next. Every
        // branch settles it — a successor loads with this intent, a retry
        // reloads with it, and an exhausted queue stops.
        self.state.error = Some(format!("Spotify track is unavailable: {track_id}"));

        // librespot leaves the failed loader in PlayerState::Loading after
        // sending Unavailable. Stop it explicitly so a later Play can submit a
        // fresh Load command instead of toggling start_playback on a
        // terminated future.
        if let Some(player) = &self.player {
            player.stop();
        }
        // That stop has a `Stopped` event behind it, and the engine must not
        // still be listening for one: the failed play request is dead to every
        // branch below — each either reloads or waits for the owner — and the
        // arm that handles `Stopped` would otherwise clear `playing` moments
        // after this, quietly disarming the retry the engine just promised.
        self.play_request_id = None;
        self.invalidate_audio_signals();

        // An isolated failure can be a corrupt/truncated cache entry;
        // librespot's decoder retry handles one cached format and this removes
        // every format before the next user retry. Once failures cluster,
        // preserve all cache files: key-service or network failures are not
        // evidence of corruption.
        if !burst.clustered {
            self.evict_track_audio_cache(track_id.clone());
        }

        // Pause has to win here, and it used to lose. librespot runs a load to
        // completion whether or not it was told to start playing, so the
        // failure of a paused load arrived exactly like the failure of a
        // playing one and skipped onward all the same: the queue walked itself
        // silently, track after track, with the pause button visibly doing
        // nothing. A paused engine has no continuity to protect, so hold the
        // failed row and let the owner choose — Play retries it, Next moves
        // past it. It is also what disarms the retry below, since the tick
        // only fires one for a queue that is playing.
        if !self.state.playing {
            eprintln!("transport: load failed for {track_id} while paused; holding this row");
            return true;
        }

        // A run of failures is the service refusing this client, not a run of
        // bad rows, so stop and say so rather than spending the rest of the
        // queue finding that out one track at a time.
        if burst.consecutive >= UNAVAILABLE_STOP_LIMIT {
            self.state.playing = false;
            // The banner in App.svelte ellipsises a long error, so the part
            // that says what happened comes first.
            self.state.error = Some(format!(
                "Spotify refused audio for {} tracks in a row; playback stopped",
                burst.consecutive
            ));
            eprintln!(
                "transport: {} load failures within {} s; stopping instead of skipping past {track_id}",
                burst.consecutive,
                UNAVAILABLE_BURST_WINDOW.as_secs(),
            );
            return true;
        }

        // The row gets one more attempt before it is given up on. See
        // [`LOAD_RETRY_BACKOFF`]: the refusal is transient, the track is
        // usually fine, and skipping on the first failure both discards a
        // working track and pays another key request to discover the next one
        // is refused too.
        if !self.current_load_is_retry {
            self.retry_current_at = Some(Instant::now() + LOAD_RETRY_BACKOFF);
            // What the banner says has to be what is happening, and for the
            // next three seconds what is happening is a retry.
            self.state.error = Some("Spotify refused audio for this track; retrying".to_owned());
            eprintln!(
                "transport: load failed for {track_id}; retrying this row in {} s",
                LOAD_RETRY_BACKOFF.as_secs(),
            );
            return true;
        }

        // A runtime failure is an automatic progression opportunity: continue
        // with the next eligible row when one exists. The
        // `skip_current_for_repeat` guard prevents repeat-one from retrying the
        // same failed loader forever. With no candidate, the branch below
        // simply leaves this failed row stopped.
        if let Err(error) = self.advance_with_current_skip(false, true, false) {
            self.state.playing = false;
            self.state.error = Some(error);
        }
        true
    }

    /// Drives the two things a failing or recovering load needs a clock for.
    /// Called from the same heartbeat that advances the playhead, so no extra
    /// timer exists; both checks are a comparison on engine-local state.
    ///
    /// Returns whether the engine's state changed and should be emitted.
    pub fn tick_playback_health(&mut self) -> bool {
        // Audio is out: whatever the run of failures was, it is over.
        // Preloading and corrupt-cache cleanup come back with it.
        if self.current_load_produced_audio && !self.recent_unavailable.is_empty() {
            self.clear_unavailable_burst();
        }

        // The playhead this reads is a projection, so project it first rather
        // than depend on the caller having done so.
        self.tick_position();
        // Edge-triggered, and only for a track that is running: a paused track
        // is not approaching its end, and scrubbing back and forth across the
        // watermark must not buy the same key twice. A load is what re-arms
        // it, so resuming a track paused past the watermark reaches this on
        // the next tick.
        if !self.preload_armed && self.state.playing && self.at_preload_watermark() {
            self.preload_armed = true;
            self.preload_next();
        }

        // A retry belongs to a queue that is still playing. Pause clears
        // `state.playing` and that is the whole cancellation: the owner's
        // complaint last time was a pause button that could not stop the
        // cascade, and an armed timer that outlived it would be the same bug
        // wearing a different hat.
        let due = self
            .retry_current_at
            .is_some_and(|at| Instant::now() >= at && self.state.playing);
        if !due {
            return false;
        }
        // `load_current` clears both fields, so the flag is set afterwards:
        // it marks the load now in flight as the one that has already had its
        // second chance.
        if let Err(error) = self.load_current(true) {
            self.retry_current_at = None;
            self.state.playing = false;
            self.state.error = Some(error);
            return true;
        }
        self.current_load_is_retry = true;
        true
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

    /// The order the queue will actually play from here: shuffle's own plan
    /// when shuffle is on, the sequential walk when it is off, with exclusions
    /// and unavailable rows dropped either way. The current row is not part of
    /// it; the UI puts that at the head itself.
    ///
    /// Repeat-one is deliberately ignored. It loops one row for as long as it
    /// is set, which is a transport detail of the current track rather than a
    /// different plan for the queue behind it.
    fn upcoming_indices(&self) -> Vec<usize> {
        let Some(current) = self.state.current_index else {
            // Removals can leave a populated queue with no current row. Listing
            // every eligible row keeps the view honest instead of blank.
            return (0..self.state.queue.len())
                .filter(|index| self.automatic_track_eligible(*index))
                .collect();
        };
        if self.state.shuffle {
            // The pool is popped from the back, so reversed is play order. When
            // it empties under repeat-context the engine reshuffles into a
            // brand-new random order (see `take_next_index_with_skip`), which
            // nothing can know in advance — so the list simply stops there
            // rather than inventing a continuation.
            return self
                .shuffle_pool
                .iter()
                .rev()
                .copied()
                .filter(|index| self.automatic_track_eligible(*index))
                .collect();
        }
        let mut upcoming: Vec<usize> = (current.saturating_add(1)..self.state.queue.len())
            .filter(|index| self.automatic_track_eligible(*index))
            .collect();
        if self.state.repeat == RepeatMode::Context {
            // Inclusive of `current`, matching `sequential_automatic_index`'s
            // wrap exactly: when nothing later is eligible the walk restarts at
            // zero and may legitimately land back on the row playing now.
            upcoming.extend(
                (0..=current.min(self.state.queue.len().saturating_sub(1)))
                    .filter(|index| self.automatic_track_eligible(*index)),
            );
        }
        upcoming
    }

    /// Draws a brand-new random play order. This is the right answer only
    /// where the previous order has genuinely stopped existing: shuffle being
    /// switched on (there was no plan), a whole different queue being
    /// installed, and the repeat-context refill that starts a new lap. Every
    /// other mutation repairs the bag instead — see [`Engine::repair_shuffle_pool`].
    ///
    /// The bag it produces defines the four invariants every repair must also
    /// hold: it never contains `current_index`, never contains an index outside
    /// `state.queue`, never repeats an index, and is empty whenever shuffle is
    /// off.
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

    /// Carries the drawn order across a change in the queue's shape by mapping
    /// every entry through `remap` and dropping the ones that map to `None`.
    /// Survivors keep their relative order, which is the whole point: the pool
    /// is published as "up next", and redrawing it — which is what every queue
    /// mutation used to do — rescrambles the list the owner is looking at just
    /// because one unrelated row was added, removed, or dragged.
    ///
    /// `remap` must be injective over the bag, since nothing here can tell a
    /// collision apart from an honest duplicate; the range and current-row
    /// invariants are enforced below rather than trusted from each caller's
    /// arithmetic, because a bag entry that no longer indexes the queue is a
    /// panic waiting in `take_next_index_with_skip`.
    fn repair_shuffle_pool(&mut self, remap: impl Fn(usize) -> Option<usize>) {
        if !self.state.shuffle {
            // `rebuild_shuffle_pool` empties the bag when shuffle goes off, and
            // a repair must never be the thing that resurrects entries into it.
            self.shuffle_pool.clear();
            return;
        }
        let length = self.state.queue.len();
        let current = self.state.current_index;
        self.shuffle_pool = std::mem::take(&mut self.shuffle_pool)
            .into_iter()
            .filter_map(remap)
            .filter(|index| *index < length && Some(*index) != current)
            .collect();
    }

    /// Puts a newly eligible row into the drawn order at a uniformly random
    /// position, drawn from the same PRNG that shuffled the bag in the first
    /// place. Pushing it onto one end would be simpler, but the bag is popped
    /// from the back, so that would make a queued track either always play next
    /// or always play last — neither of which is what shuffle means.
    fn splice_into_shuffle_pool(&mut self, index: usize) {
        let position = (self.next_random() as usize) % (self.shuffle_pool.len() + 1);
        self.shuffle_pool.insert(position, index);
    }

    /// Folds rows appended at `first_new` and beyond into the existing drawn
    /// order. Only the tail of the queue is walked, because that is the only
    /// part an append can have changed — the indices already in the bag still
    /// name the same tracks.
    fn splice_new_rows_into_shuffle_pool(&mut self, first_new: usize) {
        if !self.state.shuffle {
            self.shuffle_pool.clear();
            return;
        }
        for index in first_new..self.state.queue.len() {
            if self.automatic_track_eligible(index) && self.state.current_index != Some(index) {
                self.splice_into_shuffle_pool(index);
            }
        }
    }

    /// Repairs the bag after a change to what counts as eligible rather than to
    /// the queue's shape: rows that just became ineligible leave, rows that just
    /// became eligible are spliced in, and every survivor keeps its drawn
    /// position. It walks the queue rather than a single index because one
    /// track id can occupy several queue rows.
    fn repair_shuffle_pool_for_eligibility(&mut self) {
        if !self.state.shuffle {
            self.shuffle_pool.clear();
            return;
        }
        let eligible: Vec<bool> = (0..self.state.queue.len())
            .map(|index| self.automatic_track_eligible(index))
            .collect();
        // Membership is answered from a set built once. Asking the bag itself
        // with `Vec::contains` per queue row would be quadratic, and a playlist
        // queue holds thousands of rows for the sake of this one toggle.
        let pooled: HashSet<usize> = self.shuffle_pool.iter().copied().collect();
        self.shuffle_pool
            .retain(|index| eligible.get(*index).copied().unwrap_or(false));
        let current = self.state.current_index;
        for index in 0..eligible.len() {
            if eligible[index] && Some(index) != current && !pooled.contains(&index) {
                self.splice_into_shuffle_pool(index);
            }
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
                // Deliberately not where the failure burst is cleared. This
                // event means librespot has a decoder and intends to use it,
                // which is not the same as audio existing: a track loaded
                // without its key reaches exactly here, with a playhead about
                // to start moving over silence. Clearing the run on this would
                // reset the breaker once per phantom track and guarantee it
                // never counts to three. `AudioSignal::Output` is the event
                // that means playback.
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
                // The failure this catches is the one the owner reported
                // twice: a track that never produced a sample, whose playhead
                // ran for three seconds over silence, and which then "ended".
                // Advancing on that is what walked the queue. It is a load
                // failure wearing the end of a track, so it is handled as one.
                if !self.end_of_track_is_genuine() {
                    eprintln!(
                        "transport: {track_id} ended at {} ms of {} ms having produced {}; treating it as a failed load",
                        self.state.position_ms,
                        self.state.duration_ms,
                        if self.current_load_produced_audio {
                            "audio"
                        } else {
                            "no audio at all"
                        },
                    );
                    return self.fail_current_load(track_id);
                }
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
            // librespot's own `TimeToPreloadNextTrack` is deliberately not
            // used. It answers nearly the same question — it fires at the same
            // 30 s — but it measures against the source file rather than the
            // compiled timeline the listener hears, it fires in the Paused
            // state as readily as the Playing one, and it withholds itself
            // entirely until the current track has finished downloading, which
            // on a slow link means the preload silently never happens. The
            // engine's own watermark answers all three, so having both would
            // only be two triggers disagreeing about one decision.
            PlayerEvent::Unavailable {
                play_request_id,
                track_id,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.fail_current_load(track_id)
            }
            // A failed *preload*. librespot stamps this with the play request
            // of the track currently playing but the track id of the one it
            // was fetching ahead (`player.rs`, the `PlayerPreload::Loading`
            // error arm), so it matched neither the current row nor the
            // ignored-event log's expectations, and fell silently through both
            // — which is how a whole class of key refusals stayed invisible to
            // the breaker that exists to count them. It is still evidence
            // about the service, so it counts; it says nothing about the track
            // that is playing fine, so nothing else here moves.
            PlayerEvent::Unavailable {
                play_request_id,
                track_id,
            } if self.play_request_id == Some(play_request_id) => {
                let burst = self.record_unavailable(Instant::now());
                eprintln!(
                    "transport: preload failed for {track_id} ({} in this run); holding further preloads back",
                    burst.consecutive,
                );
                false
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
        self.detach_player();
        self.recent_unavailable.clear();
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
    }

    /// Drops the installed player and everything that only made sense with it,
    /// keeping the session: the queue, the playhead and browsing do not depend
    /// on the audio device, and a player attached to a stream that can never
    /// drain again would do nothing for a transport command but lie about it.
    ///
    /// The caller owns the generation bump that discards events still in
    /// flight from this player — see [`Engine::enter_audio_unavailable`].
    fn detach_player(&mut self) {
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
        self.current_load_produced_audio = false;
        self.retry_current_at = None;
        self.current_load_is_retry = false;
        self.preload_armed = false;
        self.mixer = None;
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

/// What the user is told when the output stopped draining mid-track.
///
/// Deliberately not the "no output device" wording yet: all the engine knows at
/// this point is that nothing has consumed the audio ring for a couple of
/// seconds, and the device may still be there — a driver restart, a USB
/// re-enumeration, a display that went to sleep. The probe that follows asks
/// the device itself, and its answer replaces this one within seconds when
/// there really is none.
fn audio_stalled_message() -> String {
    "the audio output stopped responding; playback resumes by itself when an output device is \
     available"
        .to_owned()
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
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    use super::{
        AUDIO_PROBE_BACKOFF_MAX, AUDIO_PROBE_BACKOFF_MIN, AudioSignal, AuthFailure, AuthSignal,
        ConnectedSession, Engine, LOAD_RETRY_BACKOFF, PRELOAD_WATERMARK_MS, PlaybackHandles,
        PlaybackState, PlayerSignal, RECONNECT_BACKOFF_MAX, RECONNECT_BACKOFF_MIN,
        TRACK_CHANGE_BURST_WINDOW, TRACK_CHANGE_MIN_INTERVAL, UNAVAILABLE_BURST_WINDOW,
        UNAVAILABLE_STOP_LIMIT, automatic_track_eligible, first_automatic_from,
        first_automatic_wrapping, first_available_from, first_available_wrapping,
        remap_current_index_after_move, sequential_automatic_index, sequential_available_index,
        sequential_next_index, track_change_wait, with_preview_edit,
    };
    use crate::audio::{self, RodioError};
    use crate::auth::{PlaybackError, create_playback};
    use crate::customization::TrackEditStore;
    use crate::io::ProtocolWriter;
    use librespot_core::SpotifyUri;
    use librespot_playback::audio_backend::{Sink, SinkResult};
    use librespot_playback::config::PlayerConfig;
    use librespot_playback::convert::Converter;
    use librespot_playback::decoder::AudioPacket;
    use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume, softmixer::SoftMixer};
    use librespot_playback::player::{Player, PlayerEvent};
    use renderer_engine::protocol::{Command, LoopRange, RepeatMode, TimeRange, TrackEdit, TrackRef};

    fn test_engine() -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_in(PathBuf::new())
    }

    fn test_engine_in(state_directory: PathBuf) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        test_engine_with_audio(state_directory, device_present())
    }

    /// A machine that always has an output device, behind a sink that accepts
    /// everything: librespot needs a `Sink` and nothing in these tests listens
    /// to what it is handed. Tests that are *about* the device replace this
    /// with one they can take away (see [`TestAudioDevice`]).
    fn device_present() -> audio::SinkOpener {
        Arc::new(|_| Ok(Box::new(TestSink)))
    }

    fn test_engine_with_audio(
        state_directory: PathBuf,
        audio_device: audio::SinkOpener,
    ) -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
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
                audio_device,
            ),
            buffer,
        )
    }

    /// Stands in for a machine's audio output: librespot needs a sink and
    /// nothing here listens to what it is handed.
    struct TestSink;

    impl Sink for TestSink {
        fn write(&mut self, _packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            Ok(())
        }
    }

    /// A machine whose output device can be unplugged and plugged back in, and
    /// which counts the sinks the engine opened on it. Both directions of the
    /// device bug turn on exactly these two facts, and neither is something
    /// real hardware can be asked to provide on cue.
    #[derive(Default)]
    struct TestAudioDevice {
        present: AtomicBool,
        opened: AtomicUsize,
    }

    impl TestAudioDevice {
        /// A machine whose output device is not there.
        fn absent() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// A machine with a working output device.
        fn present() -> Arc<Self> {
            let device = Arc::new(Self::default());
            device.plug_in();
            device
        }

        fn plug_in(&self) {
            self.present.store(true, Ordering::Release);
        }

        fn unplug(&self) {
            self.present.store(false, Ordering::Release);
        }

        fn opened(&self) -> usize {
            self.opened.load(Ordering::Acquire)
        }

        fn opener(self: &Arc<Self>) -> audio::SinkOpener {
            let device = Arc::clone(self);
            Arc::new(move |_| {
                if !device.present.load(Ordering::Acquire) {
                    return Err(RodioError::NoDeviceAvailable);
                }
                device.opened.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(TestSink))
            })
        }
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

    fn playback_handles(player: Arc<Player>) -> PlaybackHandles {
        let events = player.get_player_event_channel();
        PlaybackHandles {
            player,
            events,
            mixer: Arc::new(SoftMixer::open(MixerConfig::default()).expect("software mixer")),
            volume_percent: 50,
        }
    }

    /// The signal a successful cached connection produces: a live session with
    /// a player built against it.
    fn connected_handles(
        player: Arc<Player>,
        session: librespot_core::Session,
    ) -> ConnectedSession {
        ConnectedSession {
            session,
            playback: Ok(playback_handles(player)),
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

    /// Long enough that a run of failures has somewhere to march to, which is
    /// what the circuit-breaker tests have to be able to observe it not doing.
    fn five_track_state() -> PlaybackState {
        let mut state = playback_state(240_000);
        for suffix in ['M', 'N', 'P', 'Q'] {
            let id = format!("0123456789ABCDEFGHIJK{suffix}");
            state.queue.push(TrackRef {
                uri: format!("spotify:track:{id}"),
                id,
                duration_ms: 180_000,
                ..TrackRef::default()
            });
        }
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
    /// Fails the load of whatever row is current, the way librespot does:
    /// with the play request the engine is holding and the row's own uri.
    fn fail_current_row(engine: &mut Engine, play_request_id: u64) -> bool {
        let index = engine.state.current_index.expect("a current row");
        let track_id =
            SpotifyUri::from_uri(&engine.state.queue[index].uri).expect("queue uris are valid");
        engine.play_request_id = Some(play_request_id);
        engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id,
            track_id,
        })
    }

    /// Brings an armed retry forward to now and runs the tick that fires it.
    fn fire_due_retry(engine: &mut Engine) {
        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the armed retry fires");
    }

    /// What "this track is really playing" looks like: the sink reports the
    /// decoder produced a packet for the pipeline this load configured.
    /// Anything simulating audible playback owes the engine this, because it
    /// is the only evidence the engine accepts that a track is not silent.
    fn note_audio(engine: &mut Engine) {
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
    }

    fn hear_audio(engine: &mut Engine) {
        note_audio(engine);
        engine.tick_playback_health();
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

    /// The archive keeps plays that were listened to, so a test that wants a
    /// real row has to supply real listening rather than a touch.
    fn listened(engine: &mut Engine) {
        engine.listening_history.pretend_listened();
    }

    fn history_rows(engine: &Engine) -> Vec<renderer_engine::protocol::HistoryItem> {
        engine
            .history(&renderer_engine::protocol::HistoryQuery {
                limit: usize::MAX,
                ..renderer_engine::protocol::HistoryQuery::default()
            })
            .unwrap()
            .items
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
        listened(&mut engine);
        assert_eq!(history_rows(&engine).len(), 1);

        let track = engine.state.queue[0].clone();
        assert!(
            engine
                .preview_track_edit(track, vec![range(1_000, 2_000)], None, 5_000, 1)
                .is_err(),
            "the capture engine has no player"
        );
        assert!(engine.preview_mode);
        let rows = history_rows(&engine);
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
            history_rows(&engine).len(),
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
        assert!(history_rows(&engine).is_empty());
        engine.shutdown();
        assert!(history_rows(&engine).is_empty());
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
        listened(&mut engine);
        let rows = history_rows(&engine);
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
        listened(&mut engine);
        assert!(engine.on_player_event(PlayerEvent::Paused {
            play_request_id: 7,
            track_id: uri,
            position_ms: 5_000,
        }));
        let paused_ms = history_rows(&engine)[0].row.ms_played;
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            history_rows(&engine)[0].row.ms_played,
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
        listened(&mut engine);
        assert!(engine.on_player_signal(PlayerSignal::Closed { generation: 0 }));

        // The play is in the journal and the in-progress sidecar is gone.
        let journal = std::fs::read_to_string(root.join("listening_history.jsonl")).unwrap();
        assert_eq!(journal.lines().filter(|line| !line.is_empty()).count(), 1);
        assert!(!root.join("listening_history_active.json").exists());
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

    fn playlist_queue(ids: &[&str]) -> Vec<TrackRef> {
        ids.iter()
            .map(|id| contextual_track(id, "playlist:playlist"))
            .collect()
    }

    const QUEUE_IDS: [&str; 4] = [
        "0abcdefghijklmnopqrstu",
        "1abcdefghijklmnopqrstu",
        "2abcdefghijklmnopqrstu",
        "3abcdefghijklmnopqrstu",
    ];

    #[test]
    fn upcoming_is_the_shuffle_pool_in_pop_order() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[2])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.shuffle = true;
        engine.rebuild_shuffle_pool();

        let popped: Vec<usize> = engine.shuffle_pool.iter().rev().copied().collect();
        assert_eq!(
            engine.upcoming_indices(),
            popped,
            "upcoming is exactly the order the pool will be popped in"
        );

        // A pool built before an exclusion still carries the row, and the
        // transport skips it on the way past; upcoming must skip it too.
        engine.shuffle_pool = vec![3, 2, 1];
        assert_eq!(engine.upcoming_indices(), vec![1, 3]);
        drop(directory);
    }

    #[test]
    fn upcoming_walks_sequentially_past_an_excluded_row() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[2])]);
        let engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);

        assert_eq!(engine.upcoming_indices(), vec![1, 3]);
        drop(directory);
    }

    #[test]
    fn upcoming_wraps_through_the_current_row_under_repeat_context() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[1])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 2);
        engine.state.repeat = RepeatMode::Context;

        assert_eq!(
            engine.upcoming_indices(),
            vec![3, 0, 2],
            "the wrap is inclusive of the current row, like sequential_automatic_index"
        );
        drop(directory);
    }

    #[test]
    fn upcoming_ignores_repeat_one() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.repeat = RepeatMode::Track;

        assert_eq!(
            engine.upcoming_indices(),
            vec![1, 2, 3],
            "repeat-one loops the current row, it does not replan the queue"
        );
        drop(directory);
    }

    #[test]
    fn upcoming_lists_every_eligible_row_without_a_current() {
        let (directory, store) = store_with_exclusions(&[("playlist", QUEUE_IDS[1])]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.current_index = None;

        assert_eq!(engine.upcoming_indices(), vec![0, 2, 3]);
        drop(directory);
    }

    /// A shuffling engine over `QUEUE_IDS` with a hand-written drawn order, so
    /// a repair's effect on that order is exactly readable. The seed is fixed
    /// because splices consume the same PRNG the bag was drawn with.
    fn shuffling_engine(store: TrackEditStore, current: usize, pool: &[usize]) -> Engine {
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), current);
        engine.state.shuffle = true;
        engine.shuffle_pool = pool.to_vec();
        engine.random_state = 0x2545_f491_4f6c_dd1d;
        engine
    }

    /// Every invariant the bag carries, checked as a set rather than one at a
    /// time, because a repair that breaks one usually breaks another.
    fn assert_shuffle_pool_invariants(engine: &Engine) {
        if !engine.state.shuffle {
            assert!(
                engine.shuffle_pool.is_empty(),
                "the bag is empty whenever shuffle is off"
            );
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for index in &engine.shuffle_pool {
            assert!(
                *index < engine.state.queue.len(),
                "bag entry {index} does not index the queue"
            );
            assert!(
                Some(*index) != engine.state.current_index,
                "the bag holds the current row {index}"
            );
            assert!(seen.insert(*index), "the bag holds {index} twice");
        }
    }

    #[test]
    fn removing_a_row_shifts_the_bag_instead_of_redrawing_it() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(engine.remove_queue(2), Ok(true));
        assert_eq!(
            engine.shuffle_pool,
            vec![2, 1],
            "the removed row leaves and the rows above it slide down one"
        );
        assert_eq!(engine.upcoming_indices(), vec![1, 2]);
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn removing_the_current_row_drops_its_replacement_from_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 1, &[3, 2, 0]);

        assert!(
            engine.remove_queue(1).is_err(),
            "the capture engine has no player to reload the replacement on"
        );
        assert_eq!(engine.state.current_index, Some(1));
        assert_eq!(
            engine.shuffle_pool,
            vec![2, 0],
            "the promoted replacement leaves the bag, the rest keep their order"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn moving_a_row_remaps_the_bag_the_same_way_as_the_current_row() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 1, &[3, 2, 0]);

        assert_eq!(engine.move_queue(0, 3), Ok(true));
        assert_eq!(engine.state.current_index, Some(0));
        assert_eq!(
            engine.shuffle_pool,
            vec![
                remap_current_index_after_move(3, 0, 3),
                remap_current_index_after_move(2, 0, 3),
                remap_current_index_after_move(0, 0, 3),
            ],
            "one permutation moves the queue, the current row and the bag"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn moving_a_row_onto_the_current_row_keeps_it_out_of_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 2, &[3, 1, 0]);

        assert_eq!(engine.move_queue(0, 2), Ok(true));
        assert_eq!(engine.state.current_index, Some(1));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn adding_a_row_splices_it_without_reordering_the_survivors() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[]);
        // Seven survivors, not three: a redraw would have to reproduce one
        // ordering out of five thousand to sneak past this assertion, which a
        // four-row queue leaves well within coincidence.
        for ordinal in 4..8u32 {
            engine.state.queue.push(contextual_track(
                &format!("0abcdefghijklmnopqrst{ordinal}"),
                "playlist:playlist",
            ));
        }
        engine.shuffle_pool = vec![2, 7, 4, 1, 6, 3, 5];

        assert_eq!(
            engine.add_queue(
                contextual_track("0abcdefghijklmnopqrst9", "playlist:playlist"),
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert!(engine.shuffle_pool.contains(&8));
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 8)
                .collect::<Vec<usize>>(),
            vec![2, 7, 4, 1, 6, 3, 5],
            "queueing a track must not redraw the plan already on screen"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn a_batch_add_splices_every_new_row_and_keeps_the_survivors_ordered() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(
            engine.add_queue_batch(
                vec![
                    contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                    contextual_track("5abcdefghijklmnopqrstu", "playlist:playlist"),
                ],
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index < 4)
                .collect::<Vec<usize>>(),
            vec![3, 2, 1]
        );
        assert!(engine.shuffle_pool.contains(&4) && engine.shuffle_pool.contains(&5));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn queued_rows_land_somewhere_other_than_one_end_of_the_bag() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        let mut interior = false;
        for ordinal in 4..24u32 {
            let id = format!("{ordinal:02}bcdefghijklmnopqrstu");
            let added = engine.state.queue.len();
            assert_eq!(
                engine.add_queue(
                    contextual_track(&id, "playlist:playlist"),
                    "playlist:playlist".to_owned(),
                ),
                Ok(true)
            );
            let at = engine
                .shuffle_pool
                .iter()
                .position(|index| *index == added)
                .expect("the new row joined the bag");
            interior |= at > 0 && at + 1 < engine.shuffle_pool.len();
        }
        assert!(
            interior,
            "appending to an end would make queued tracks always play next or always last"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn play_queue_index_drops_only_the_row_it_jumps_to() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert!(
            engine.play_queue_index(2).is_err(),
            "the capture engine has no player"
        );
        assert_eq!(engine.state.current_index, Some(2));
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "the row jumped to leaves; the row left behind is `previous`'s business"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn exclusion_toggles_repair_the_bag_in_both_directions() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[3, 2, 1]);

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[2], true),
            Ok(())
        );
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "an excluded row leaves without disturbing the others"
        );
        assert_shuffle_pool_invariants(&engine);

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[2], false),
            Ok(())
        );
        assert!(engine.shuffle_pool.contains(&2));
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 2)
                .collect::<Vec<usize>>(),
            vec![3, 1],
            "a re-included row is spliced in, it does not trigger a redraw"
        );
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn an_exclusion_toggle_reaches_every_queue_row_holding_that_track() {
        let repeated = "0abcdefghijklmnopqrstu";
        let queue = vec![
            contextual_track("1abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(repeated, "playlist:playlist"),
            contextual_track("2abcdefghijklmnopqrstu", "playlist:playlist"),
            contextual_track(repeated, "playlist:playlist"),
        ];
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, queue, 0);
        engine.state.shuffle = true;
        engine.shuffle_pool = vec![3, 2, 1];
        engine.random_state = 0x2545_f491_4f6c_dd1d;

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", repeated, true),
            Ok(())
        );
        assert_eq!(
            engine.shuffle_pool,
            vec![2],
            "both rows carrying the excluded id leave the bag"
        );

        assert_eq!(
            engine.set_playlist_track_excluded("playlist", repeated, false),
            Ok(())
        );
        assert_eq!(engine.shuffle_pool.len(), 3);
        assert!(engine.shuffle_pool.contains(&1) && engine.shuffle_pool.contains(&3));
        assert_shuffle_pool_invariants(&engine);
        drop(directory);
    }

    #[test]
    fn preview_mode_eligibility_governs_a_repair_like_it_governs_a_redraw() {
        let excluded_id = QUEUE_IDS[2];
        let (directory, store) = store_with_exclusions(&[("playlist", excluded_id)]);
        let mut engine = shuffling_engine(store, 0, &[3, 1]);
        engine.preview_mode = true;

        // Preview ignores playlist exclusions, so the excluded row is eligible
        // here and a repair must splice it in rather than leave it out.
        engine.repair_shuffle_pool_for_eligibility();
        assert!(engine.shuffle_pool.contains(&2));
        assert_shuffle_pool_invariants(&engine);

        engine.preview_mode = false;
        engine.repair_shuffle_pool_for_eligibility();
        assert_eq!(
            engine.shuffle_pool,
            vec![3, 1],
            "leaving preview puts the exclusion back in force"
        );
        drop(directory);
    }

    #[test]
    fn a_mutation_leaves_the_drawn_order_of_untouched_rows_alone() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);
        engine.state.shuffle = true;
        engine.random_state = 0x9e37_79b9_7f4a_7c15;
        engine.rebuild_shuffle_pool();
        let drawn = engine.shuffle_pool.clone();

        assert_eq!(
            engine.add_queue(
                contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                "playlist:playlist".to_owned(),
            ),
            Ok(true)
        );
        assert_eq!(
            engine
                .shuffle_pool
                .iter()
                .copied()
                .filter(|index| *index != 4)
                .collect::<Vec<usize>>(),
            drawn,
            "this is the regression: a queue mutation used to redraw the whole plan"
        );
        drop(directory);
    }

    #[test]
    fn repairs_never_resurrect_the_bag_while_shuffle_is_off() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = engine_with_queue(store, playlist_queue(&QUEUE_IDS), 0);

        for seed in 0..3 {
            engine.shuffle_pool = vec![3, 2, 1];
            match seed {
                0 => assert_eq!(engine.move_queue(1, 2), Ok(true)),
                1 => assert_eq!(engine.remove_queue(3), Ok(true)),
                _ => assert_eq!(
                    engine.add_queue(
                        contextual_track("4abcdefghijklmnopqrstu", "playlist:playlist"),
                        "playlist:playlist".to_owned(),
                    ),
                    Ok(true)
                ),
            }
            assert!(
                engine.shuffle_pool.is_empty(),
                "an ordered queue has no shuffle plan to repair"
            );
        }
        engine.shuffle_pool = vec![3, 2, 1];
        assert_eq!(
            engine.set_playlist_track_excluded("playlist", QUEUE_IDS[1], true),
            Ok(())
        );
        assert!(engine.shuffle_pool.is_empty());
        drop(directory);
    }

    #[test]
    fn the_repeat_context_refill_still_draws_a_fresh_lap() {
        let (directory, store) = store_with_exclusions(&[]);
        let mut engine = shuffling_engine(store, 0, &[]);
        engine.state.repeat = RepeatMode::Context;

        let next = engine
            .take_next_index_with_skip(false, false)
            .expect("an emptied bag refills under repeat-context");
        assert_ne!(next, 0, "the refill never offers the current row");
        assert_eq!(
            engine.shuffle_pool.len(),
            2,
            "a new lap is a new draw of every other eligible row"
        );
        assert_shuffle_pool_invariants(&engine);
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
        // The engine's own advance carries the intent across.
        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert_eq!(paused.advance(false), Ok(true));
        assert_eq!(paused.state.current_index, Some(1));
        assert!(
            !paused.state.playing,
            "an automatic advance must not start a paused queue playing"
        );

        // A press of Next is a request to hear the next track, so it resumes.
        let (mut pressed, _pressed_probe) = engine_with_player();
        pressed.state = two_track_state();
        pressed.state.playing = false;
        assert_eq!(
            pressed.advance_with_current_skip(false, false, true),
            Ok(true)
        );
        assert_eq!(pressed.state.current_index, Some(1));
        assert!(
            pressed.state.playing,
            "pressing Next on a paused queue must start playing"
        );

        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert_eq!(playing.advance(false), Ok(true));
        assert_eq!(playing.state.current_index, Some(1));
        assert!(playing.state.playing);
    }

    /// A runtime failure only moves a queue that is actually playing: the
    /// progression exists to keep audio flowing, and a paused queue has no
    /// flow.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_track_moves_on_only_while_playing() {
        let (mut playing, _playing_probe) = engine_with_player();
        playing.state = two_track_state();
        playing.state.playing = true;
        assert!(playing.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            playing.state.current_index,
            Some(0),
            "the first failure buys the row a retry, not a skip"
        );
        assert!(playing.retry_current_at.is_some());
        assert!(
            playing.state.playing,
            "a failure mid-playback keeps the transport intent"
        );

        // The bug the owner hit: librespot finishes a paused load and fails it
        // just the same, and the engine used to skip on that failure, so pause
        // could not stop the queue from walking itself.
        let (mut paused, _paused_probe) = engine_with_player();
        paused.state = two_track_state();
        paused.state.playing = false;
        assert!(paused.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            paused.state.current_index,
            Some(0),
            "a failure while paused holds the row instead of advancing"
        );
        assert!(!paused.state.playing);
        assert!(paused.loading_failed, "Play must re-arm a fresh loader");
        assert!(
            paused.retry_current_at.is_none(),
            "a paused failure must not arm a retry either"
        );
    }

    /// A row gets one more attempt before it is given up on: the refusal
    /// behind these failures is transient, and skipping on the first one
    /// discards a track that was never broken.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_row_is_retried_once_and_only_then_skipped() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(0));
        let due = engine.retry_current_at.expect("the row is owed a retry");
        assert!(
            due.duration_since(Instant::now()) <= LOAD_RETRY_BACKOFF,
            "the retry waits out the backoff"
        );

        // Not yet due: nothing happens, and the row is still held.
        assert!(!engine.tick_playback_health());
        assert_eq!(engine.state.current_index, Some(0));

        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the retry fires");
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "the retry reloads the same row"
        );
        assert!(engine.current_load_is_retry);
        assert!(engine.retry_current_at.is_none());

        // The retry fails too, and only now is the row given up on.
        engine.play_request_id = Some(11);
        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 11,
            track_id: track_uri(),
        }));
        assert_eq!(
            engine.state.current_index,
            Some(1),
            "a row that failed twice is skipped"
        );
        assert!(
            engine.retry_current_at.is_none(),
            "the successor starts with a clean slate"
        );
    }

    /// The engine stops the failed loader itself, and librespot answers that
    /// with a `Stopped` carrying the play request the engine was holding.
    /// Handled as an ordinary stop it would clear `playing` three seconds
    /// before the retry is due, and the retry — which only fires for a queue
    /// that is playing — would never run.
    #[tokio::test(flavor = "current_thread")]
    async fn the_engines_own_stop_does_not_disarm_the_retry_it_just_armed() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        assert!(fail_current_row(&mut engine, 7));
        assert!(engine.retry_current_at.is_some());

        assert!(
            !engine.on_player_event(PlayerEvent::Stopped {
                play_request_id: 7,
                track_id: track_uri(),
            }),
            "the stopped loader's play request is no longer the engine's"
        );
        assert!(
            engine.state.playing,
            "the retry still has a queue to run in"
        );
        fire_due_retry(&mut engine);
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.current_load_is_retry);
    }

    /// The retry a failed row is owed has to ask for the position the row was
    /// loaded at, and the drift projection is what decides that. A failure
    /// takes seconds to arrive — the key request times out, librespot downloads
    /// without decryption, the decoder waits out its own deadline — and the
    /// engine's clock does not stop for it: the projection clamps at the row's
    /// own duration, so on a short row it landed on the final millisecond and
    /// the retry went out as a load *at the end of the track*. No audio either
    /// way, a second failure the row never earned, and a healthy one-second
    /// track skipped as unavailable — a run of them being exactly the queue
    /// that appeared to march through itself in silence.
    ///
    /// Freezing only *after* the failure would be too late: the seconds spent
    /// dying are exactly the seconds the projection has to survive, so the
    /// playhead must not move for a load that has not produced audio at all.
    /// Both halves are asserted here, in the order they happen.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_load_holds_the_playhead_where_the_retry_can_resume() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = playback_state(1_000);
        engine.state.playing = true;
        engine.update_position(0);

        // The load is in flight and has produced nothing yet; nine seconds of
        // wall clock pass before librespot gives up on it.
        engine.position_anchor = Some((0, Instant::now() - Duration::from_secs(9)));
        assert!(
            !engine.tick_position(),
            "a load with no audio yet is not playing, so there is no playhead to advance"
        );
        assert_eq!(
            engine.state.position_ms, 0,
            "the load offset is the last true position until audio exists"
        );

        assert!(fail_current_row(&mut engine, 7));
        assert!(
            !engine.tick_position(),
            "a load that failed is not playing, so there is no playhead to advance"
        );
        assert_eq!(engine.state.position_ms, 0, "the failure did not advance it either");

        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(engine.tick_playback_health(), "the armed retry fires");
        assert_eq!(
            engine.state.position_ms, 0,
            "the retry loads the row where it was asked to start, not at its end"
        );
        assert!(engine.current_load_is_retry);
    }

    /// What the UI needs to stop projecting on its own: a playing row reports
    /// `buffering` until the decoder produces its first packet, and the first
    /// packet is the state change that clears it. The position it clears at is
    /// the load offset — nothing has moved, precisely because the projection
    /// was held — so the local projection can resume from exactly where the
    /// engine says playback starts.
    #[test]
    fn buffering_covers_a_playing_row_until_its_first_packet_and_then_clears() {
        let (mut engine, buffer) = test_engine();
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.update_position(30_000);

        let state = |engine: &mut Engine, buffer: &Arc<std::sync::Mutex<Vec<u8>>>| {
            engine.emit_state().expect("state emits");
            let mut bytes = buffer.lock().expect("buffer lock");
            serde_json::from_slice::<serde_json::Value>(&std::mem::take(&mut *bytes))
                .expect("state json")
        };

        let before = state(&mut engine, &buffer);
        assert_eq!(before["playing"], true);
        assert_eq!(
            before["buffering"], true,
            "intent is playing but no packet has come out of this load yet"
        );
        assert_eq!(before["position_ms"], 30_000);

        // The decoder's first packet for the configured pipeline.
        assert!(
            engine.on_audio_signal(AudioSignal::Output {
                revision: engine.audio_revision,
            }),
            "the first packet is a state change: buffering just ended"
        );
        let after = state(&mut engine, &buffer);
        assert_eq!(after["buffering"], false);
        assert_eq!(
            after["position_ms"], 30_000,
            "audio starts where the load was told to start"
        );
        assert!(
            engine.tick_position(),
            "the playhead projects once there is audio to project"
        );
        assert!(
            engine.state.position_ms >= 30_000 && engine.state.position_ms < 31_000,
            "and it projects from the load offset, not from when the load was issued: {}",
            engine.state.position_ms
        );

        // Later packets are not news.
        assert!(!engine.on_audio_signal(AudioSignal::Output {
            revision: engine.audio_revision,
        }));

        // A paused row is not buffering: its position is simply frozen.
        engine.state.playing = false;
        assert_eq!(state(&mut engine, &buffer)["buffering"], false);
    }

    /// Pause is the cascade's off switch, and an armed retry must not route
    /// around it. The owner's complaint the last time this bug ran was a pause
    /// button that visibly did nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn pause_stops_an_armed_retry() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        assert!(engine.on_player_event(PlayerEvent::Unavailable {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert!(engine.retry_current_at.is_some());

        assert_eq!(engine.pause(), Ok(true));
        engine.retry_current_at = Some(Instant::now() - Duration::from_millis(1));
        assert!(
            !engine.tick_playback_health(),
            "a paused queue does not retry"
        );
        assert!(!engine.state.playing);
        assert_eq!(engine.state.current_index, Some(0));
    }

    /// A run of failures is the key service refusing this client. Stopping is
    /// the only answer that does not empty the queue in silence and does not
    /// keep asking the throttle for more keys.
    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_failures_stop_playback_with_an_error() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        // The first row's two attempts, which cost it one skip.
        assert!(fail_current_row(&mut engine, 7));
        assert_eq!(engine.state.current_index, Some(0));
        fire_due_retry(&mut engine);
        assert!(fail_current_row(&mut engine, 101));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(
            engine.state.playing,
            "the queue keeps flowing below the limit"
        );

        // The replacement fails too. Three failures inside the window is the
        // service refusing this client, not three dead rows.
        assert!(fail_current_row(&mut engine, 102));
        assert!(!engine.state.playing, "the run trips the breaker");
        assert_eq!(
            engine.state.current_index,
            Some(1),
            "the breaker stops on the failed row rather than spending another"
        );
        assert!(
            engine.retry_current_at.is_none(),
            "a stop does not leave a retry armed behind it"
        );
        let error = engine.state.error.expect("the breaker explains itself");
        assert!(
            error.contains("tracks in a row"),
            "the UI needs a reason, got {error:?}"
        );
    }

    /// The breaker must not turn one dead row into a stop. Audio coming out
    /// resets the run, which is what tells "this track is gone" apart from
    /// "the service is gone".
    #[tokio::test(flavor = "current_thread")]
    async fn an_isolated_failure_still_skips_and_audio_resets_the_run() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;

        for round in 0..UNAVAILABLE_STOP_LIMIT + 1 {
            let index = engine.state.current_index.expect("a current row");

            // A lone bad row still costs two attempts and exactly one skip.
            assert!(fail_current_row(&mut engine, 300 + round as u64));
            assert_eq!(engine.state.current_index, Some(index));
            fire_due_retry(&mut engine);
            assert!(fail_current_row(&mut engine, 400 + round as u64));
            assert!(
                engine.state.playing,
                "round {round}: an isolated failure skips, it does not stop"
            );
            let landed = engine.state.current_index.expect("a replacement row");
            assert_eq!(landed, (index + 1) % engine.state.queue.len());

            // The replacement is heard, which is what a lone bad row looks
            // like. librespot's `Playing` deliberately does not count: a track
            // loaded without its audio key reaches it too.
            hear_audio(&mut engine);
            assert!(
                engine.recent_unavailable.is_empty(),
                "round {round}: audio ends the run"
            );
        }
    }

    /// The bug the owner reported twice. Spotify's key service refuses the
    /// key, librespot loads the track undecrypted anyway and announces it as
    /// playing, the playhead runs for three seconds over silence, and the
    /// decoder then chokes and reports `EndOfTrack` — indistinguishable, to
    /// the engine that used to believe it, from a track that finished. So the
    /// queue advanced, and advanced, and advanced.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_never_produced_audio_did_not_finish() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        // Exactly what librespot reports for a keyless track: loaded, playing,
        // and then three seconds later, over.
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 0,
        }));
        engine.update_position(3_000);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(
            engine.state.current_index,
            Some(0),
            "a track that never played is a failed load, not a finished one"
        );
        assert!(engine.loading_failed);
        assert!(engine.retry_current_at.is_some(), "the row is owed a retry");
        assert_eq!(
            engine.recent_unavailable.len(),
            1,
            "and the breaker finally counts it"
        );
    }

    /// The other half of that judgement: a track that really ended must still
    /// advance the queue, or the fix would simply stop playback instead.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_played_to_its_end_still_advances() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        assert!(engine.on_player_event(PlayerEvent::Playing {
            play_request_id: 7,
            track_id: track_uri(),
            position_ms: 0,
        }));
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
        engine.update_position(engine.state.duration_ms);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(1));
        assert!(engine.state.playing);
        assert!(engine.recent_unavailable.is_empty());
    }

    /// A decoder that collapses halfway through a track it *was* playing also
    /// reports `EndOfTrack`. Audio came out, so the silence test passes it;
    /// the position test is what catches it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_track_that_broke_halfway_did_not_finish_either() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        let revision = engine.audio_revision;
        engine.on_audio_signal(AudioSignal::Output { revision });
        engine.update_position(engine.state.duration_ms / 2);

        assert!(engine.on_player_event(PlayerEvent::EndOfTrack {
            play_request_id: 7,
            track_id: track_uri(),
        }));
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.loading_failed);
    }

    /// A failed *preload* carries the current track's play request and the
    /// preloaded track's id, so it matched no arm at all and the breaker never
    /// saw it. It is evidence about the service, so it counts — and it must
    /// not disturb the track that is playing perfectly well.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_preload_counts_toward_the_breaker_without_moving_the_queue() {
        let (mut engine, _probe) = engine_with_player();
        engine.state = five_track_state();
        engine.state.playing = true;
        engine.preload_armed = true;

        let preloaded = SpotifyUri::from_uri(&engine.state.queue[1].uri).expect("a valid uri");
        assert!(
            !engine.on_player_event(PlayerEvent::Unavailable {
                play_request_id: 7,
                track_id: preloaded,
            }),
            "nothing the user can see changed"
        );
        assert_eq!(engine.state.current_index, Some(0));
        assert!(engine.state.playing);
        assert!(!engine.loading_failed);
        assert_eq!(
            engine.recent_unavailable.len(),
            1,
            "the refusal still counts against the service"
        );
        assert!(
            engine.preload_target().is_none(),
            "and holds the next preload back"
        );
    }

    /// Preloading asks for a second audio key per load. While the key service
    /// is the thing failing, that is the request that must not be made.
    #[test]
    fn a_failure_burst_holds_back_the_next_track_preload() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = true;
        engine.preload_armed = true;

        assert!(
            engine.preload_target().is_some(),
            "an armed, healthy engine preloads the next track"
        );
        engine.record_unavailable(Instant::now());
        assert!(
            engine.preload_target().is_none(),
            "a failing engine must not ask for a second audio key per load"
        );
        engine.clear_unavailable_burst();
        assert!(
            engine.preload_target().is_some(),
            "a recovered engine restores gapless preloading"
        );
    }

    /// The preload watermark. A track the user just started is not about to
    /// finish, so no second audio key is spent on it; a track being listened
    /// to arms one with plenty of lead.
    #[test]
    fn a_preload_waits_until_the_current_track_is_nearly_over() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = true;

        engine.update_position(0);
        assert!(!engine.at_preload_watermark());
        engine.update_position(240_000 - PRELOAD_WATERMARK_MS - 1_000);
        assert!(
            !engine.at_preload_watermark(),
            "a second before the watermark is still churn territory"
        );
        engine.update_position(240_000 - PRELOAD_WATERMARK_MS);
        assert!(engine.at_preload_watermark());

        // A track shorter than the watermark must still preload, so the floor
        // is capped at half its length the way `play_qualifies` caps its own.
        let mut short = playing_engine();
        short.state = playback_state(20_000);
        short.state.playing = true;
        short.update_position(9_000);
        assert!(!short.at_preload_watermark());
        short.update_position(10_000);
        assert!(
            short.at_preload_watermark(),
            "half a short track is its watermark"
        );
    }

    /// Clicking through a queue is what provokes the key service, and it is
    /// the case the watermark makes free: no track is ever near its end, so
    /// no preload is ever asked for.
    #[test]
    fn clicking_through_tracks_never_arms_a_preload() {
        let mut engine = playing_engine();
        engine.state = five_track_state();
        engine.state.playing = true;

        for _ in 0..5 {
            engine.update_position(2_000);
            engine.tick_playback_health();
            assert!(
                !engine.preload_armed,
                "a track two seconds in is not about to finish"
            );
            engine.state.current_index =
                Some((engine.state.current_index.expect("a current row") + 1) % 5);
            engine.state.duration_ms = 180_000;
        }
    }

    /// Edge-triggered per load, and never while paused. A paused track is not
    /// approaching its end, and scrubbing across the watermark must not buy
    /// the same key again.
    #[test]
    fn the_watermark_arms_once_per_load_and_not_while_paused() {
        let mut engine = playing_engine();
        engine.state = two_track_state();
        engine.state.playing = false;
        engine.update_position(239_000);
        engine.tick_playback_health();
        assert!(
            !engine.preload_armed,
            "a paused track is not approaching its end"
        );

        // Resuming re-evaluates on the next tick without anything special.
        engine.state.playing = true;
        engine.tick_playback_health();
        assert!(engine.preload_armed);

        // Scrubbing back and forward across it changes nothing: only a load
        // disarms it.
        engine.update_position(1_000);
        engine.tick_playback_health();
        assert!(engine.preload_armed);
    }

    #[test]
    fn loop_marker_reloads_when_decoder_eof_arrived_first() {
        let mut engine = playing_engine();
        engine.state.playing = true;
        engine.state.queue[0].effective_edit = Some(renderer_engine::protocol::TrackEdit {
            cuts: Vec::new(),
            loop_range: Some(loop_range(1_500, 2_000, 2)),
        });
        note_audio(&mut engine);
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
        note_audio(&mut engine);
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
            note_audio(&mut engine);

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
        note_audio(&mut engine);
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
                result: Ok(connected_handles(new_player, new_session)),
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
                result: Ok(connected_handles(new_player, new_session)),
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
            !engine.record_unavailable(start).clustered,
            "the first failure in a quiet period stays eligible for cleanup"
        );
        let second = start + Duration::from_millis(8_900);
        assert!(
            engine.record_unavailable(second).clustered,
            "a failure 8.9 s later is the same outage, not a corrupt cache file"
        );
        assert!(
            engine
                .record_unavailable(second + Duration::from_millis(6_500))
                .clustered,
            "and so is the one after that"
        );
    }

    #[test]
    fn unavailable_burst_classifier_keeps_isolated_cleanup_eligible() {
        let mut engine = playing_engine();
        let start = Instant::now();

        // One quiet load failure is eligible for the corrupt-cache cleanup.
        engine.note_track_change(start);
        assert!(
            !engine
                .record_unavailable(start + Duration::from_millis(10))
                .clustered
        );

        // A second failure shortly afterward is clustered, so valid cache
        // files are preserved while the key/network burst settles.
        assert!(
            engine
                .record_unavailable(start + Duration::from_millis(20))
                .clustered
        );

        // After both windows expire, cleanup is eligible again.
        let quiet = start
            + TRACK_CHANGE_BURST_WINDOW.max(UNAVAILABLE_BURST_WINDOW)
            + Duration::from_millis(25);
        assert!(!engine.record_unavailable(quiet).clustered);

        // Two paced load starts protect even the first failure observed after
        // a rapid-click burst.
        engine.note_track_change(quiet);
        engine.note_track_change(quiet + Duration::from_millis(100));
        assert!(
            engine
                .record_unavailable(quiet + Duration::from_millis(200))
                .clustered
        );
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

        // Intent alone is not playback: a load that has not produced a packet
        // yet is buffering, and its playhead is where it will start, not a
        // position anything has reached.
        engine.state.playing = true;
        assert!(!engine.tick_position(), "silence is not progress");

        // The first packet is what starts the projection, anchored where the
        // load was told to start rather than at the instant it was issued.
        note_audio(&mut engine);
        assert!(engine.tick_position());
        assert!(engine.state.position_ms < 1_000);

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
        note_audio(&mut engine);
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
        note_audio(&mut engine);
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
            note_audio(&mut engine);
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
                    device_present(),
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
            device_present(),
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
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let published = engine
            .state
            .auth_url
            .clone()
            .expect("needs_login publishes a url");
        let (pending, _listener) = engine.begin_login_flow().expect("flow begins");
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
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.state.auth_state = renderer_engine::protocol::AuthState::NeedsLogin;
        let (pending, _listener) = engine.begin_login_flow().expect("flow begins");
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

    /// The bug this whole path exists to prevent: the browser used to be
    /// opened without the command ever running, so Spotify redirected to a
    /// port nothing held. The fix is an ordering promise — by the time the
    /// command path returns, the callback socket is already accepting — and
    /// this is that promise measured from outside the engine.
    #[test]
    fn the_command_path_holds_the_callback_port_before_it_returns() {
        let _serialized = crate::auth::lock_oauth_port();
        let mut engine = test_engine().0;
        engine.enter_needs_login();

        let (_pending, listener) = engine.begin_login_flow().expect("flow begins");

        assert_eq!(listener.address().to_string(), "127.0.0.1:5588");
        assert!(
            std::net::TcpListener::bind("127.0.0.1:5588").is_err(),
            "the engine must already hold the port when it answers"
        );
        std::net::TcpStream::connect("127.0.0.1:5588")
            .expect("a redirect arriving now would be accepted, not refused");
    }

    /// A port this application cannot have is a dead end, because the redirect
    /// URI is registered against librespot's client id and cannot be moved.
    /// The click must fail with that news and leave the engine exactly where
    /// it was, ready for a clean retry — not stranded in `Authenticating`
    /// waiting for a callback that can never arrive.
    #[test]
    fn a_taken_callback_port_refuses_the_login_without_disturbing_the_state() {
        let _serialized = crate::auth::lock_oauth_port();
        let squatter =
            std::net::TcpListener::bind("127.0.0.1:5588").expect("the test takes the port first");
        let mut engine = test_engine().0;
        engine.enter_needs_login();
        let published = engine.state.auth_url.clone().expect("a url is published");
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();

        let error = engine.login(&sender).expect_err("the port is unavailable");
        assert!(error.contains("5588"), "the port must be named: {error}");
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin,
            "a login that cannot start must not claim to be authenticating"
        );
        assert!(!engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some(published.as_str()),
            "the prepared attempt survives for the retry"
        );
        assert!(engine.pending_auth.is_some());
        drop(squatter);
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

    /// Awaits one signal from the engine, so a task that never answers fails
    /// the test instead of hanging it.
    async fn receive_auth_signal(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<AuthSignal>,
    ) -> AuthSignal {
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("the engine answers within the timeout")
            .expect("the engine holds the sender")
    }

    /// Waits for the load the engine issued to reach librespot. librespot
    /// announces every load with `PlayRequestIdChanged` before it fetches
    /// anything, so this is the only evidence of a real load that exists
    /// outside the engine — and it is what "the row is playing again" has to
    /// mean when the load itself cannot succeed without Spotify.
    async fn receive_load(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<PlayerSignal>) {
        let loaded = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(signal) = receiver.recv().await {
                if matches!(
                    signal,
                    PlayerSignal::Event {
                        event: PlayerEvent::PlayRequestIdChanged { .. },
                        ..
                    }
                ) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(
            matches!(loaded, Ok(true)),
            "the engine's load must reach librespot"
        );
    }

    /// Puts the engine in the state a machine with no output device boots into,
    /// through the real player-construction path: the device step fails, the
    /// session survives it, and the engine is left holding a message instead of
    /// a player.
    async fn engine_without_output_device(
        engine: &mut Engine,
        device: &Arc<TestAudioDevice>,
    ) -> librespot_core::Session {
        let session = librespot_core::Session::new(librespot_core::SessionConfig::default(), None);
        let cache = librespot_core::cache::Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache");
        let message = match create_playback(session.clone(), cache, false, device.opener()).await {
            Err(PlaybackError::NoOutputDevice(message)) => message,
            Err(error) => panic!("a missing device must be a device failure, got {error}"),
            Ok(_) => panic!("a machine with no output device must not produce a player"),
        };
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Ok(ConnectedSession {
                    session: session.clone(),
                    playback: Err(message),
                }),
            },
            player_sender,
        ));
        session
    }

    /// Booting with no output device must not cost the session, the browsing or
    /// the engine itself. Before this, the device step panicked on librespot's
    /// player thread, playback construction failed, the session was shut down
    /// with it, and the engine answered every browse command with a login
    /// prompt — for a machine whose only problem was that its dongle had not
    /// been recognised yet.
    #[tokio::test(flavor = "current_thread")]
    async fn no_output_device_leaves_a_live_session_browsing_and_says_why() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener());
        let session = engine_without_output_device(&mut engine, &device).await;

        assert_eq!(device.opened(), 0, "nothing was opened on a machine with no device");
        assert!(
            !session.is_invalid(),
            "the output device is not the session's business"
        );
        assert!(engine.state.ready, "the engine is up");
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::Ready,
            "a missing device is not an authentication failure"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("no audio output device")),
            "the user is told the cause, not a WASAPI stack trace: {:?}",
            engine.state.error
        );
        assert!(engine.player.is_none(), "no player is installed without a device");
        assert!(engine.session.is_some(), "the session is installed");
        assert!(
            engine.browse_session_clone().is_ok(),
            "browse commands have their session"
        );
        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            matches!(
                engine.process_command(Command::Status, &auth_sender).await,
                Ok(true)
            ),
            "status still answers"
        );
        let refused = engine
            .player()
            .err()
            .expect("there is no player to reach");
        assert!(
            refused.contains("no audio output device"),
            "a command that needs a player names the device: {refused}"
        );
    }

    /// The other direction, and the owner's expectation: the device appears, so
    /// the engine builds the player it could not build before, forgets the
    /// message, and plays what was asked for — from where the sound stopped.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_that_appears_rebuilds_the_player_and_resumes_the_queue() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener());
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.update_position(42_000);
        let session = engine_without_output_device(&mut engine, &device).await;
        assert!(engine.player.is_none());

        device.plug_in();
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::unbounded_channel();
        // The probe waits its backoff before it runs; bring the due time
        // forward the way the heartbeat would.
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device")
            .retry_at = Instant::now() - Duration::from_millis(1);
        assert!(
            !engine.tick_audio_device(&auth_sender),
            "starting a probe is not itself a state change"
        );
        let signal = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(signal, player_sender));

        assert_eq!(device.opened(), 1, "the probe opened the device");
        assert!(engine.player.is_some(), "the player is back");
        assert!(engine.audio_unavailable.is_none());
        assert!(
            engine.state.error.is_none(),
            "the message goes with the condition that produced it"
        );
        assert!(engine.state.playing, "the requested playback is resumed");
        assert!(
            !engine.current_needs_load,
            "the row is loaded again, not merely marked as needing it"
        );
        assert_eq!(
            engine.state.position_ms, 42_000,
            "it resumes where the sound stopped"
        );
        assert!(!session.is_invalid(), "on the session that never went away");
        receive_load(&mut player_receiver).await;
    }

    /// A device that disappears mid-track lands in the same recoverable state,
    /// and does not take the session, the queue or the intent to resume with
    /// it. The sink's report of the stall is the only evidence there is:
    /// librespot turns the write error into a pause and never mentions the
    /// device, and the pause arrives on another channel, in either order.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_lost_mid_playback_returns_to_the_recoverable_state() {
        let device = TestAudioDevice::present();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener());
        let probe = SinkProbe::new();
        let (player, session) = probe_player(&probe);
        let player_handle = Arc::downgrade(&player);
        engine.state = playback_state(240_000);
        engine.state.playing = true;
        engine.player = Some(player);
        engine.session = Some(session.clone());
        engine.play_request_id = Some(7);
        engine.update_position(42_000);
        note_audio(&mut engine);
        let generation = engine.generation;

        assert!(engine.on_audio_signal(AudioSignal::OutputStalled {
            revision: engine.audio_revision,
        }));

        assert!(engine.player.is_none(), "the dead player is not left installed");
        assert!(
            player_handle.upgrade().is_none() && !probe.is_alive(),
            "and it is really gone, sink and all"
        );
        assert!(engine.session.is_some(), "the session is not the device's");
        assert!(engine.state.ready);
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::Ready
        );
        assert!(
            engine.state.playing,
            "the user asked for this track and has not asked to stop it"
        );
        assert!(
            engine
                .state
                .error
                .as_deref()
                .is_some_and(|message| message.contains("stopped responding")),
            "the stall is reported as what it is: {:?}",
            engine.state.error
        );
        assert!(engine.audio_unavailable.is_some(), "the probe is armed");
        assert!(engine.current_needs_load, "the row will have to be loaded again");
        assert_eq!(engine.state.position_ms, 42_000);
        assert!(
            !engine.tick_position(),
            "nothing is audible, so the playhead does not move"
        );

        // librespot's own reaction to the same failure, and the player thread
        // ending behind it, both belong to the player that was just let go.
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation,
            event: PlayerEvent::Paused {
                play_request_id: 7,
                track_id: track_uri(),
                position_ms: 42_000,
            },
        }));
        assert!(!engine.on_player_signal(PlayerSignal::Closed { generation }));
        assert!(
            engine.session.is_some(),
            "a dead player thread must not take the session with it"
        );
        assert!(
            engine.state.playing,
            "and must not erase the intent to resume"
        );

        // Still no device: the probe answers with the real cause, which
        // replaces the stall's wording, and schedules the next attempt.
        device.unplug();
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::unbounded_channel();
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device")
            .retry_at = Instant::now() - Duration::from_millis(1);
        assert!(!engine.tick_audio_device(&auth_sender));
        let signal = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(signal, player_sender.clone()));
        let message = engine.state.error.clone().expect("still no sound");
        assert!(message.contains("no audio output device"), "{message}");
        assert_eq!(device.opened(), 0);

        // The dongle is plugged back in.
        device.plug_in();
        engine
            .audio_unavailable
            .as_mut()
            .expect("no device")
            .retry_at = Instant::now() - Duration::from_millis(1);
        assert!(!engine.tick_audio_device(&auth_sender));
        let signal = receive_auth_signal(&mut auth_receiver).await;
        assert!(engine.on_auth_signal(signal, player_sender));

        assert!(engine.player.is_some(), "the player comes back");
        assert!(engine.state.error.is_none(), "and the message does not");
        assert!(engine.state.playing, "the track plays again");
        assert_eq!(engine.state.position_ms, 42_000);
        receive_load(&mut player_receiver).await;
    }

    /// The device exemption is from the device check, not from the session's:
    /// an engine that has just logged out must not accept a volume change
    /// because the machine also happens to have no output device. The device
    /// state is forgotten with the session, so the reason it gives is the real
    /// one.
    #[tokio::test(flavor = "current_thread")]
    async fn a_device_failure_never_stands_in_for_a_missing_session() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener());
        let session = engine_without_output_device(&mut engine, &device).await;
        assert!(engine.audio_unavailable.is_some());

        assert!(engine.logout().expect("logout succeeds"));
        assert_eq!(
            engine.state.auth_state,
            renderer_engine::protocol::AuthState::NeedsLogin
        );
        assert!(
            engine.audio_unavailable.is_none(),
            "the device's failure does not outlive the session it was noticed on"
        );
        assert!(session.is_invalid(), "and the session really ends");

        let (auth_sender, _auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let refused = engine
            .process_command(Command::SetVolume { percent: 30 }, &auth_sender)
            .await
            .expect_err("a volume change is not what a signed-out engine needs");
        assert!(refused.contains("login"), "{refused}");
        assert_eq!(engine.state.volume, 50, "and the volume did not move");
    }

    /// The retry clock: one probe at a time, each failure waiting longer than
    /// the last, and a ceiling — a machine that has been without audio since
    /// boot must not enumerate devices on every heartbeat for the rest of the
    /// day, and a device that lands must still be picked up in seconds.
    #[tokio::test(flavor = "current_thread")]
    async fn device_probes_back_off_to_a_ceiling_one_at_a_time() {
        let device = TestAudioDevice::absent();
        let (mut engine, _) = test_engine_with_audio(PathBuf::new(), device.opener());
        engine_without_output_device(&mut engine, &device).await;
        let (auth_sender, mut auth_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (player_sender, _player_receiver) = tokio::sync::mpsc::unbounded_channel();

        assert!(!engine.tick_audio_device(&auth_sender));
        assert!(
            auth_receiver.try_recv().is_err(),
            "the first probe waits its backoff rather than firing immediately"
        );
        let mut waits = Vec::new();
        for _ in 0..5 {
            let retry_at = engine.audio_unavailable.as_ref().expect("no device").retry_at;
            waits.push(retry_at.saturating_duration_since(Instant::now()));
            // Bring the due time forward, the way a heartbeat would.
            engine
                .audio_unavailable
                .as_mut()
                .expect("no device")
                .retry_at = Instant::now() - Duration::from_millis(1);
            assert!(!engine.tick_audio_device(&auth_sender));
            // The probe holds the clock while it runs, so a tick arriving
            // during it cannot start a second one against the same device.
            let held = engine.audio_unavailable.as_ref().expect("no device").retry_at;
            assert!(held > Instant::now(), "the probe holds the clock");
            assert!(!engine.tick_audio_device(&auth_sender));
            assert_eq!(
                engine.audio_unavailable.as_ref().expect("no device").retry_at,
                held,
                "and a second tick leaves it exactly where the probe put it"
            );
            let signal = receive_auth_signal(&mut auth_receiver).await;
            assert!(engine.on_auth_signal(signal, player_sender.clone()));
            assert_eq!(device.opened(), 0, "there is still nothing to open");
        }

        assert!(
            waits
                .windows(2)
                .all(|pair| pair[0] <= pair[1] + Duration::from_millis(1)),
            "each failure waits at least as long as the last: {waits:?}"
        );
        assert!(
            waits.iter().all(|wait| *wait <= AUDIO_PROBE_BACKOFF_MAX),
            "no wait may outlast the ceiling: {waits:?}"
        );
        assert_eq!(
            engine.audio_probe_backoff, AUDIO_PROBE_BACKOFF_MAX,
            "and the backoff stops growing there"
        );
        assert!(
            AUDIO_PROBE_BACKOFF_MIN >= Duration::from_secs(1)
                && AUDIO_PROBE_BACKOFF_MAX <= Duration::from_secs(30),
            "the ceiling is what bounds how long a plugged-in device stays silent"
        );
    }
}
