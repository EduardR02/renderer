use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::auth::{PlaybackHandles, authenticate, percent_to_volume};
use crate::io::ProtocolWriter;
use crate::protocol::{
    AuthState, Command, RepeatMode, Response, StateEvent, TrackRef, WebApiTokenResponse,
};
/// Pressing previous within this many milliseconds of a track start restarts
/// the current track instead of switching tracks. Mirrors the UI's optimistic
/// restart window (OnPrevious in app.cpp) so both sides agree.
const PREVIOUS_RESTART_THRESHOLD_MS: u32 = 3_000;

/// Web API tokens are refreshed this far ahead of their real expiry so a
/// request issued near the boundary never races server-side revocation.
const WEB_TOKEN_SKEW: Duration = Duration::from_secs(60);

/// An engine-minted login5 token plus its local expiry anchor.
struct CachedWebToken {
    token_type: String,
    access_token: String,
    expires_at: Instant,
}

pub struct Engine {

    writer: ProtocolWriter,
    cache: Cache,
    temporary_directory: std::path::PathBuf,
    state: PlaybackState,
    player: Option<Arc<Player>>,
    mixer: Option<Arc<SoftMixer>>,
    session: Option<librespot_core::Session>,
    play_request_id: Option<u64>,
    position_anchor: Option<(u32, Instant)>,
    shuffle_pool: Vec<usize>,
    history: Vec<usize>,
    random_state: u64,
    generation: u64,
    auth_running: bool,
    /// Cached login5-minted Web API token, refreshed with skew by
    /// [`Engine::web_api_token`].
    web_token: Option<CachedWebToken>,
}

