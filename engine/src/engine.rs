use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_metadata::Metadata;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::auth::{
    PendingAuth, PlaybackHandles, complete_oauth, connect_cached, percent_to_volume,
    prepare_oauth,
};
use crate::io::ProtocolWriter;
use spotify_playback_engine::protocol::{AuthState, BrowseResponse, Command, PositionEvent, RepeatMode, Response, StateEvent, TrackRef};
use serde::Serialize;
/// Pressing previous within this many milliseconds of a track start restarts
/// the current track instead of switching tracks. Mirrors the UI's optimistic
/// restart window (OnPrevious in app.cpp) so both sides agree.
const PREVIOUS_RESTART_THRESHOLD_MS: u32 = 3_000;
/// Minimum spacing between command-driven track changes (PlayQueue, Next,
/// Previous). Loading an uncached track fetches its audio decryption key
/// from Spotify's key service, which rate-limits bursts ("Unable to load
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
    history: Vec<usize>,
    random_state: u64,
    generation: u64,
    /// A seek is mid-transition (the player was paused by [`Engine::seek`]).
    /// While set, the transient pause event from the seek is suppressed, and
    /// a stale Playing event cannot override the seek's requested play/pause
    /// intent.
    seek_in_flight: bool,
    /// Whether the seek that set `seek_in_flight` was issued while playing.
    seek_should_play: bool,
    auth_running: bool,
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
    current_index: Option<usize>,
    queue: Vec<TrackRef>,
    error: Option<String>,
}

pub enum AuthSignal {
    Complete {
        generation: u64,
        result: Result<PlaybackHandles, String>,
    },
}

pub enum PlayerSignal {
    Event {
        generation: u64,
        event: PlayerEvent,
    },
    Closed {
        generation: u64,
    },
}

