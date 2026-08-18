use std::sync::Arc;
use std::time::Instant;

use librespot_core::SpotifyUri;
use librespot_core::cache::Cache;
use librespot_playback::mixer::{Mixer, softmixer::SoftMixer};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc;

use crate::auth::{
    PendingAuth, PlaybackHandles, complete_oauth, connect_cached, percent_to_volume,
    prepare_oauth,
};
use crate::browse;
use crate::edits;
use crate::io::ProtocolWriter;
use crate::protocol::{
    AlbumBrowse, ArtistBrowse, AuthState, BrowseResponse, Command, PlaylistBrowse, PlaylistRef,
    RepeatMode, Response, SearchBrowse, StateEvent, TrackRef,
};
use serde::Serialize;
/// Pressing previous within this many milliseconds of a track start restarts
/// the current track instead of switching tracks. Mirrors the UI's optimistic
/// restart window (OnPrevious in app.cpp) so both sides agree.
const PREVIOUS_RESTART_THRESHOLD_MS: u32 = 3_000;

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
    position_anchor: Option<(u32, Instant)>,
    shuffle_pool: Vec<usize>,
    history: Vec<usize>,
    random_state: u64,
    generation: u64,
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
            position_anchor: None,
            shuffle_pool: Vec::new(),
            history: Vec::new(),
            random_state,
            generation: 0,
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
        match command {
            Command::Status
            | Command::Shutdown
            | Command::Login
            | Command::Logout
            | Command::BrowsePlaylists { .. }
            | Command::BrowsePlaylist { .. }
            | Command::BrowseAlbum { .. }
            | Command::BrowseArtist { .. }
            | Command::BrowseSearch { .. }
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

    /// The live session for browse commands: browsing only needs the
    /// authenticated session (unlike playback commands, no player yet).
    fn browse_session(&self) -> Result<&librespot_core::Session, String> {
        self.session.as_ref().ok_or_else(|| match self.state.auth_state {
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

    pub async fn browse_playlists(&mut self, length: usize) -> Result<Vec<PlaylistRef>, String> {
        let session = self.browse_session()?;
        browse::playlists_browse(session, length).await
    }

    pub async fn browse_playlist(&mut self, id: &str) -> Result<PlaylistBrowse, String> {
        let session = self.browse_session()?;
        browse::playlist_browse(session, id).await
    }

    pub async fn browse_album(&mut self, id: &str) -> Result<AlbumBrowse, String> {
        let session = self.browse_session()?;
        browse::album_browse(session, id).await
    }

    pub async fn browse_artist(&mut self, id: &str) -> Result<ArtistBrowse, String> {
        let session = self.browse_session()?;
        browse::artist_browse(session, id).await
    }

    pub async fn browse_search(&mut self, query: &str, limit: usize) -> Result<SearchBrowse, String> {
        let session = self.browse_session()?;
        browse::search_browse(session, query, limit).await
    }

    /// Playlist edits run on the same spclient session as browsing; like
    /// browse commands they need no player.
    pub async fn edit_create_playlist(&mut self, name: &str) -> Result<PlaylistRef, String> {
        let session = self.browse_session()?;
        edits::create_playlist(session, name).await
    }

    pub async fn edit_rename_playlist(&mut self, id: &str, name: &str) -> Result<(), String> {
        let session = self.browse_session()?;
        edits::rename_playlist(session, id, name).await
    }

    pub async fn edit_delete_playlist(&mut self, id: &str) -> Result<(), String> {
        let session = self.browse_session()?;
        edits::delete_playlist(session, id).await
    }

    pub async fn edit_add_playlist_tracks(
        &mut self,
        id: &str,
        uris: &[String],
    ) -> Result<(), String> {
        let session = self.browse_session()?;
        edits::add_tracks(session, id, uris).await
    }

    pub async fn edit_remove_playlist_tracks(
        &mut self,
        id: &str,
        uris: &[String],
    ) -> Result<(), String> {
        let session = self.browse_session()?;
        edits::remove_tracks(session, id, uris).await
    }

    pub async fn edit_reorder_playlist_tracks(
        &mut self,
        id: &str,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        let session = self.browse_session()?;
        edits::reorder_tracks(session, id, from, to).await
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;


    use librespot_core::SpotifyUri;
    use librespot_playback::player::PlayerEvent;
    use super::{
        remap_current_index_after_move, sequential_next_index, AuthSignal, Engine, PlaybackState,
        PlayerSignal,
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
            auth_state: crate::protocol::AuthState::Ready,
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
        assert!(engine.state.auth_state == crate::protocol::AuthState::NeedsLogin);
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
        assert!(engine.state.auth_state == crate::protocol::AuthState::NeedsLogin);
    }

    #[test]
    fn startup_without_cached_credentials_enters_needs_login_without_a_flow() {
        let (mut engine, buffer) = test_engine(); // cache has no credentials
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        engine.start_authentication(sender);
        assert!(!engine.auth_running, "no implicit flow may start");
        assert!(engine.state.auth_state == crate::protocol::AuthState::NeedsLogin);
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
        engine.state.auth_state = crate::protocol::AuthState::Ready;
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.login(&sender).expect("login no-ops when authenticated"));
        assert!(!engine.auth_running);
        assert!(engine.state.auth_state == crate::protocol::AuthState::Ready);
    }

    #[test]
    fn login_is_a_noop_while_a_flow_is_already_running() {
        let mut engine = test_engine().0;
        engine.auth_running = true;
        engine.state.auth_state = crate::protocol::AuthState::Authenticating;
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
        assert!(engine.state.auth_state == crate::protocol::AuthState::Authenticating);
        assert!(engine.auth_running);
        assert_eq!(engine.state.auth_url.as_deref(), Some(published.as_str()));
        assert!(engine.pending_auth.is_none());
    }

    #[test]
    fn begin_login_flow_prepares_a_fresh_attempt_when_none_is_pending() {
        let mut engine = test_engine().0;
        engine.state.auth_state = crate::protocol::AuthState::NeedsLogin;
        let pending = engine.begin_login_flow().expect("flow begins");
        assert!(pending.auth_url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(engine.state.auth_state == crate::protocol::AuthState::Authenticating);
        assert_eq!(engine.state.auth_url.as_deref(), Some(pending.auth_url.as_str()));
    }

    #[test]
    fn failed_auth_signal_returns_to_needs_login_with_a_fresh_url() {
        let mut engine = test_engine().0;
        engine.state.auth_state = crate::protocol::AuthState::Authenticating;
        engine.state.auth_url = Some("https://accounts.spotify.com/authorize?first".to_owned());
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(engine.on_auth_signal(
            AuthSignal::Complete {
                generation: engine.generation,
                result: Err("Spotify authentication failed: test".to_owned()),
            },
            sender,
        ));
        assert!(engine.state.auth_state == crate::protocol::AuthState::NeedsLogin);
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