struct PlaybackState {
    ready: bool,
    auth_state: AuthState,
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
            state: PlaybackState {
                ready: false,
                auth_state: AuthState::Authenticating,
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
            position_anchor: None,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            random_state,
            generation: 0,
            auth_running: false,
            web_token: None,
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
            playing: self.state.playing,
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

    /// Records an authoritative playback position and re-anchors the drift
    /// projection at the current wall clock. Called on every player event and
    /// command that establishes a position (track change, play/pause, seek,
    /// position correction) so `tick_position` projects from the newest truth.
    fn update_position(&mut self, position_ms: u32) {
        self.state.position_ms = position_ms.min(self.state.duration_ms);
        self.position_anchor = Some((self.state.position_ms, Instant::now()));
    }

    /// Advances the reported position from the latest anchor and reports
    /// whether a state event should be emitted. Emits at most once per call;
    /// while paused the position is static and no event is produced.
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
        let cache = self.cache.clone();
        let temporary_directory = self.temporary_directory.clone();
        tokio::spawn(async move {
            let result = authenticate(cache, temporary_directory).await;
            let _ = sender.send(AuthSignal::Complete { generation, result });
        });
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
                self.player = Some(handles.player);
                self.mixer = Some(handles.mixer);
                self.session = Some(handles.session);
                // A new session must never serve tokens minted for the old
                // one (the account may have changed); drop the cache.
                self.web_token = None;
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
                self.state.ready = false;
                self.state.auth_state = AuthState::Error;
                self.state.playing = false;
                // No usable session: any cached web token is stale.
                self.web_token = None;
                self.state.error = Some(error);
                true
            }
        }
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
        if matches!(&command, Command::WebApiToken) {
            return self.web_api_token().await;
        }
        self.ensure_ready()?;
        match command {
            Command::Status | Command::Shutdown | Command::WebApiToken => unreachable!(),
            Command::PlayQueue {
                queue,
                index,
                position_ms,
            } => self.play_queue(queue, index, position_ms),
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Next => self.advance(false),
            Command::Previous => self.previous(),
            Command::Seek { position_ms } => self.seek(position_ms),
            Command::SetVolume { percent } => self.set_volume(percent),
            Command::SetShuffle { enabled } => self.set_shuffle(enabled),
            Command::SetRepeat { mode } => self.set_repeat(mode),
            Command::AddQueue { track } => self.add_queue(track),
            Command::RemoveQueue { index } => self.remove_queue(index),
            Command::MoveQueue { from, to } => self.move_queue(from, to),
        }
    }

    /// Serves the cached login5-minted Web API token, re-minting through
    /// `session.login5().auth_token()` when the cached token is within
    /// `WEB_TOKEN_SKEW` of expiry. The token itself is never logged; failure
    /// messages carry only the login5 error text.
    async fn web_api_token(&mut self) -> Result<bool, String> {
        let now = Instant::now();
        if let Some(cached) = &self.web_token {
            if web_token_is_fresh(cached.expires_at, now) {
                return Ok(true);
            }
        }
        let session = self.session.as_ref().ok_or_else(|| match self.state.auth_state {
            AuthState::Authenticating => "Spotify authentication is still in progress".to_owned(),
            AuthState::Error => self
                .state
                .error
                .clone()
                .unwrap_or_else(|| "Spotify authentication failed".to_owned()),
            AuthState::Ready => "the Spotify session is unavailable".to_owned(),
        })?;
        let token = session
            .login5()
            .auth_token()
            .await
            .map_err(|error| format!("could not mint a Spotify Web API token: {error}"))?;
        self.web_token = Some(CachedWebToken {
            token_type: token.token_type,
            access_token: token.access_token,
            expires_at: now + token.expires_in,
        });
        Ok(true)
    }

    /// Payload for a successful `web_api_token` response: the cached token
    /// plus the skew-adjusted remaining lifetime in seconds (never zero).
    pub fn web_token_payload(&self) -> Option<(&str, &str, u64)> {
        self.web_token.as_ref().map(|cached| {
            (
                cached.token_type.as_str(),
                cached.access_token.as_str(),
                web_token_expires_in(cached.expires_at, Instant::now()),
            )
        })
    }

    /// Sends the dedicated `web_api_token` response: token fields only on
    /// success, error text only on failure.
    pub fn send_web_token_response(
        &self,
        request_id: &str,
        result: &Result<bool, String>,
    ) -> Result<(), String> {
        let (token_type, access_token, expires_in) = match (result, self.web_token_payload()) {
            (Ok(_), Some(payload)) => (Some(payload.0), Some(payload.1), Some(payload.2)),
            _ => (None, None, None),
        };
        self.writer.send(&WebApiTokenResponse {
            kind: "web_api_token",
            request_id,
            ok: result.is_ok(),
            error: result.as_ref().err().map(String::as_str),
            token_type,
            access_token,
            expires_in,
        })
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
        self.load_current(true)?;
        Ok(true)
    }

    fn play(&mut self) -> Result<bool, String> {
        if self.state.current_index.is_none() {
            return Err("the queue has no current track".to_owned());
        }
        if self.state.duration_ms > 0 && self.state.position_ms >= self.state.duration_ms {
            self.state.position_ms = 0;
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
        self.player()?.pause();
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
        self.player()?.seek(position);
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
        let volume = percent_to_volume(percent);
        self.mixer
            .as_ref()
            .ok_or_else(|| "the software mixer is unavailable".to_owned())?
            .set_volume(volume);
        self.cache.save_volume(volume);
        self.player()?.emit_volume_changed_event(volume);
        self.state.volume = percent;
        self.state.error = None;
        Ok(true)
    }

    fn set_shuffle(&mut self, enabled: bool) -> Result<bool, String> {
        self.state.shuffle = enabled;
        self.history.clear();
        self.rebuild_shuffle_pool();
        self.player()?.emit_shuffle_changed_event(enabled);
        self.preload_next();
        Ok(true)
    }

    fn set_repeat(&mut self, mode: RepeatMode) -> Result<bool, String> {
        self.state.repeat = mode;
        self.player()?.emit_repeat_changed_event(
            mode == RepeatMode::Context,
            mode == RepeatMode::Track,
        );
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
        let player = self.player()?;
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
            PlayerEvent::Playing {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
                self.state.playing = true;
                self.update_position(position_ms);
                self.state.error = None;
                true
            }
            PlayerEvent::Paused {
                play_request_id,
                track_id,
                position_ms,
            } if self.is_current_event(play_request_id, &track_id) => {
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
                self.state.playing = false;
                self.state.error = Some(format!("Spotify track is unavailable: {track_id}"));
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
        self.mixer = None;
        self.web_token = None;
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
    }
}

/// A cached token is still safe to serve when its remaining lifetime exceeds
/// the skew, so the consumer always receives a token that outlives a slow
/// request.
fn web_token_is_fresh(expires_at: Instant, now: Instant) -> bool {
    now + WEB_TOKEN_SKEW < expires_at
}

/// Skew-adjusted remaining lifetime reported to the UI in seconds. Never
/// zero, so a just-minted short-lived token still counts as usable.
fn web_token_expires_in(expires_at: Instant, now: Instant) -> u64 {
    expires_at
        .saturating_duration_since(now)
        .saturating_sub(WEB_TOKEN_SKEW)
        .as_secs()
        .max(1)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};


    use librespot_core::SpotifyUri;
    use librespot_playback::player::PlayerEvent;
    use super::{
        remap_current_index_after_move, sequential_next_index, web_token_expires_in,
        web_token_is_fresh, Engine, PlaybackState, PlayerSignal,
    };
    use crate::io::ProtocolWriter;
    use crate::protocol::{RepeatMode, TrackRef};

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
            Engine::new(writer, cache, PathBuf::new()),
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
            auth_state: crate::protocol::AuthState::Ready,
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
    fn moving_queue_items_preserves_the_current_track() {
        assert_eq!(remap_current_index_after_move(2, 2, 0), 0);
        assert_eq!(remap_current_index_after_move(2, 0, 3), 1);
        assert_eq!(remap_current_index_after_move(2, 4, 1), 3);
        assert_eq!(remap_current_index_after_move(2, 0, 1), 2);
        assert_eq!(remap_current_index_after_move(2, 3, 4), 2);
    }

    #[test]
    fn web_token_cache_serves_only_tokens_that_outlive_the_skew() {
        let expires_at = Instant::now() + Duration::from_secs(120);
        assert!(web_token_is_fresh(expires_at, Instant::now()));
        assert!(!web_token_is_fresh(
            expires_at,
            Instant::now() + Duration::from_secs(61)
        ));
    }

    #[test]
    fn web_token_lifetime_applies_skew_and_never_reports_zero() {
        let now = Instant::now();
        assert_eq!(
            web_token_expires_in(now + Duration::from_secs(3600), now),
            3600 - 60
        );
        // A token already inside the skew window (which would trigger a
        // re-mint) still reports a positive lifetime rather than zero.
        assert_eq!(web_token_expires_in(now + Duration::from_secs(30), now), 1);
        assert_eq!(web_token_expires_in(now + Duration::from_secs(61), now), 1);
    }
}