impl Engine {
    pub fn new(
        writer: ProtocolWriter,
        cache: Cache,
        temporary_directory: std::path::PathBuf,
        credentials_file: std::path::PathBuf,
    ) -> Self {
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
                current_index: None,
                queue: Vec::new(),
                error: None,
            },
            player: None,
            mixer: None,
            session: None,
            play_request_id: None,
            loading_failed: false,
            position_anchor: None,
            last_track_change: None,
            recent_track_changes: VecDeque::new(),
            recent_unavailable: VecDeque::new(),
            next_reconnect: None,
            reconnect_backoff: RECONNECT_BACKOFF_MIN,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            random_state,
            generation: 0,
            seek_in_flight: false,
            seek_should_play: false,
            auth_running: false,
            pending_auth: None,
        }
    }

    pub fn writer(&self) -> &ProtocolWriter {
        &self.writer
    }

    pub fn emit_state(&self) -> Result<(), String> {
        let current_uri = self
            .state
            .current_index
            .and_then(|index| self.state.queue.get(index))
            .map(|track| track.uri.as_str());
        self.writer.send(&StateEvent {
            kind: "state",
            ready: self.state.ready,
            auth_state: self.state.auth_state,
            auth_url: self.state.auth_url.as_deref(),
            playing: self.state.playing,
            username: self.session.as_ref().map(|session| session.username()),
            position_ms: self.state.position_ms,
            duration_ms: self.state.duration_ms,
            volume: self.state.volume,
            shuffle: self.state.shuffle,
            repeat: self.state.repeat,
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
        self.writer.send(&PositionEvent {
            kind: "position",
            position_ms: self.state.position_ms,
            duration_ms: self.state.duration_ms,
        })
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
        let elapsed_ms = u32::try_from(anchor_time.elapsed().as_millis())
            .unwrap_or(u32::MAX);
        self.state.position_ms = anchor_position_ms
            .saturating_add(elapsed_ms)
            .min(self.state.duration_ms);
        true
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
    pub fn tick_session_health(
        &mut self,
        sender: &mpsc::UnboundedSender<AuthSignal>,
    ) -> bool {
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
            self.state.ready = false;
            self.state.playing = false;
            self.state.error =
                Some("the Spotify connection dropped; reconnecting".to_owned());
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
        tokio::spawn(async move {
            let result = connect_cached(cache, temporary_directory).await;
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
    pub fn login(&mut self, auth_sender: &mpsc::UnboundedSender<AuthSignal>) -> Result<bool, String> {
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
        tokio::spawn(async move {
            let result = complete_oauth(cache, temporary_directory, pending).await;
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
        let AuthSignal::Complete { generation, result } = signal;
        if generation != self.generation {
            return false;
        }
        self.auth_running = false;
        match result {
            Ok(handles) => {
                self.state.ready = true;
                self.state.auth_state = AuthState::Ready;
                self.state.volume = handles.volume_percent;
                self.state.error = None;
                // The attempt is spent: the URL it published no longer applies.
                self.state.auth_url = None;
                self.pending_auth = None;
                self.player = Some(handles.player);
                self.mixer = Some(handles.mixer);
                self.session = Some(handles.session);
                // A live session again: drop any reconnect schedule and reset
                // the backoff so the next outage recovers quickly too.
                self.next_reconnect = None;
                self.reconnect_backoff = RECONNECT_BACKOFF_MIN;
                let mut events = handles.events;
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
                true
            }
            Err(error) => {
                eprintln!("Spotify playback authentication failed: {error}");
                // A failed login returns to needs_login with a fresh URL so
                // the user can retry with the Log in button.
                self.enter_needs_login();
                self.state.error = Some(error);
                true
            }
        }
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
        Self::prune_recent_times(
            &mut self.recent_unavailable,
            now,
            UNAVAILABLE_BURST_WINDOW,
        );
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
        let wait =
            track_change_wait(self.last_track_change, Instant::now(), TRACK_CHANGE_MIN_INTERVAL);
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
        if matches!(&command, Command::Login) {
            return self.login(auth_sender);
        }
        if matches!(&command, Command::Logout) {
            return self.logout();
        }
        self.ensure_ready()?;
        // Pace command-driven track changes so rapid next/prev spam cannot
        // burst audio-key requests (each load of an uncached track fetches
        // its decryption key; the key service rate-limits bursts and
        // playback dies). Only PlayQueue/Next/Previous are paced; other
        // commands may wait a tick — no press is dropped.
        if matches!(
            &command,
            Command::PlayQueue { .. } | Command::PlayQueueIndex { .. } | Command::Next | Command::Previous
        ) {
            self.pace_track_change().await;
        }
        match command {
            Command::Status
            | Command::Shutdown
            | Command::Login
            | Command::Logout
            | Command::BrowsePlaylists { .. }
            | Command::BrowsePlaylist { .. }
            | Command::BrowseAlbum { .. }
            | Command::BrowseArtist { .. }
            | Command::BrowseArtistReleases { .. }
            | Command::BrowseArtistCatalogue { .. }
            | Command::BrowseLikedSongs { .. }
            | Command::BrowseSearch { .. }
            | Command::BrowseTrackCredits { .. }
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
            } => self.play_queue(queue, index, position_ms),
            Command::PlayQueueIndex { index } => self.play_queue_index(index),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Next => self.advance(false),
            Command::Previous => self.previous(),
            Command::Seek { position_ms } => self.seek(position_ms),
            Command::SetVolume { percent } => self.set_volume(percent),
            Command::SetShuffle { enabled } => self.set_shuffle(enabled),
            Command::SetRepeat { mode } => self.set_repeat(mode),
            Command::AddQueue { track } => self.add_queue(track),
            Command::AddQueueBatch { tracks } => self.add_queue_batch(tracks),
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
        self.session.clone().ok_or_else(|| match self.state.auth_state {
            AuthState::Authenticating => "Spotify authentication is still in progress".to_owned(),
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
                    if let Some(session) = self.session.take() {
                        session.shutdown();
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub fn shutdown(&mut self) {
        self.auth_running = false;
        self.generation = self.generation.wrapping_add(1);
        self.shutdown_playback();
        self.state.ready = false;
        self.state.playing = false;
    }

    fn ensure_ready(&self) -> Result<(), String> {
        if self.state.ready && self.player.is_some() {
            Ok(())
        } else {
            Err(match self.state.auth_state {
                AuthState::Authenticating => "Spotify authentication is still in progress".to_owned(),
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

    fn play_queue(
        &mut self,
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
    ) -> Result<bool, String> {
        for track in &queue {
            parse_track_uri(track)?;
        }
        if queue.is_empty() {
            if index != 0 {
                return Err("index must be zero for an empty queue".to_owned());
            }
            self.player()?.stop();
            self.state.queue.clear();
            self.state.current_index = None;
            self.state.position_ms = 0;
            self.state.duration_ms = 0;
            self.state.playing = false;
            self.history.clear();
            self.shuffle_pool.clear();
            self.state.error = None;
            self.loading_failed = false;
            self.recent_track_changes.clear();
            self.recent_unavailable.clear();
            return Ok(true);
        }
        if index >= queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }

        self.state.queue = queue;
        self.state.current_index = Some(index);
        self.state.duration_ms = self.state.queue[index].duration_ms;
        self.update_position(position_ms);
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

    fn play_queue_index(&mut self, index: usize) -> Result<bool, String> {
        if index >= self.state.queue.len() {
            return Err(format!("queue index {index} is out of range"));
        }
        if let Some(current) = self.state.current_index {
            if current != index {
                self.history.push(current);
            }
        }
        self.state.current_index = Some(index);
        self.state.duration_ms = self.state.queue[index].duration_ms;
        self.update_position(0);
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
        if self.loading_failed
            || (self.state.duration_ms > 0 && self.state.position_ms >= self.state.duration_ms)
        {
            if !self.loading_failed {
                self.state.position_ms = 0;
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
        // `Unavailable` stops the player below, so a later pause must not send
        // `pause` to librespot's terminal Loading/Stopped state. Play will
        // issue a fresh load when requested.
        if !self.loading_failed {
            self.player()?.pause();
        }
        self.state.playing = false;
        self.update_position(self.state.position_ms);
        self.state.error = None;
        Ok(true)
    }

    fn seek(&mut self, position_ms: u32) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        let position = position_ms.min(self.state.duration_ms);
        if self.loading_failed {
            // The failed loader was stopped by the Unavailable handler. A
            // seek is also a valid recovery request: load at the new offset
            // while preserving the current play/pause intent.
            let start_playing = self.state.playing;
            self.update_position(position);
            self.load_current(start_playing)?;
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
            if self.state.shuffle && !self.shuffle_pool.contains(&current) {
                self.shuffle_pool.push(current);
            }
            self.state.current_index = Some(index);
            self.state.duration_ms = self.state.queue[index].duration_ms;
            self.update_position(0);
            self.state.error = None;
            let start_playing = self.state.playing;
            self.load_current(start_playing)?;
            Ok(true)
        } else {
            // No earlier track (first track, or already restarting): seek the
            // current track back to its beginning instead of erroring.
            self.seek(0)
        }
    }

    /// Decides which track `previous` should switch to. `Some(index)` names a
    /// valid queue index; `None` means "restart the current track". The
    /// position restart threshold matches the UI's 3-second restart window, so
    /// the optimistic flip and the engine agree. History entries that no longer
    /// index the queue (stale after a mutation) are dropped instead of
    /// panicking.
    fn previous_index(&mut self) -> Option<usize> {
        if self.state.position_ms > PREVIOUS_RESTART_THRESHOLD_MS {
            return None;
        }
        while let Some(index) = self.history.pop() {
            if index < self.state.queue.len() {
                return Some(index);
            }
        }
        let current = self.state.current_index?;
        if !self.state.shuffle && current > 0 && current <= self.state.queue.len() {
            return Some(current - 1);
        }
        None
    }

    fn advance(&mut self, at_end: bool) -> Result<bool, String> {
        if at_end {
            // A natural end-of-track advance is not user-paced, but it
            // refreshes the change clock so a press right after a track ends
            // is not double-waited (the load has already started).
            self.last_track_change = Some(Instant::now());
        }
        let current = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let next = self.take_next_index(at_end);
        match next {
            Some(index) => {
                if index != current {
                    self.history.push(current);
                }
                self.state.current_index = Some(index);
                self.state.duration_ms = self.state.queue[index].duration_ms;
                self.update_position(0);
                self.state.error = None;
                self.load_current(true)?;
            }
            None => {
                self.player()?.stop();
                self.state.playing = false;
                self.update_position(self.state.duration_ms);
            }
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

    fn add_queue(&mut self, track: TrackRef) -> Result<bool, String> {
        parse_track_uri(&track)?;
        self.state.queue.push(track);
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.preload_next();
        Ok(true)
    }

    fn add_queue_batch(&mut self, tracks: Vec<TrackRef>) -> Result<bool, String> {
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
        self.state.queue.remove(index);
        self.history.clear();
        let mut reload = false;

        match current {
            None => {}
            Some(_) if self.state.queue.is_empty() => {
                self.player()?.stop();
                self.state.current_index = None;
                self.state.duration_ms = 0;
                self.state.playing = false;
                self.update_position(0);
                self.play_request_id = None;
            }
            Some(current) if index == current => {
                let replacement = current.min(self.state.queue.len() - 1);
                self.state.current_index = Some(replacement);
                self.state.duration_ms = self.state.queue[replacement].duration_ms;
                self.update_position(0);
                reload = true;
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
        let index = self
            .state
            .current_index
            .ok_or_else(|| "the queue has no current track".to_owned())?;
        let track = self.state.queue.get(index).ok_or_else(|| {
            "the queue has no current track (index out of range)".to_owned()
        })?;
        let uri = parse_track_uri(track)?;
        let position_ms = self.state.position_ms;
        let next_uri = self
            .peek_next_index()
            .and_then(|next| parse_track_uri(&self.state.queue[next]).ok());
        self.play_request_id = None;
        self.seek_in_flight = false;
        self.seek_should_play = false;
        // Clone the Arc so the immutable borrow of `self.player` ends before
        // the recovery bookkeeping below mutates the engine.
        let player = Arc::clone(self.player()?);
        self.loading_failed = false;
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
        let queue_len = self.state.queue.len();
        if at_end && self.state.repeat == RepeatMode::Track {
            return (current < queue_len).then_some(current);
        }
        if self.state.shuffle {
            if self.shuffle_pool.is_empty() && self.state.repeat == RepeatMode::Context {
                self.rebuild_shuffle_pool();
            }
            return self.shuffle_pool.pop().filter(|index| *index < queue_len);
        }
        sequential_next_index(current, queue_len, self.state.repeat)
    }

    fn peek_next_index(&self) -> Option<usize> {
        let current = self.state.current_index?;
        let queue_len = self.state.queue.len();
        if self.state.repeat == RepeatMode::Track {
            return (current < queue_len).then_some(current);
        }
        if self.state.shuffle {
            return self.shuffle_pool.last().copied().filter(|index| *index < queue_len);
        }
        sequential_next_index(current, queue_len, self.state.repeat)
    }

    fn rebuild_shuffle_pool(&mut self) {
        self.shuffle_pool.clear();
        if !self.state.shuffle {
            return;
        }
        let current = self.state.current_index;
        self.shuffle_pool.extend(
            (0..self.state.queue.len()).filter(|index| Some(*index) != current),
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
                self.clear_unavailable_burst();
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
                self.state.playing = false;
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
                self.state.playing = false;
                self.state.error = Some(format!("Spotify track is unavailable: {track_id}"));

                // librespot leaves the failed loader in PlayerState::Loading
                // after sending Unavailable. Stop it explicitly so a later
                // Play can submit a fresh Load command instead of toggling
                // start_playback on a terminated future.
                if let Some(player) = &self.player {
                    player.stop();
                }

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
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.play_request_id = None;
        self.loading_failed = false;
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

fn parse_track_uri(track: &TrackRef) -> Result<SpotifyUri, String> {
    let uri = SpotifyUri::from_uri(&track.uri)
        .map_err(|error| format!("invalid Spotify track URI '{}': {error}", track.uri))?;
    if !matches!(&uri, SpotifyUri::Track { .. }) {
        return Err(format!("queue item is not a Spotify track URI: {}", track.uri));
    }
    Ok(uri)
}

fn sequential_next_index(
    current: usize,
    queue_len: usize,
    repeat: RepeatMode,
) -> Option<usize> {
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
    use std::sync::Arc;
    use std::time::{Duration, Instant};


    use librespot_core::SpotifyUri;
    use librespot_playback::player::PlayerEvent;
    use super::{
        remap_current_index_after_move, sequential_next_index, track_change_wait, AuthSignal,
        Engine, PlaybackState, PlayerSignal, RECONNECT_BACKOFF_MAX, RECONNECT_BACKOFF_MIN,
        TRACK_CHANGE_BURST_WINDOW, TRACK_CHANGE_MIN_INTERVAL, UNAVAILABLE_BURST_WINDOW,
    };
    use crate::io::ProtocolWriter;
    use spotify_playback_engine::protocol::{RepeatMode, TrackRef};

    fn test_engine() -> (Engine, Arc<std::sync::Mutex<Vec<u8>>>) {
        let (writer, buffer) = ProtocolWriter::capture();
        let cache = librespot_core::cache::Cache::new(
            None::<PathBuf>,
            None::<PathBuf>,
            None::<PathBuf>,
            None,
        )
        .expect("cache with no paths");
        (
            Engine::new(writer, cache, PathBuf::new(), PathBuf::new()),
            buffer,
        )
    }

    fn track_ref() -> TrackRef {
        TrackRef {
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
            current_index: Some(0),
            queue: vec![track_ref()],
            error: None,
        }
    }

    fn playing_engine() -> Engine {
        let (mut engine, _) = test_engine();
        engine.state = playback_state(240_000);
        engine.play_request_id = Some(7);
        engine
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

        // A retry gets a new player request id, then Loading clears the
        // failed-loader marker before playback begins.
        assert!(!engine.on_player_event(PlayerEvent::PlayRequestIdChanged {
            play_request_id: 8,
        }));
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
        let session = librespot_core::Session::new(
            librespot_core::SessionConfig::default(),
            None,
        );
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
        assert!(engine.next_reconnect.is_none(), "a live session is not a corpse");
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
        assert!(!engine.unavailable_is_clustered(
            start + Duration::from_millis(10)
        ));

        // A second failure shortly afterward is clustered, so valid cache
        // files are preserved while the key/network burst settles.
        assert!(engine.unavailable_is_clustered(
            start + Duration::from_millis(20)
        ));

        // After both windows expire, cleanup is eligible again.
        let quiet = start
            + TRACK_CHANGE_BURST_WINDOW
            .max(UNAVAILABLE_BURST_WINDOW)
            + Duration::from_millis(25);
        assert!(!engine.unavailable_is_clustered(quiet));

        // Two paced load starts protect even the first failure observed after
        // a rapid-click burst.
        engine.note_track_change(quiet);
        engine.note_track_change(quiet + Duration::from_millis(100));
        assert!(engine.unavailable_is_clustered(
            quiet + Duration::from_millis(200)
        ));
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
        assert!(!object.contains_key("queue"), "a heartbeat never carries the queue");
        assert!(!object.contains_key("playing"), "a heartbeat carries no flags");
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
        assert_eq!(engine.previous_index(), Some(0), "falls back to current - 1");
        engine.state.current_index = Some(0);
        engine.history.push(99);
        assert_eq!(engine.previous_index(), None, "first track: restart in place");
    }

    #[test]
    fn previous_without_a_player_errors_gracefully() {
        let mut engine = playing_engine();
        engine.state.position_ms = 0;
        // No previous target -> seek(0) -> no player attached: the command must
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
        assert!(!engine.on_player_signal(PlayerSignal::Event {
            generation: engine.generation,
            event: PlayerEvent::Playing {
                play_request_id: engine.play_request_id.unwrap_or(7),
                track_id: track_uri(),
                position_ms: 0,
            },
        }), "events for a missing queue index are ignored, not panicked");
        engine.state.current_index = None;
        assert!(engine.previous().is_err());
        assert!(engine.advance(false).is_err());
        assert!(engine.load_current(true).is_err());
    }

    #[test]
    fn sequential_queue_stops_or_wraps_at_the_end() {
        assert_eq!(sequential_next_index(0, 3, RepeatMode::Off), Some(1));
        assert_eq!(sequential_next_index(2, 3, RepeatMode::Off), None);
        assert_eq!(
            sequential_next_index(2, 3, RepeatMode::Context),
            Some(0)
        );
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
                Engine::new(writer, cache, PathBuf::new(), fixture.credentials_file()),
                buffer,
            )
        };
        // A live session: playback state that logout must tear down.
        engine.state = playback_state(240_000);
        engine.state.playing = true;

        assert!(engine.logout().expect("logout succeeds"));
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin);
        assert!(!engine.state.ready);
        assert!(!engine.state.playing);
        assert!(engine.state.current_index.is_none());
        assert!(engine.state.queue.is_empty());
        let published_url = engine.state.auth_url.clone().expect("auth url published");
        assert!(published_url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(!fixture.credentials_exist(), "credentials file must be removed");

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
        let mut engine =
            Engine::new(writer, cache, PathBuf::new(), fixture.credentials_file());
        assert!(engine.logout().expect("first logout"));
        assert!(!fixture.credentials_exist());
        assert!(engine.logout().expect("second logout is a no-op"));
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin);
    }

    #[test]
    fn startup_without_cached_credentials_enters_needs_login_without_a_flow() {
        let (mut engine, buffer) = test_engine(); // cache has no credentials
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.start_authentication(sender);
        assert!(!engine.auth_running, "no implicit flow may start");
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin);
        assert!(engine.state.auth_url.is_some());

        engine.emit_state().expect("state emits");
        let line = {
            let mut bytes = buffer.lock().expect("buffer lock");
            std::mem::take(&mut *bytes)
        };
        let value: serde_json::Value = serde_json::from_slice(&line).expect("state json");
        assert_eq!(value["auth_state"], "needs_login");
        assert!(value["auth_url"].as_str().is_some_and(|url| {
            url.starts_with("https://accounts.spotify.com/authorize?")
        }));
    }

    #[test]
    fn login_is_a_noop_while_a_session_is_live() {
        let mut engine = playing_engine(); // ready with a player-less state
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::Ready;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.login(&sender).expect("login no-ops when authenticated"));
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
        assert!(engine.login(&sender).expect("login no-ops while authenticating"));
        assert!(engine.auth_running);
        assert_eq!(
            engine.state.auth_url.as_deref(),
            Some("https://accounts.spotify.com/authorize?running"),
            "an in-flight attempt keeps its URL"
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
        assert_eq!(pending.auth_url, published, "the flow must use the published URL");
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::Authenticating);
        assert!(engine.auth_running);
        assert_eq!(engine.state.auth_url.as_deref(), Some(published.as_str()));
        assert!(engine.pending_auth.is_none());
    }

    #[test]
    fn begin_login_flow_prepares_a_fresh_attempt_when_none_is_pending() {
        let mut engine = test_engine().0;
        engine.state.auth_state = spotify_playback_engine::protocol::AuthState::NeedsLogin;
        let pending = engine.begin_login_flow().expect("flow begins");
        assert!(pending.auth_url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::Authenticating);
        assert_eq!(engine.state.auth_url.as_deref(), Some(pending.auth_url.as_str()));
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
        assert!(engine.state.auth_state == spotify_playback_engine::protocol::AuthState::NeedsLogin);
        assert!(!engine.state.ready);
        assert!(!engine.auth_running);
        let retry_url = engine.state.auth_url.clone().expect("retry url published");
        assert_ne!(
            retry_url, "https://accounts.spotify.com/authorize?first",
            "a retry must regenerate the URL"
        );
        assert!(engine.state.error.as_deref().is_some_and(|error| {
            error.contains("Spotify authentication failed")
        }));
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
