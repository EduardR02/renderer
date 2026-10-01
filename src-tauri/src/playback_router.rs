//! One output owner for UI commands and system media keys.
//!
//! While a Spotify device plays, the engine stays paused and remains the only
//! queue model: every edit goes to it. Spotify holds a window of the engine's
//! order as the `uris` of one play request, so the device can play through and
//! use its own Next button. Replacing a window can rebuffer: Spotify provides
//! no atomic queue replacement or ordering guarantee across Player endpoints.
//! The router keeps an occurrence cursor, never a second durable queue.
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::app::AppState;
use crate::engine_client::{EngineClient, StateLine};
use crate::media_keys;
use crate::personal_api::{self, player_path, Device, PersonalApi, PlayerState};
use crate::types::{PlaybackEvent, PlaybackState, Track};

/// Spotify starts at most 100 `uris` from one play request.
const WINDOW: usize = 100;
/// A track change extends the window once fewer entries than this remain.
const LOW_WATER: usize = 10;
/// Spotify can keep reporting the previous item for a few seconds after a
/// command; until then a disagreeing reading is retried, not believed.
const GRACE: Duration = Duration::from_secs(8);
const HEARTBEAT: Duration = Duration::from_secs(60);
/// The last window entry counts as finished this close to its projected end.
const END_SLACK_MS: u32 = 1500;
/// Revisions cross the JavaScript bridge as exactly representable integers.
const MAX_REVISION: u64 = (1 << 53) - 1;
const LOCAL_ONLY: &str = "Playback speed and track-editor previews require This computer; Spotify devices play original audio";
const FOREIGN_QUEUE: &str = "This queue belongs to the Spotify app on that device; edit it there, or play something from here";
const MOVED_AWAY: &str = "Spotify playback moved away from the selected device; reselect a device or This computer";
const RELEASED: &str = "The selected output was released";

pub enum Action {
    Play, Pause, Next, Previous, Seek(u32), Volume(u8), Shuffle(bool), Repeat(String), Speed(f32),
    Queue(Vec<Track>, usize, String, bool), Index(usize), Add(Track, String), AddBatch(Vec<Track>, String), Remove(usize), Move(usize, usize),
    Preview(Track, Vec<renderer_engine::protocol::TimeRange>, Option<renderer_engine::protocol::LoopRange>, u32, u64),
    RestorePreview(u64),
}

/// A queue command for the paused engine. None of these load audio, except
/// that removing the engine's own current row loads its replacement paused.
pub(crate) enum EngineOp<'a> {
    Restore { queue: &'a [Track], index: usize, position_ms: u32, context: &'a str },
    Cursor { index: usize, position_ms: u32 },
    Add(&'a Track, &'a str),
    AddBatch(&'a [Track], &'a str),
    Remove(usize),
    Move(usize, usize),
    Shuffle(bool),
    Repeat(&'a str),
}

/// The engine after an operation, read consistently with its order.
pub(crate) struct EngineView {
    /// The state the operation produced; `None` when it changed nothing.
    state: Option<PlaybackState>,
    /// The engine's order after its own current row.
    upcoming: Vec<usize>,
}

/// Everything the router does outside itself, so the decisions can be driven
/// against a local HTTP mock and an in-memory engine in tests.
pub(crate) trait Host: Sync {
    /// One Spotify Web API request; `None` is an empty answer.
    fn spotify(&self, method: Method, path: String, body: Option<Value>) -> impl Future<Output = Result<Option<Value>, String>> + Send;
    /// Applies `op` to the engine queue, then reads the engine back.
    fn engine(&self, op: Option<EngineOp<'_>>) -> impl Future<Output = Result<EngineView, String>> + Send;
    fn exclusions(&self, context: &str) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn pause_engine(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn local(&self, action: Action) -> impl Future<Output = Result<(), String>> + Send;
    fn local_state(&self) -> PlaybackState;
    /// Shows the engine's own state, rows included.
    fn publish_local(&self);
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S);
    fn media(&self, state: &PlaybackState);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind { Prepared, Owned, Foreign }

enum Mode {
    /// The paused local queue waits for Play before Spotify hears anything.
    Prepared,
    /// Spotify plays a window of the engine's order.
    Owned(Window),
    /// Something else plays there (the phone started it): shown, not managed.
    Foreign,
}

/// The URIs Spotify holds from the router's last play request.
struct Window {
    /// The engine row behind each URI; `None` once that row was removed.
    rows: Vec<Option<usize>>,
    uris: Vec<String>,
    /// The entry Spotify plays.
    at: usize,
    /// A paused edit changed the order; the next Play sends it.
    stale: bool,
}

/// What the router's own last command should make Spotify report.
struct Pending {
    since: Instant,
    uri: Option<String>,
    playing: Option<bool>,
    position_ms: Option<u32>,
    retries: u32,
}

struct Remote {
    /// What the window shows. While the engine's order plays, `queue` is the
    /// engine's rows, `current_index` the device's row and `upcoming` the
    /// order from it.
    state: PlaybackState,
    mode: Mode,
    /// The engine's own current row. It stays put while the device advances.
    anchor: Option<usize>,
    /// The engine's order after `anchor`.
    engine_revision: u64,
    plan: Vec<usize>,
    /// Rows the device has played since `anchor`: shuffle's spent draws.
    consumed: HashSet<usize>,
    supports_volume: bool,
    anchored: Instant,
    published_queue: Option<u64>,
    published_order: Option<u64>,
    polling: bool,
    pending: Option<Pending>,
    paused_checks: u32,
    failures: u32,
    repeat_dirty: bool,
    pause_dirty: bool,
}

#[derive(Clone, Copy)]
enum Remap { Keep, Remove(usize), Move(usize, usize) }

impl Remap {
    /// Where engine row `row` sits after the edit; `None` once it is gone.
    fn apply(self, row: usize) -> Option<usize> {
        match self {
            Remap::Keep => Some(row),
            Remap::Remove(removed) => match row.cmp(&removed) {
                std::cmp::Ordering::Less => Some(row),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(row - 1),
            },
            Remap::Move(from, to) => Some(if row == from { to }
                else if from < row && row <= to { row - 1 }
                else if to <= row && row < from { row + 1 }
                else { row }),
        }
    }
}

impl Remote {
    fn new(state: PlaybackState, supports_volume: bool) -> Self {
        Self {
            anchor: state.current_index,
            engine_revision: state.queue_revision,
            plan: state.upcoming.clone(),
            state,
            mode: Mode::Prepared,
            consumed: HashSet::new(),
            supports_volume,
            anchored: Instant::now(),
            published_queue: None,
            published_order: None,
            polling: false,
            pending: None,
            paused_checks: 0,
            failures: 0,
            repeat_dirty: false,
            pause_dirty: false,
        }
    }

    fn kind(&self) -> Kind {
        match self.mode { Mode::Prepared => Kind::Prepared, Mode::Owned(_) => Kind::Owned, Mode::Foreign => Kind::Foreign }
    }

    fn position(&self) -> u32 { projected_position(&self.state, self.anchored.elapsed()) }

    fn rebase(&mut self) {
        self.state.position_ms = self.position();
        self.anchored = Instant::now();
    }

    /// Spotify plays the window in order; only repeat-one is its own job.
    fn spotify_repeat(&self) -> &'static str { if self.state.repeat == "track" { "track" } else { "off" } }

    fn device_path(&self, endpoint: &str, query: &[(&str, String)]) -> Result<String, String> {
        player_path(&self.state.output_device_id, endpoint, query)
    }

    fn expect(&mut self, uri: Option<String>, playing: Option<bool>) {
        self.pending = Some(Pending { since: Instant::now(), uri, playing, position_ms: None, retries: 0 });
        self.polling = true;
        self.paused_checks = 0;
    }

    /// Points the view at row `row` of the rows it shows.
    fn show_row(&mut self, row: Option<usize>) {
        self.state.current_index = row.filter(|row| *row < self.state.queue.len());
        match self.state.current_index.map(|row| &self.state.queue[row]) {
            Some(track) => {
                self.state.current_uri.clone_from(&track.uri);
                self.state.context.clone_from(&track.context);
                self.state.duration_ms = track.duration_ms;
            }
            None => {
                self.state.current_uri.clear();
                self.state.context.clear();
                self.state.duration_ms = 0;
            }
        }
    }

    fn adopt_engine(&mut self, view: EngineView) {
        if let Some(engine) = view.state {
            // An unchanged revision names rows already held (and an engine
            if engine.queue_revision == 0 || engine.queue_revision != self.engine_revision || self.kind() == Kind::Foreign {
                self.state.queue = engine.queue;
                self.state.queue_revision = next_revision(self.state.queue_revision);
            }
            self.engine_revision = engine.queue_revision;
            self.anchor = engine.current_index;
            self.state.shuffle = engine.shuffle;
            self.state.repeat = engine.repeat;
        }
        self.plan = view.upcoming;
    }

    fn set_order(&mut self, order: Vec<usize>) {
        if self.state.upcoming != order {
            self.state.upcoming = order;
            self.state.order_revision = next_revision(self.state.order_revision);
        }
    }

    fn order_from(&self, current: Option<usize>, cut: usize) -> Option<Vec<usize>> {
        order_from(self.anchor, &self.plan, self.state.shuffle, &self.state.repeat, current, cut, &self.consumed)
    }

    /// Shuffle with repeat has drawn every row of this lap: a new lap is a new
    /// draw, which only the engine makes, and only from a fresh anchor.
    fn lap_spent(&self, current: Option<usize>) -> bool {
        self.state.shuffle && self.state.repeat == "context" && (self.anchor != current || !self.consumed.is_empty())
    }

    fn adopt_transport(&mut self, player: &PlayerState) {
        self.state.playing = player.is_playing;
        self.state.position_ms = player.progress_ms.unwrap_or(0);
        self.state.volume = player.device.volume_percent.unwrap_or(self.state.volume);
        self.state.output_device_name.clone_from(&player.device.name);
        self.state.error.clear();
        self.supports_volume = player.device.supports_volume;
        self.anchored = Instant::now();
        self.paused_checks = if player.is_playing { 0 } else { self.paused_checks.saturating_add(1) };
    }

    /// Shows the device's item followed by Spotify's own queue.
    fn show_foreign(&mut self, player: Option<&PlayerState>, queue: &[Value]) {
        let current = player.and_then(|player| player.item.as_ref()).and_then(|item| track_from_player(item).ok());
        let offset = usize::from(current.is_some());
        let mut rows = Vec::with_capacity(queue.len() + offset);
        rows.extend(current);
        rows.extend(queue.iter().filter_map(|item| track_from_player(item).ok()));
        if self.state.queue != rows {
            self.state.queue = rows;
            self.state.queue_revision = next_revision(self.state.queue_revision);
        }
        self.show_row((offset == 1).then_some(0));
        self.state.context.clear();
        self.set_order((offset..self.state.queue.len()).collect());
        if let Some(duration) = player.and_then(|player| player.item.as_ref()).and_then(|item| item["duration_ms"].as_u64()) {
            self.state.duration_ms = duration.min(u64::from(u32::MAX)) as u32;
        }
    }
}

impl Pending {
    fn met_by(&self, player: Option<&PlayerState>, repeat: Option<&str>) -> bool {
        let Some(player) = player else { return false; };
        self.uri.as_deref().is_none_or(|uri| item_uri(player) == uri)
            && self.playing.is_none_or(|playing| player.is_playing == playing)
            && self.position_ms.is_none_or(|position| player.progress_ms.is_some_and(|progress| {
                let elapsed = if player.is_playing { self.since.elapsed().as_millis().min(u32::MAX as u128) as u32 } else { 0 };
                progress.abs_diff(position.saturating_add(elapsed)) <= 3000
            }))
            && repeat.is_none_or(|repeat| !player.shuffle_state && player.repeat_state == repeat)
    }
}

pub struct PlaybackRouter {
    client: Arc<EngineClient>,
    personal: Arc<PersonalApi>,
    core: Core,
}

impl PlaybackRouter {
    pub fn new(client: Arc<EngineClient>, personal: Arc<PersonalApi>) -> Arc<Self> {
        Arc::new(Self { client, personal, core: Core::default() })
    }

    fn host<'a>(&'a self, app: &'a AppHandle) -> AppHost<'a> {
        AppHost { app, client: &self.client, personal: &self.personal }
    }

    pub fn is_remote(&self) -> bool { self.core.remote.lock().is_some() }

    pub fn snapshot(&self) -> Option<PlaybackState> { self.core.snapshot() }

    pub fn report_error(&self, app: &AppHandle, error: &str) { self.core.report_error(&self.host(app), error); }

    pub fn suspend(&self, app: &AppHandle, error: &str) { self.core.suspend(&self.host(app), error); }

    pub async fn select(&self, app: &AppHandle, device_id: Option<String>) -> Result<(), String> {
        self.core.select(&self.host(app), device_id).await
    }

    /// Disconnect never resumes local audio or leaves a dormant remote timer.
    pub async fn disconnect(&self, app: &AppHandle) { self.core.disconnect(&self.host(app)).await; }

    pub async fn run(&self, app: &AppHandle, action: Action) -> Result<(), String> {
        self.core.run(&self.host(app), action).await
    }

    pub async fn watch(self: Arc<Self>, app: AppHandle) {
        let host = self.host(&app);
        self.core.watch(&host).await;
    }
}

#[derive(Default)]
struct Core {
    operation: AsyncMutex<()>,
    remote: Mutex<Option<Remote>>,
    changed: Notify,
}

impl Core {
    fn snapshot(&self) -> Option<PlaybackState> {
        self.remote.lock().as_ref().map(|remote| {
            let mut state = remote.state.clone();
            state.position_ms = remote.position();
            state
        })
    }

    fn publish<H: Host>(&self, host: &H) {
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return; };
        remote.rebase();
        let include_queue = remote.published_queue != Some(remote.state.queue_revision);
        let include_order = remote.published_order != Some(remote.state.order_revision);
        host.emit("state", PlaybackEvent::new(&remote.state, include_queue, include_order));
        remote.published_queue = Some(remote.state.queue_revision);
        remote.published_order = Some(remote.state.order_revision);
        host.media(&remote.state);
    }

    /// Transport-only changes must not serialize a long queue or its order.
    fn publish_transport<H: Host>(&self, host: &H) {
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return; };
        remote.rebase();
        let state = &remote.state;
        host.emit("state", json!({ "playing": state.playing, "position_ms": state.position_ms, "volume": state.volume,
            "shuffle": state.shuffle, "repeat": state.repeat, "error": state.error }));
        host.media(state);
    }

    fn report_error<H: Host>(&self, host: &H, error: &str) {
        if let Some(remote) = self.remote.lock().as_mut() { remote.state.error = error.to_owned(); }
        host.emit("playback-action-error", error);
        self.publish_transport(host);
        self.changed.notify_one();
    }

    fn suspend<H: Host>(&self, host: &H, error: &str) {
        if let Some(remote) = self.remote.lock().as_mut() { remote.polling = false; }
        self.report_error(host, error);
    }

    async fn command<H: Host>(&self, host: &H, method: Method, endpoint: &str, query: &[(&str, String)], body: Option<Value>) -> Result<(), String> {
        let path = self.remote.lock().as_ref().ok_or(RELEASED)?.device_path(endpoint, query)?;
        host.spotify(method, path, body).await.map(drop)
    }

    async fn select<H: Host>(&self, host: &H, device_id: Option<String>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let result = self.select_locked(host, device_id).await;
        if let Err(error) = &result { self.report_error(host, error); }
        self.changed.notify_one();
        result
    }

    async fn select_locked<H: Host>(&self, host: &H, device_id: Option<String>) -> Result<(), String> {
        let Some(device_id) = device_id.filter(|id| !id.is_empty()) else {
            return self.return_local(host).await;
        };
        player_path(&device_id, "play", &[])?;
        let selected = self.remote.lock().as_ref().map(|remote| (remote.state.output_device_id == device_id, remote.kind(), remote.state.playing));
        if let Some((true, kind, _)) = selected {
            // Reselecting a staged queue must not adopt an unrelated session there.
            return if kind == Kind::Prepared { Ok(()) } else { self.sync(host).await };
        }
        let devices = host.spotify(Method::GET, "me/player/devices".into(), None).await?.unwrap_or_default();
        let devices: Vec<Device> = serde_json::from_value(devices["devices"].clone())
            .map_err(|_| "Spotify returned an invalid devices response".to_owned())?;
        let device = devices.into_iter().find(|device| device.id.as_deref() == Some(&device_id))
            .ok_or("Spotify device is unavailable; open Spotify there and refresh devices")?;
        if device.is_restricted { return Err("Spotify does not allow controlling this device".into()); }
        if let Some((_, kind, playing)) = selected {
            // Spotify moves its whole session — window, position and all.
            let play = playing && kind != Kind::Prepared;
            host.spotify(Method::PUT, "me/player".into(), Some(json!({ "device_ids": [device_id], "play": play }))).await?;
            {
                let mut guard = self.remote.lock();
                let remote = guard.as_mut().ok_or(RELEASED)?;
                remote.state.output_device_id = device_id;
                remote.state.output_device_name = device.name;
                remote.supports_volume = device.supports_volume;
                if kind != Kind::Prepared { remote.expect(None, Some(play)); }
            }
            self.publish(host);
            return Ok(());
        }
        let mut state = host.local_state();
        if state.preview { return Err("Return to This computer and finish the track preview before selecting another output".into()); }
        // Await the actual engine pause before any remote transfer or start.
        host.pause_engine().await?;
        host.spotify(Method::PUT, "me/player".into(), Some(json!({ "device_ids": [device_id], "play": false }))).await?;
        let view = host.engine(None).await?;
        let was_playing = state.playing;
        state.output_device_id = device_id;
        state.output_device_name = device.name;
        state.buffering = false;
        state.playing = false;
        state.playback_speed = 1.0;
        state.audible_playback_speed = 1.0;
        state.error.clear();
        let position_ms = state.position_ms;
        let mut remote = Remote::new(state, device.supports_volume);
        remote.adopt_engine(view);
        remote.show_row(remote.anchor);
        remote.set_order(remote.plan.clone());
        let current = remote.anchor;
        if current.is_none() {
            // Nothing local to hand over: show what the device plays.
            remote.mode = Mode::Foreign;
            remote.polling = true;
        }
        *self.remote.lock() = Some(remote);
        self.publish(host);
        match current {
            Some(current) if was_playing => self.start(host, current, position_ms).await,
            Some(_) => Ok(()),
            None => self.sync(host).await,
        }
    }

    async fn return_local<H: Host>(&self, host: &H) -> Result<(), String> {
        let Some(kind) = self.remote.lock().as_ref().map(Remote::kind) else { return Ok(()); };
        // Returning is deliberately paused, never an automatic local fallback.
        let needs_pause = kind != Kind::Prepared || self.remote.lock().as_ref().is_some_and(|remote| remote.pause_dirty);
        let stopped = if needs_pause { self.command(host, Method::PUT, "pause", &[], None).await } else { Ok(()) };
        let Some(remote) = self.remote.lock().take() else { return Ok(()); };
        self.changed.notify_one();
        // Resume here where the device was only if it played the engine's
        // order. Anything the phone started leaves the local queue alone.
        let resume = match (&remote.mode, remote.state.current_index) {
            (Mode::Owned(_), Some(row)) => Some((row, remote.position())),
            _ => None,
        };
        let restored = match resume {
            Some((index, position_ms)) => host.engine(Some(EngineOp::Cursor { index, position_ms })).await.map(drop),
            None => Ok(()),
        };
        // A restore publishes the engine's own state through the normal path.
        if resume.is_none() || restored.is_err() { host.publish_local(); }
        restored?;
        stopped.map_err(|error| format!("Could not pause the previous output: {error}. Stop it in Spotify before resuming here."))
    }

    async fn disconnect<H: Host>(&self, host: &H) {
        let _operation = self.operation.lock().await;
        if let Err(error) = self.return_local(host).await { self.report_error(host, &error); }
    }

    async fn run<H: Host>(&self, host: &H, action: Action) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let kind = self.remote.lock().as_ref().map(Remote::kind);
        let result = match kind {
            None => host.local(action).await,
            Some(kind) => self.run_remote(host, kind, action).await,
        };
        if let Err(error) = &result { self.report_error(host, error); }
        else if kind.is_some() {
            let recovered = self.remote.lock().as_mut().is_some_and(|remote| {
                if remote.state.error.is_empty() { return false; }
                remote.state.error.clear();
                true
            });
            if recovered { self.publish_transport(host); }
        }
        if kind.is_some() { self.changed.notify_one(); }
        result
    }

    async fn run_remote<H: Host>(&self, host: &H, kind: Kind, action: Action) -> Result<(), String> {
        let foreign = kind == Kind::Foreign;
        match action {
            Action::Speed(_) | Action::Preview(..) | Action::RestorePreview(_) => Err(LOCAL_ONLY.into()),
            Action::Volume(percent) => self.volume(host, percent).await,
            Action::Seek(position_ms) => self.seek(host, kind, position_ms).await,
            Action::Pause => self.pause(host, kind).await,
            Action::Play => self.play(host, kind).await,
            Action::Next => self.next(host, kind).await,
            Action::Previous => self.previous(host, kind).await,
            Action::Shuffle(enabled) if foreign => self.foreign_setting(host, "shuffle", enabled.to_string()).await,
            Action::Shuffle(enabled) => self.edit(host, kind, EngineOp::Shuffle(enabled)).await,
            Action::Repeat(mode) => {
                if !matches!(mode.as_str(), "off" | "context" | "track") { return Err("invalid repeat mode".into()); }
                if foreign { self.foreign_setting(host, "repeat", mode).await } else { self.edit(host, kind, EngineOp::Repeat(&mode)).await }
            }
            Action::Queue(queue, index, context, automatic) => self.play_queue(host, queue, index, context, automatic).await,
            Action::Index(index) if foreign => self.play_foreign_row(host, index).await,
            Action::Index(index) => self.jump(host, index).await,
            Action::Add(track, _) if foreign => self.queue_on_spotify(host, std::slice::from_ref(&track)).await,
            Action::AddBatch(tracks, _) if foreign => self.queue_on_spotify(host, &tracks).await,
            Action::Add(track, context) => self.edit(host, kind, EngineOp::Add(&track, &context)).await,
            Action::AddBatch(tracks, _) if tracks.is_empty() => Ok(()),
            Action::AddBatch(tracks, context) => self.edit(host, kind, EngineOp::AddBatch(&tracks, &context)).await,
            Action::Remove(_) | Action::Move(..) if foreign => Err(FOREIGN_QUEUE.into()),
            Action::Remove(index) => self.edit(host, kind, EngineOp::Remove(index)).await,
            Action::Move(from, to) => self.edit(host, kind, EngineOp::Move(from, to)).await,
        }
    }

    async fn volume<H: Host>(&self, host: &H, percent: u8) -> Result<(), String> {
        if percent > 100 { return Err("volume percent must be between 0 and 100".into()); }
        if !self.remote.lock().as_ref().is_some_and(|remote| remote.supports_volume) {
            return Err("This Spotify device does not support remote volume control".into());
        }
        self.command(host, Method::PUT, "volume", &[("volume_percent", percent.to_string())], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() { remote.state.volume = percent; }
        self.publish_transport(host);
        Ok(())
    }

    async fn seek<H: Host>(&self, host: &H, kind: Kind, position_ms: u32) -> Result<(), String> {
        let duration = self.remote.lock().as_ref().map_or(0, |remote| remote.state.duration_ms);
        let position_ms = if duration == 0 { position_ms } else { position_ms.min(duration) };
        // A staged queue starts from the sought position when Play sends it.
        if kind != Kind::Prepared {
            self.command(host, Method::PUT, "seek", &[("position_ms", position_ms.to_string())], None).await?;
        }
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.state.position_ms = position_ms;
            remote.anchored = Instant::now();
            if kind != Kind::Prepared {
                remote.expect(Some(remote.state.current_uri.clone()), None);
                if let Some(pending) = remote.pending.as_mut() { pending.position_ms = Some(position_ms); }
            }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn pause<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        if kind != Kind::Prepared { self.command(host, Method::PUT, "pause", &[], None).await?; }
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.rebase();
            remote.state.playing = false;
            if kind != Kind::Prepared { remote.expect(None, Some(false)); }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn play<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        let (current, position_ms, stale) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let stale = matches!(&remote.mode, Mode::Owned(window) if window.stale);
            (remote.state.current_index, remote.state.position_ms, stale)
        };
        if kind == Kind::Prepared || stale {
            return self.start(host, current.ok_or("No track is selected")?, position_ms).await;
        }
        self.command(host, Method::PUT, "play", &[], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.rebase();
            remote.state.playing = true;
            remote.expect(None, Some(true));
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn next<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        if kind == Kind::Foreign {
            self.command(host, Method::POST, "next", &[], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() {
                // The phone's queue is shown in Spotify's order: its next row is due.
                let next = remote.state.current_index.and_then(|row| remote.state.queue.get(row + 1)).map(|track| track.uri.clone());
                remote.expect(next, None);
            }
            return Ok(());
        }
        let (current, step) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let step = match &remote.mode {
                Mode::Owned(window) if !window.stale => window.rows.get(window.at + 1).copied().flatten()
                    .map(|row| (row, window.uris[window.at + 1].clone())),
                _ => None,
            };
            (remote.state.current_index, step)
        };
        let Some(current) = current else { return Ok(()); };
        let Some((row, uri)) = step else {
            // Nothing staged next on the device: send the order from the next row.
            let order = self.order(host, Some(current), current).await?;
            return match order.first() {
                Some(&next) => self.start(host, next, 0).await,
                None => self.finish(host).await,
            };
        };
        self.command(host, Method::POST, "next", &[], None).await?;
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let duplicate = remote.state.current_uri == uri;
            if let Mode::Owned(window) = &mut remote.mode { window.at += 1; }
            remote.consumed.insert(row);
            remote.show_row(Some(row));
            remote.state.position_ms = 0;
            remote.state.playing = true;
            remote.anchored = Instant::now();
            // Devices differ on whether a skip while paused resumes; the
            // reading settles that.
            remote.expect(Some(uri), None);
            if duplicate {
                if let Some(pending) = remote.pending.as_mut() { pending.position_ms = Some(0); }
            }
        }
        self.show_order(host, row).await
    }

    async fn previous<H: Host>(&self, host: &H, kind: Kind) -> Result<(), String> {
        if kind == Kind::Foreign {
            self.command(host, Method::POST, "previous", &[], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() { remote.expect(None, None); }
            return Ok(());
        }
        let (current, back) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let back = match &remote.mode {
                Mode::Owned(window) if !window.stale => Some((remote.position() < 3000 && window.at > 0)
                    .then(|| window.rows[window.at - 1].map(|row| (row, window.uris[window.at - 1].clone()))).flatten()),
                _ => None,
            };
            (remote.state.current_index, back)
        };
        let current = current.ok_or("No track is selected")?;
        // Nothing earlier on the device (or nothing sent yet): restart this row.
        let Some(back) = back else { return self.start(host, current, 0).await; };
        self.command(host, Method::POST, "previous", &[], None).await?;
        let row = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let row = match back {
                Some((row, uri)) => {
                    // Spotify's Previous steps back once this item is under 3 s in.
                    if let Mode::Owned(window) = &mut remote.mode { window.at -= 1; }
                    remote.consumed.remove(&current);
                    remote.show_row(Some(row));
                    remote.expect(Some(uri), None);
                    row
                }
                None => {
                    let uri = remote.state.current_uri.clone();
                    remote.expect(Some(uri), None);
                    current
                }
            };
            remote.state.position_ms = 0;
            remote.state.playing = true;
            remote.anchored = Instant::now();
            row
        };
        self.show_order(host, row).await
    }

    /// Publishes the order from the device's row after Spotify moved there
    /// on its own; the window itself is checked by the next reconciliation.
    async fn show_order<H: Host>(&self, host: &H, row: usize) -> Result<(), String> {
        self.reanchor(host, row, self.snapshot().map_or(0, |state| state.position_ms)).await?;
        let order = self.order(host, Some(row), row).await?;
        if let Some(remote) = self.remote.lock().as_mut() { remote.set_order(order); }
        self.publish(host);
        Ok(())
    }

    /// The order ran out: stop, and let the next Play start the last row again.
    async fn finish<H: Host>(&self, host: &H) -> Result<(), String> {
        let playing = self.remote.lock().as_ref().is_some_and(|remote| remote.state.playing);
        if playing { self.command(host, Method::PUT, "pause", &[], None).await?; }
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.state.playing = false;
            remote.state.position_ms = 0;
            remote.anchored = Instant::now();
            remote.set_order(Vec::new());
            if let Mode::Owned(window) = &mut remote.mode { window.stale = true; }
            if playing { remote.expect(None, Some(false)); }
        }
        self.publish(host);
        Ok(())
    }

    async fn jump<H: Host>(&self, host: &H, index: usize) -> Result<(), String> {
        {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            validate_remote_track(remote.state.queue.get(index).ok_or("queue index is out of range")?)?;
        }
        self.start(host, index, 0).await
    }

    async fn play_queue<H: Host>(&self, host: &H, queue: Vec<Track>, index: usize, context: String, automatic: bool) -> Result<(), String> {
        if queue.is_empty() {
            let view = host.engine(Some(EngineOp::Restore { queue: &queue, index: 0, position_ms: 0, context: &context })).await?;
            {
                let mut guard = self.remote.lock();
                let remote = guard.as_mut().ok_or(RELEASED)?;
                let playing = remote.state.playing;
                remote.adopt_engine(view);
                remote.show_row(None);
                remote.set_order(Vec::new());
                remote.state.position_ms = 0;
                remote.state.playing = false;
                remote.mode = Mode::Prepared;
                remote.pending = None;
                remote.pause_dirty = playing;
                remote.polling = playing;
            }
            self.publish(host);
            self.apply_pause(host).await?;
            return Ok(());
        }
        if index >= queue.len() { return Err("queue index is out of range".into()); }
        let index = if automatic {
            let excluded = host.exclusions(&context).await?;
            (index..queue.len()).chain(0..index)
                .find(|&row| remote_playable(&queue[row]) && !excluded.contains(&queue[row].id))
                .ok_or("No eligible tracks remain for automatic playback")?
        } else { index };
        validate_remote_track(&queue[index])?;
        let view = host.engine(Some(EngineOp::Restore { queue: &queue, index, position_ms: 0, context: &context })).await?;
        let current = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.adopt_engine(view);
            remote.consumed.clear();
            remote.anchor
        };
        self.publish(host);
        match current {
            Some(current) => self.start(host, current, 0).await,
            None => self.finish(host).await,
        }
    }

    /// Commit the device's occurrence to the canonical engine queue. The engine
    /// consumes upcoming draws/history without replacing any rows.
    async fn reanchor<H: Host>(&self, host: &H, index: usize, position_ms: u32) -> Result<(), String> {
        let view = host.engine(Some(EngineOp::Cursor { index, position_ms })).await?;
        let mut guard = self.remote.lock();
        let remote = guard.as_mut().ok_or(RELEASED)?;
        remote.adopt_engine(view);
        remote.consumed.clear();
        Ok(())
    }

    /// The engine's order from the device's row (or, after that row was
    /// removed, from where it was). Re-anchors when the engine's own order
    /// cannot answer.
    async fn order<H: Host>(&self, host: &H, current: Option<usize>, cut: usize) -> Result<Vec<usize>, String> {
        let position_ms = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            match remote.order_from(current, cut) {
                Some(order) if !(order.is_empty() && remote.lap_spent(current)) => return Ok(order),
                _ => if current.is_some() { remote.position() } else { 0 },
            }
        };
        self.reanchor(host, current.unwrap_or(cut), position_ms).await?;
        let guard = self.remote.lock();
        Ok(guard.as_ref().ok_or(RELEASED)?.order_from(current, cut).unwrap_or_default())
    }

    /// Starts the engine's order on the device at `current`. Spotify's own
    /// shuffle and repeat go out of the way first unless the router already
    /// owns the device.
    async fn start<H: Host>(&self, host: &H, current: usize, position_ms: u32) -> Result<(), String> {
        if self.remote.lock().as_ref().is_some_and(|remote| remote.anchor != Some(current)) {
            self.reanchor(host, current, position_ms).await?;
        }
        let order = self.order(host, Some(current), current).await?;
        let (configure, tail) = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.consumed.insert(current);
            let configure = (remote.kind() != Kind::Owned).then(|| (
                remote.device_path("shuffle", &[("state", "false".into())]),
                remote.device_path("repeat", &[("state", remote.spotify_repeat().into())]),
            ));
            (configure, window_tail(&remote.state.queue, &order))
        };
        if let Some((shuffle, repeat)) = configure {
            host.spotify(Method::PUT, shuffle?, None).await?;
            host.spotify(Method::PUT, repeat?, None).await?;
        }
        self.send_window(host, current, tail, position_ms).await?;
        if let Some(remote) = self.remote.lock().as_mut() { remote.set_order(order); }
        self.publish(host);
        Ok(())
    }

    /// Hands Spotify the device's row followed by `tail`, resuming at
    /// `position_ms`: a brief rebuffer, never a restart from 0.
    async fn send_window<H: Host>(&self, host: &H, current: usize, tail: Vec<usize>, position_ms: u32) -> Result<(), String> {
        let (path, uris) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            validate_remote_track(remote.state.queue.get(current).ok_or("queue index is out of range")?)?;
            let uris: Vec<String> = std::iter::once(current).chain(tail.iter().copied())
                .map(|row| remote.state.queue[row].uri.clone()).collect();
            (remote.device_path("play", &[])?, uris)
        };
        host.spotify(Method::PUT, path, Some(json!({ "uris": uris, "offset": { "position": 0 }, "position_ms": position_ms }))).await?;
        let mut guard = self.remote.lock();
        let remote = guard.as_mut().ok_or(RELEASED)?;
        let uri = uris[0].clone();
        remote.mode = Mode::Owned(Window {
            rows: std::iter::once(Some(current)).chain(tail.into_iter().map(Some)).collect(),
            uris,
            at: 0,
            stale: false,
        });
        remote.show_row(Some(current));
        remote.state.playing = true;
        remote.state.position_ms = position_ms;
        remote.state.error.clear();
        remote.pause_dirty = false;
        remote.anchored = Instant::now();
        remote.expect(Some(uri), Some(true));
        Ok(())
    }

    /// Applies a queue edit to the engine and carries the device through it:
    /// Spotify hears a new window only when what it holds no longer matches.
    async fn edit<H: Host>(&self, host: &H, kind: Kind, op: EngineOp<'_>) -> Result<(), String> {
        let remap = match op { EngineOp::Remove(row) => Remap::Remove(row), EngineOp::Move(from, to) => Remap::Move(from, to), _ => Remap::Keep };
        // Toggling shuffle replaces the bag of draws; repeat keeps it.
        let new_draw = matches!(op, EngineOp::Shuffle(_));
        let shuffling_on = matches!(op, EngineOp::Shuffle(true));
        let (before, repeat_before) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            (remote.state.current_index, remote.spotify_repeat())
        };
        let view = host.engine(Some(op)).await?;
        let (current, removed, anchor, position_ms, repeat_after) = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.adopt_engine(view);
            if kind == Kind::Prepared {
                // Nothing plays there yet: the view is the engine's own. Its
                // replacement for a removed current row starts from 0.
                if matches!(remap, Remap::Remove(row) if before == Some(row)) { remote.state.position_ms = 0; }
                remote.show_row(remote.anchor);
                remote.set_order(remote.plan.clone());
                drop(guard);
                self.publish(host);
                return Ok(());
            }
            let current = before.and_then(|row| remap.apply(row));
            remote.consumed = if new_draw { HashSet::new() } else { remote.consumed.iter().filter_map(|row| remap.apply(*row)).collect() };
            if let Mode::Owned(window) = &mut remote.mode {
                for row in &mut window.rows { *row = row.and_then(|row| remap.apply(row)); }
            }
            remote.show_row(current);
            let removed = match remap { Remap::Remove(row) if current.is_none() && before.is_some() => Some(row), _ => None };
            remote.repeat_dirty |= remote.spotify_repeat() != repeat_before;
            (current, removed, remote.anchor, remote.position(), remote.spotify_repeat())
        };
        self.publish(host);
        if shuffling_on && current.is_some() && anchor != current {
            // A new shuffle draw has to leave out the row the device plays.
            self.reanchor(host, current.unwrap_or_default(), position_ms).await?;
        }
        if repeat_after != repeat_before { self.apply_repeat(host).await?; }
        if let Some(cut) = removed {
            // The device's row is gone: continue where the engine order resumes.
            let order = self.order(host, None, cut).await?;
            let playing = self.remote.lock().as_ref().is_some_and(|remote| remote.state.playing);
            return match order.first().copied() {
                Some(next) if playing => self.start(host, next, 0).await,
                Some(next) => {
                    if let Some(remote) = self.remote.lock().as_mut() {
                        remote.consumed.insert(next);
                        remote.show_row(Some(next));
                        remote.state.position_ms = 0;
                        remote.set_order(order[1..].to_vec());
                        if let Mode::Owned(window) = &mut remote.mode { window.stale = true; }
                    }
                    self.publish(host);
                    Ok(())
                }
                None => self.finish(host).await,
            };
        }
        let Some(current) = current else {
            self.publish(host);
            return Ok(());
        };
        let order = self.order(host, Some(current), current).await?;
        self.settle(host, current, order).await
    }

    /// Publish canonical edits even if the Spotify write fails. A stale window
    /// keeps ownership and retries on reconciliation; paused edits wait for Play.
    async fn settle<H: Host>(&self, host: &H, current: usize, order: Vec<usize>) -> Result<(), String> {
        let send = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let tail = window_tail(&remote.state.queue, &order);
            let playing = remote.state.playing;
            let position_ms = remote.position();
            remote.set_order(order);
            match &mut remote.mode {
                Mode::Owned(window) if window_differs(window, current, &tail) => {
                    window.stale = true;
                    if playing { Some((tail, position_ms)) } else { None }
                }
                _ => None,
            }
        };
        self.publish(host);
        if let Some((tail, position_ms)) = send {
            self.send_window(host, current, tail, position_ms).await?;
            self.publish(host);
        }
        Ok(())
    }

    async fn foreign_setting<H: Host>(&self, host: &H, endpoint: &str, value: String) -> Result<(), String> {
        self.command(host, Method::PUT, endpoint, &[("state", value.clone())], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            if endpoint == "shuffle" { remote.state.shuffle = value == "true"; } else { remote.state.repeat = value; }
        }
        self.publish_transport(host);
        Ok(())
    }

    async fn play_foreign_row<H: Host>(&self, host: &H, index: usize) -> Result<(), String> {
        let (path, uris) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().ok_or(RELEASED)?;
            let rows = &remote.state.queue;
            validate_remote_track(rows.get(index).ok_or("queue index is out of range")?)?;
            let uris: Vec<&str> = rows[index..].iter().filter(|track| remote_playable(track)).take(WINDOW).map(|track| track.uri.as_str()).collect();
            (remote.device_path("play", &[])?, json!({ "uris": uris, "offset": { "position": 0 } }))
        };
        host.spotify(Method::PUT, path, Some(uris)).await?;
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            remote.show_row(Some(index));
            remote.set_order((index + 1..remote.state.queue.len()).collect());
            remote.state.playing = true;
            remote.state.position_ms = 0;
            remote.anchored = Instant::now();
            let uri = remote.state.current_uri.clone();
            remote.expect(Some(uri), Some(true));
        }
        self.publish(host);
        Ok(())
    }

    /// Spotify plays its own queue before the rest of whatever it plays.
    async fn queue_on_spotify<H: Host>(&self, host: &H, tracks: &[Track]) -> Result<(), String> {
        for track in tracks { validate_remote_track(track)?; }
        for track in tracks {
            self.command(host, Method::POST, "queue", &[("uri", track.uri.clone())], None).await?;
        }
        let queue = self.spotify_queue(host).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            let current = remote.state.current_index.and_then(|row| remote.state.queue.get(row)).cloned();
            let offset = usize::from(current.is_some());
            remote.state.queue = current.into_iter().chain(queue.iter().filter_map(|item| track_from_player(item).ok())).collect();
            remote.state.queue_revision = next_revision(remote.state.queue_revision);
            remote.show_row((offset == 1).then_some(0));
            remote.set_order((offset..remote.state.queue.len()).collect());
        }
        self.publish(host);
        Ok(())
    }

    async fn spotify_queue<H: Host>(&self, host: &H) -> Result<Vec<Value>, String> {
        match host.spotify(Method::GET, "me/player/queue".into(), None).await?.as_mut()
            .and_then(Value::as_object_mut).and_then(|answer| answer.remove("queue")) {
            Some(Value::Array(queue)) => Ok(queue),
            _ => Err("Spotify returned no playback queue".into()),
        }
    }

    async fn apply_repeat<H: Host>(&self, host: &H) -> Result<(), String> {
        let repeat = self.remote.lock().as_ref().filter(|remote| remote.repeat_dirty).map(Remote::spotify_repeat);
        if let Some(repeat) = repeat {
            self.command(host, Method::PUT, "repeat", &[("state", repeat.into())], None).await?;
            if let Some(remote) = self.remote.lock().as_mut() {
                remote.repeat_dirty = false;
                remote.expect(None, None);
            }
        }
        Ok(())
    }

    async fn apply_pause<H: Host>(&self, host: &H) -> Result<(), String> {
        if !self.remote.lock().as_ref().is_some_and(|remote| remote.pause_dirty) { return Ok(()); }
        self.command(host, Method::PUT, "pause", &[], None).await?;
        if let Some(remote) = self.remote.lock().as_mut() {
            remote.pause_dirty = false;
            remote.polling = false;
            remote.failures = 0;
            remote.state.error.clear();
        }
        self.publish_transport(host);
        Ok(())
    }

    /// One reconciliation: reads the device and acts on what it reports.
    async fn sync<H: Host>(&self, host: &H) -> Result<(), String> {
        let Some(device) = self.remote.lock().as_ref().map(|remote| remote.state.output_device_id.clone()) else { return Ok(()); };
        self.apply_pause(host).await?;
        if self.remote.lock().as_ref().is_some_and(|remote| remote.kind() == Kind::Prepared) { return Ok(()); }
        self.apply_repeat(host).await?;
        let player = host.spotify(Method::GET, "me/player?additional_types=track,episode".into(), None).await?
            .map(serde_json::from_value::<PlayerState>).transpose()
            .map_err(|_| "Spotify returned an invalid playback state".to_owned())?;
        if player.as_ref().is_some_and(|player| player.device.id.as_deref() != Some(device.as_str())) {
            return Err(MOVED_AWAY.into());
        }
        if player.as_ref().is_some_and(|player| player.device.is_restricted) {
            return Err("Spotify does not allow controlling this device".into());
        }
        let (kind, commanded) = {
            let mut guard = self.remote.lock();
            let Some(remote) = guard.as_mut() else { return Ok(()); };
            let repeat = (remote.kind() == Kind::Owned).then(|| remote.spotify_repeat());
            if let Some(pending) = remote.pending.as_mut() {
                // Spotify still catching up with the router's own command.
                if pending.since.elapsed() < GRACE && !pending.met_by(player.as_ref(), repeat) {
                    pending.retries += 1;
                    return Ok(());
                }
            }
            let commanded = remote.pending.is_some();
            remote.pending = None;
            (remote.kind(), commanded)
        };
        let result = match kind {
            Kind::Owned => self.observe_owned(host, player, !commanded).await,
            Kind::Foreign => self.observe_foreign(host, player).await,
            Kind::Prepared => Ok(()),
        };
        if result.is_ok() {
            if let Some(remote) = self.remote.lock().as_mut() { remote.failures = 0; }
        }
        result
    }

    async fn observe_owned<H: Host>(&self, host: &H, player: Option<PlayerState>, infer_transition: bool) -> Result<(), String> {
        enum Seen {
            /// Spotify plays this engine row of the window.
            Row(usize),
            /// Spotify left the order it was handed (a removed row, or
            /// autoplay past the last entry): the order goes on after this row.
            Resume(Option<usize>),
            Elsewhere,
        }
        let Some(player) = player else {
            // The device dropped its session: show it stopped, keep watching slowly.
            if let Some(remote) = self.remote.lock().as_mut() {
                remote.rebase();
                remote.state.playing = false;
                remote.state.error.clear();
                remote.paused_checks = remote.paused_checks.saturating_add(1);
            }
            self.publish_transport(host);
            return Ok(());
        };
        let seen = {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().ok_or(RELEASED)?;
            let past_end = remote.state.playing && remote.position().saturating_add(END_SLACK_MS) >= remote.state.duration_ms;
            let plan_kept = !player.shuffle_state && player.repeat_state == remote.spotify_repeat();
            let reset = infer_transition && remote.state.repeat != "track" && player.is_playing
                && (past_end || remote.position() > player.progress_ms.unwrap_or(0).saturating_add(3000));
            let Mode::Owned(window) = &mut remote.mode else { return Ok(()); };
            let uri = item_uri(&player);
            // Spotify exposes a URI, not an occurrence ID. A progress reset
            // disambiguates consecutive copies; command confirmations must
            // not count an already-applied skip twice.
            let next_copy = reset && window.uris.get(window.at + 1).is_some_and(|next| next == uri)
                && window.uris.get(window.at).is_some_and(|current| current == uri);
            let found = if next_copy { Some(window.at + 1) } else {
                window.uris[window.at..].iter().position(|held| held == uri).map(|j| window.at + j)
                    .or_else(|| window.uris[..window.at].iter().rposition(|held| held == uri))
            };
            match found {
                // Spotify's own shuffle or repeat reorders what it holds: that is
                // the phone taking over, not the window playing.
                Some(_) if !plan_kept => Seen::Elsewhere,
                Some(j) => {
                    if j > window.at {
                        remote.consumed.extend(window.rows[window.at + 1..=j].iter().flatten());
                    } else {
                        for row in window.rows[j + 1..=window.at].iter().flatten() { remote.consumed.remove(row); }
                    }
                    window.at = j;
                    let row = window.rows[j];
                    let before = window.rows[..j].iter().rev().flatten().next().copied();
                    remote.adopt_transport(&player);
                    match row {
                        Some(row) => {
                            remote.show_row(Some(row));
                            if let Some(duration) = player.item.as_ref().and_then(|item| item["duration_ms"].as_u64()).filter(|duration| *duration > 0) {
                                remote.state.duration_ms = duration.min(u64::from(u32::MAX)) as u32;
                            }
                            Seen::Row(row)
                        }
                        // A paused edit removed this entry, then the phone played on.
                        None => Seen::Resume(before),
                    }
                }
                // Past the last entry Spotify's autoplay takes over: the order
                // either goes on (the window was not extended in time) or ends.
                None if past_end && window.at + 1 == window.uris.len() => Seen::Resume(window.rows[window.at]),
                None => Seen::Elsewhere,
            }
        };
        match seen {
            Seen::Row(row) => {
                if self.remote.lock().as_ref().is_some_and(|remote| remote.anchor != Some(row)) {
                    self.reanchor(host, row, player.progress_ms.unwrap_or(0)).await?;
                }
                let order = self.order(host, Some(row), row).await?;
                self.settle(host, row, order).await
            }
            Seen::Resume(after) => {
                let order = match after {
                    Some(row) => self.order(host, Some(row), row).await?,
                    None => self.order(host, None, 0).await?,
                };
                match order.first() { Some(&next) => self.start(host, next, 0).await, None => self.finish(host).await }
            }
            Seen::Elsewhere => {
                // The phone started something else: show it, leave the engine alone.
                // Do not surrender ownership or lose rows on a failed read.
                let queue = self.spotify_queue(host).await?;
                if let Some(remote) = self.remote.lock().as_mut() {
                    remote.mode = Mode::Foreign;
                    remote.adopt_transport(&player);
                    remote.state.shuffle = player.shuffle_state;
                    remote.state.repeat.clone_from(&player.repeat_state);
                    remote.show_foreign(Some(&player), &queue);
                }
                self.publish(host);
                Ok(())
            }
        }
    }

    async fn observe_foreign<H: Host>(&self, host: &H, player: Option<PlayerState>) -> Result<(), String> {
        // A foreign queue is read only on the slow reconciliation schedule.
        // Keeping old rows on failure avoids silently deleting queued entries.
        let queue = if player.as_ref().is_some_and(|player| player.item.is_some()) {
            self.spotify_queue(host).await?
        } else { Vec::new() };
        if let Some(remote) = self.remote.lock().as_mut() {
            match &player {
                Some(player) => {
                    remote.adopt_transport(player);
                    remote.state.shuffle = player.shuffle_state;
                    remote.state.repeat.clone_from(&player.repeat_state);
                }
                None => {
                    remote.rebase();
                    remote.state.playing = false;
                    remote.state.error.clear();
                    remote.paused_checks = remote.paused_checks.saturating_add(1);
                }
            }
            remote.show_foreign(player.as_ref(), &queue);
        }
        self.publish(host);
        Ok(())
    }

    async fn watch<H: Host>(&self, host: &H) {
        loop {
            let delay = self.remote.lock().as_ref().filter(|remote| remote.polling).map(reconcile_delay);
            let Some(delay) = delay else { self.changed.notified().await; continue; };
            tokio::select! {
                _ = self.changed.notified() => continue,
                _ = tokio::time::sleep(delay) => {},
            }
            let _operation = self.operation.lock().await;
            if self.remote.lock().as_ref().is_some_and(|remote| remote.polling) {
                self.reconcile(host).await;
            }
        }
    }

    /// One timed check. A transient failure backs off and keeps control; it
    /// surfaces only once it persists.
    async fn reconcile<H: Host>(&self, host: &H) {
        let Err(error) = self.sync(host).await else { return; };
        if !personal_api::is_transient(&error) {
            self.suspend(host, &error);
            return;
        }
        let surface = self.remote.lock().as_mut().is_some_and(|remote| {
            remote.failures = remote.failures.saturating_add(1);
            if remote.failures >= 3 { remote.state.error = error; true } else { false }
        });
        if surface { self.publish_transport(host); }
    }
}

/// Retries after the router's own command, at the expected end of the item
/// (Spotify moves on by itself), or after a slow heartbeat. A paused device is
/// checked at 30, 60, then every 120 seconds; failures back off to a minute.
fn reconcile_delay(remote: &Remote) -> Duration {
    if remote.failures > 0 { return Duration::from_secs(1 << remote.failures.min(6)).min(HEARTBEAT); }
    if let Some(pending) = &remote.pending { return Duration::from_millis(750 << pending.retries.min(3)); }
    if !remote.state.playing { return Duration::from_secs(30 << remote.paused_checks.min(2)); }
    if remote.state.duration_ms == 0 { return HEARTBEAT; }
    let remaining = remote.state.duration_ms.saturating_sub(remote.position());
    Duration::from_millis(u64::from(remaining) + 750).min(HEARTBEAT)
}

/// The engine's order from the device's row `current`, derived from the
/// engine's own order after `anchor`. After the device's row was removed,
/// `current` is `None` and `cut` is where that row was. `None` when the
/// engine's order does not reach back that far (sequential, without repeat,
/// behind the engine's own row): the engine must be re-anchored.
fn order_from(anchor: Option<usize>, plan: &[usize], shuffle: bool, repeat: &str, current: Option<usize>, cut: usize, consumed: &HashSet<usize>) -> Option<Vec<usize>> {
    if shuffle {
        // Shuffle's plan is a bag of draws: the device has spent some of them.
        return Some(anchor.into_iter().chain(plan.iter().copied())
            .filter(|row| Some(*row) != current && !consumed.contains(row)).collect());
    }
    let after = |row: usize| match current { Some(current) => row > current, None => row >= cut };
    if repeat == "context" {
        // One sequential lap holds every eligible row once: begin it after the device's row.
        let mut lap = plan.to_vec();
        lap.sort_unstable();
        lap.dedup();
        let split = lap.partition_point(|row| !after(*row));
        lap.rotate_left(split);
        return Some(lap);
    }
    if anchor.is_some_and(|anchor| cut < anchor) { return None; }
    Some(anchor.into_iter().chain(plan.iter().copied()).filter(|row| after(*row)).collect())
}

/// The rows after the device's row that Spotify can play, as many as fit.
fn window_tail(rows: &[Track], order: &[usize]) -> Vec<usize> {
    order.iter().copied().filter(|row| rows.get(*row).is_some_and(remote_playable)).take(WINDOW - 1).collect()
}

/// Whether Spotify's remaining entries fall short of `tail`: a different
/// order, or running low while the order goes on.
fn window_differs(window: &Window, current: usize, tail: &[usize]) -> bool {
    if window.stale || window.rows.get(window.at) != Some(&Some(current)) { return true; }
    let held = &window.rows[window.at + 1..];
    let prefix = held.len() <= tail.len() && held.iter().zip(tail).all(|(held, row)| *held == Some(*row));
    !prefix || held.len() < LOW_WATER && tail.len() > held.len()
}

fn next_revision(revision: u64) -> u64 { revision % MAX_REVISION + 1 }

fn projected_position(state: &PlaybackState, elapsed: Duration) -> u32 {
    let position = if state.playing { state.position_ms.saturating_add(elapsed.as_millis().min(u32::MAX as u128) as u32) } else { state.position_ms };
    if state.duration_ms == 0 { position } else { position.min(state.duration_ms) }
}

fn item_uri(player: &PlayerState) -> &str {
    player.item.as_ref().and_then(|item| item["uri"].as_str()).unwrap_or_default()
}

fn remote_playable(track: &Track) -> bool { validate_remote_track(track).is_ok() }

fn validate_remote_track(track: &Track) -> Result<(), String> {
    if track.unavailable { return Err("This item is unavailable for playback".into()); }
    let id = track.uri.strip_prefix("spotify:track:").or_else(|| track.uri.strip_prefix("spotify:episode:"));
    if !id.is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric())) {
        return Err("Remote playback requires a Spotify track or episode URI".into());
    }
    Ok(())
}

fn track_from_player(item: &Value) -> Result<Track, String> {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let uri = item["uri"].as_str().ok_or("Spotify playback item has no URI")?;
    let episode = item["type"] == "episode";
    let album = if episode { &item["show"] } else { &item["album"] };
    let images = if episode { &item["images"] } else { &album["images"] };
    let artists = item["artists"].as_array();
    Ok(Track {
        id: text(&item["id"]), uri: uri.to_owned(), name: text(&item["name"]),
        artist_names: if episode { vec![text(&album["publisher"])] } else { artists.map(|a| a.iter().map(|v| text(&v["name"])).collect()).unwrap_or_default() },
        artist_ids: artists.map(|a| a.iter().map(|v| text(&v["id"])).collect()).unwrap_or_default(),
        artist_id: artists.and_then(|a| a.first()).map(|a| text(&a["id"])).unwrap_or_default(),
        album_id: text(&album["id"]), album_name: text(&album["name"]),
        cover_url: images.as_array().and_then(|a| a.first()).map(|image| text(&image["url"])).unwrap_or_default(),
        duration_ms: item["duration_ms"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        ..Track::default()
    })
}

async fn run_local(client: &EngineClient, action: Action) -> Result<(), String> {
    match action {
        Action::Play => client.play().await,
        Action::Pause => client.pause().await,
        Action::Next => client.next().await,
        Action::Previous => client.previous().await,
        Action::Seek(ms) => client.seek(ms).await,
        Action::Volume(percent) => client.set_volume(percent).await,
        Action::Shuffle(enabled) => client.set_shuffle(enabled).await,
        Action::Repeat(mode) => client.set_repeat(&mode).await,
        Action::Speed(speed) => client.set_playback_speed(speed).await,
        Action::Queue(queue, index, context, automatic) => client.play_queue(&queue, index, 0, &context, automatic).await,
        Action::Index(index) => client.play_queue_index(index).await,
        Action::Add(track, context) => client.add_queue(&track, &context).await,
        Action::AddBatch(tracks, context) => client.add_queue_batch(&tracks, &context).await,
        Action::Remove(index) => client.remove_queue(index).await,
        Action::Move(from, to) => client.move_queue(from, to).await,
        Action::Preview(track, cuts, loop_range, ms, lease) => client.preview_track_edit(&track, &cuts, loop_range, ms, lease).await,
        Action::RestorePreview(lease) => client.restore_preview(lease).await,
    }
}

struct AppHost<'a> {
    app: &'a AppHandle,
    client: &'a EngineClient,
    personal: &'a PersonalApi,
}

impl Host for AppHost<'_> {
    async fn spotify(&self, method: Method, path: String, body: Option<Value>) -> Result<Option<Value>, String> {
        self.personal.player(self.app, method, &path, body).await
    }

    async fn engine(&self, op: Option<EngineOp<'_>>) -> Result<EngineView, String> {
        // Subscribed before the command: the engine answers a command first
        // and publishes the state it produced right after.
        let mut lines = self.client.subscribe_lines();
        match op {
            None => {}
            Some(EngineOp::Restore { queue, index, position_ms, context }) => self.client.restore_queue(queue, index, position_ms, context).await?,
            Some(EngineOp::Cursor { index, position_ms }) => self.client.set_queue_cursor(index, position_ms).await?,
            Some(EngineOp::Add(track, context)) => self.client.add_queue(track, context).await?,
            Some(EngineOp::AddBatch(tracks, context)) => self.client.add_queue_batch(tracks, context).await?,
            Some(EngineOp::Remove(index)) => self.client.remove_queue(index).await?,
            Some(EngineOp::Move(from, to)) => self.client.move_queue(from, to).await?,
            Some(EngineOp::Shuffle(enabled)) => self.client.set_shuffle(enabled).await?,
            Some(EngineOp::Repeat(mode)) => self.client.set_repeat(mode).await?,
        }
        // The engine handles requests in order, so once this answer arrives
        // every state line the command produced has already been fanned out.
        let metadata = self.client.queue_metadata(None).await;
        let mut state = None;
        loop {
            match lines.try_recv() {
                Ok(StateLine::State(line)) => state = Some(line),
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        if state.is_none() { state = self.client.current_state(); }
        let upcoming = match metadata {
            Ok(metadata) => metadata.upcoming,
            Err(error) => match &state {
                Some(state) => state.upcoming.clone(),
                None => return Err(error),
            },
        };
        Ok(EngineView { state, upcoming })
    }

    async fn exclusions(&self, context: &str) -> Result<Vec<String>, String> {
        Ok(self.client.queue_metadata(Some(context)).await?.excluded_track_ids)
    }

    async fn pause_engine(&self) -> Result<(), String> { self.client.pause().await }

    async fn local(&self, action: Action) -> Result<(), String> { run_local(self.client, action).await }

    fn local_state(&self) -> PlaybackState {
        let managed = self.app.state::<Mutex<AppState>>();
        let guard = managed.lock();
        if guard.playback.preview { return guard.playback.clone(); }
        self.client.current_state().unwrap_or_else(|| guard.playback.clone())
    }

    fn publish_local(&self) {
        let managed = self.app.state::<Mutex<AppState>>();
        let guard = managed.lock();
        let _ = self.app.emit("state", PlaybackEvent::new(&guard.playback, true, true));
        media_keys::update_state(&guard.playback);
    }

    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) { let _ = self.app.emit(event, payload); }

    fn media(&self, state: &PlaybackState) { media_keys::update_state(state); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn track(id: &str) -> Track { Track { id: id.into(), uri: format!("spotify:track:{id}"), name: id.into(), duration_ms: 100_000, ..Track::default() } }

    fn device() -> Value {
        json!({"id":"phone", "name":"Phone", "type":"Smartphone", "is_active":true, "is_private_session":false, "is_restricted":false, "volume_percent":35, "supports_volume":true})
    }

    fn reading(uri: &str, playing: bool, progress: u32) -> Value {
        json!({"device": device(), "is_playing": playing, "progress_ms": progress, "shuffle_state": false, "repeat_state": "off",
            "item": {"id": uri.rsplit(':').next(), "uri": uri, "name": uri, "type": "track", "duration_ms": 100_000}})
    }

    /// A local stand-in for the Spotify Web API: it answers player reads from
    /// what the test sets and records every request.
    #[derive(Default)]
    struct Spotify {
        player: Option<Value>,
        queue: Vec<Value>,
        failures: std::collections::VecDeque<u16>,
        write_failures: std::collections::VecDeque<u16>,
        queue_failures: std::collections::VecDeque<u16>,
        log: Vec<(String, String, Option<Value>)>,
    }

    impl Spotify {
        /// Requests other than reads, as `METHOD path?query`.
        fn writes(&self) -> Vec<String> {
            self.log.iter().filter(|(method, ..)| method != "GET").map(|(method, path, _)| format!("{method} {path}")).collect()
        }
        fn last_play(&self) -> Option<Value> {
            self.log.iter().rev().find(|(method, path, _)| method == "PUT" && path.starts_with("me/player/play?")).and_then(|(.., body)| body.clone())
        }
        fn reads_of(&self, wanted: &str) -> usize {
            self.log.iter().filter(|(method, path, _)| method == "GET" && path.split('?').next() == Some(wanted)).count()
        }
    }

    async fn serve(spotify: Arc<Mutex<Spotify>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let spotify = spotify.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let end = loop {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "request ended before its headers");
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") { break end + 4; }
                    };
                    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
                    let mut request = head.lines().next().unwrap().split(' ');
                    let method = request.next().unwrap().to_owned();
                    let path = request.next().unwrap().strip_prefix("/v1/").unwrap().to_owned();
                    let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap()));
                    assert!(method == "GET" || length.is_some(), "{method} {path} was sent without a length");
                    let length = length.unwrap_or(0);
                    while bytes.len() - end < length {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "request ended before its body");
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    let body = (length > 0).then(|| serde_json::from_slice::<Value>(&bytes[end..end + length]).unwrap());
                    let (status, answer) = {
                        let mut spotify = spotify.lock();
                        spotify.log.push((method.clone(), path.clone(), body));
                        match (method.as_str(), path.split('?').next().unwrap()) {
                            ("GET", "me/player") => match spotify.failures.pop_front() {
                                Some(status) => (status, None),
                                None => spotify.player.clone().map_or((204, None), |player| (200, Some(player))),
                            },
                            ("GET", "me/player/devices") => (200, Some(json!({"devices": [device()]}))),
                            ("GET", "me/player/queue") => match spotify.queue_failures.pop_front() {
                                Some(status) => (status, None),
                                None => (200, Some(json!({"queue": spotify.queue}))),
                            },
                            _ => (spotify.write_failures.pop_front().unwrap_or(204), None),
                        }
                    };
                    let body = answer.map(|answer| answer.to_string()).unwrap_or_default();
                    stream.write_all(format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                });
            }
        });
        base
    }

    /// The engine's queue semantics, sequential only, without audio.
    struct FakeEngine {
        rows: Vec<Track>,
        current: Option<usize>,
        repeat: String,
        revision: u64,
        playing: bool,
        position_ms: u32,
        ops: Vec<String>,
    }

    impl FakeEngine {
        fn upcoming(&self) -> Vec<usize> {
            let Some(current) = self.current else { return (0..self.rows.len()).collect(); };
            let mut upcoming: Vec<usize> = (current + 1..self.rows.len()).filter(|row| !self.rows[*row].unavailable).collect();
            if self.repeat == "context" { upcoming.extend((0..=current).filter(|row| !self.rows[*row].unavailable)); }
            upcoming
        }
        fn state(&self) -> PlaybackState {
            PlaybackState { queue: self.rows.clone(), current_index: self.current, queue_revision: self.revision, repeat: self.repeat.clone(),
                upcoming: self.upcoming(), playing: self.playing, position_ms: self.position_ms, auth_state: "ready".into(), ready: true,
                current_uri: self.current.map(|row| self.rows[row].uri.clone()).unwrap_or_default(), duration_ms: 100_000, ..PlaybackState::default() }
        }
        fn apply(&mut self, op: Option<EngineOp<'_>>) -> EngineView {
            let Some(op) = op else { return EngineView { state: None, upcoming: self.upcoming() }; };
            match op {
                EngineOp::Cursor { index, position_ms } => {
                    self.ops.push(format!("cursor {index} {position_ms}"));
                    self.current = Some(index);
                    self.position_ms = position_ms;
                    return EngineView { state: Some(self.state()), upcoming: self.upcoming() };
                }
                EngineOp::Restore { queue, index, position_ms, .. } => {
                    self.ops.push(format!("restore {index} {position_ms}"));
                    self.rows = queue.to_vec();
                    self.current = (!queue.is_empty()).then_some(index);
                    self.position_ms = position_ms;
                }
                EngineOp::Add(track, _) => { self.ops.push(format!("add {}", track.id)); self.rows.push(track.clone()); }
                EngineOp::AddBatch(tracks, _) => { self.ops.push("add batch".into()); self.rows.extend_from_slice(tracks); }
                EngineOp::Remove(row) => {
                    self.ops.push(format!("remove {row}"));
                    self.rows.remove(row);
                    self.current = self.current.and_then(|current| match Remap::Remove(row).apply(current) {
                        Some(current) => Some(current),
                        None => (!self.rows.is_empty()).then(|| row.min(self.rows.len() - 1)),
                    });
                }
                EngineOp::Move(from, to) => {
                    self.ops.push(format!("move {from} {to}"));
                    let track = self.rows.remove(from);
                    self.rows.insert(to, track);
                    self.current = self.current.map(|current| Remap::Move(from, to).apply(current).unwrap());
                }
                EngineOp::Shuffle(enabled) => self.ops.push(format!("shuffle {enabled}")),
                EngineOp::Repeat(mode) => { self.ops.push(format!("repeat {mode}")); self.repeat = mode.into(); }
            }
            self.revision += 1;
            EngineView { state: Some(self.state()), upcoming: self.upcoming() }
        }
    }

    struct TestHost {
        base: String,
        http: reqwest::Client,
        spotify: Arc<Mutex<Spotify>>,
        engine: Mutex<FakeEngine>,
        local_shown: Mutex<u32>,
        events: Mutex<Vec<Value>>,
    }

    impl Host for TestHost {
        async fn spotify(&self, method: Method, path: String, body: Option<Value>) -> Result<Option<Value>, String> {
            let url = url::Url::parse(&format!("{}/v1/{path}", self.base)).unwrap();
            let response = personal_api::api_request(&self.http, "fixture", method, url, body).send().await
                .map_err(|_| personal_api::UNREACHABLE.to_owned())?;
            if !response.status().is_success() { return Err(personal_api::spotify_error(response.status())); }
            personal_api::read_player_response(response).await
        }
        async fn engine(&self, op: Option<EngineOp<'_>>) -> Result<EngineView, String> { Ok(self.engine.lock().apply(op)) }
        async fn exclusions(&self, _context: &str) -> Result<Vec<String>, String> { Ok(Vec::new()) }
        async fn pause_engine(&self) -> Result<(), String> { self.engine.lock().playing = false; Ok(()) }
        async fn local(&self, _action: Action) -> Result<(), String> { Ok(()) }
        fn local_state(&self) -> PlaybackState { self.engine.lock().state() }
        fn publish_local(&self) { *self.local_shown.lock() += 1; }
        fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) {
            if event == "state" { self.events.lock().push(serde_json::to_value(payload).unwrap()); }
        }
        fn media(&self, _state: &PlaybackState) {}
    }

    async fn fixture(rows: usize, playing: bool) -> (Core, TestHost) {
        let spotify = Arc::new(Mutex::new(Spotify::default()));
        let base = serve(spotify.clone()).await;
        let engine = FakeEngine { rows: (0..rows).map(|row| track(&format!("t{row}"))).collect(), current: Some(0), repeat: "off".into(),
            revision: 1, playing, position_ms: 0, ops: Vec::new() };
        let host = TestHost { base, http: reqwest::Client::builder().no_proxy().build().unwrap(), spotify, engine: Mutex::new(engine),
            local_shown: Mutex::new(0), events: Mutex::new(Vec::new()) };
        (Core::default(), host)
    }

    #[tokio::test]
    async fn failed_window_write_retains_additions_and_reconciles_without_resetting_position() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Add(track("added"), String::new())).await.is_err());
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "added"]);
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert!(matches!(&remote.mode, Mode::Owned(window) if window.stale));
            assert_eq!(remote.state.current_index, Some(0));
        });
        core.reconcile(&host).await;
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t1", "spotify:track:added"]);
        assert!(play["position_ms"].as_u64().unwrap() >= 1000);
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
    }

    #[tokio::test]
    async fn failed_foreign_queue_read_does_not_surrender_the_owned_order() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:phone", true, 100));
        host.spotify.lock().queue_failures.push_back(502);
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
        assert_eq!(core.snapshot().unwrap().queue.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "t2"]);
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Foreign);
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:phone");
        assert_eq!(host.engine.lock().current, Some(0));
    }

    #[tokio::test]
    async fn consecutive_copies_advance_occurrences_without_double_counting_own_next() {
        let (core, host) = fixture(3, true).await;
        host.engine.lock().rows[1] = track("t0");
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 5000));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(100));
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 200));
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        assert_eq!(host.engine.lock().current, Some(1));
        core.run(&host, Action::Previous).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 0));
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(0));
        core.run(&host, Action::Next).await.unwrap();
        core.sync(&host).await.unwrap();
        assert_eq!(core.snapshot().unwrap().current_index, Some(1));
        let engine = host.engine.lock();
        assert_eq!(engine.rows[0].uri, engine.rows[1].uri);
    }

    #[tokio::test]
    async fn explicit_empty_queue_clears_both_arrays_and_never_restores_old_rows() {
        let (core, host) = owned(2).await;
        core.run(&host, Action::Queue(Vec::new(), 0, String::new(), false)).await.unwrap();
        let events = host.events.lock();
        let clear = events.iter().rev().find(|event| event.get("queue").is_some()).unwrap();
        assert_eq!(clear["queue"], json!([]));
        assert_eq!(clear["upcoming"], json!([]));
        drop(events);
        core.select(&host, None).await.unwrap();
        assert!(host.engine.lock().rows.is_empty());
        assert_eq!(host.engine.lock().current, None);
    }

    #[tokio::test]
    async fn a_paused_edit_stages_new_order_until_resume_and_clears_recovered_error() {
        let (core, host) = owned(3).await;
        core.run(&host, Action::Pause).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", false, 1000));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        core.run(&host, Action::Move(2, 1)).await.unwrap();
        assert!(host.spotify.lock().writes().is_empty());
        assert_eq!(core.snapshot().unwrap().upcoming, [1, 2]);
        remote(&core, |remote| remote.state.error = "old failure".into());
        core.run(&host, Action::Play).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2", "spotify:track:t1"]);
        assert_eq!(play["position_ms"], 1000);
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn repeat_write_failure_retries_without_mistaking_old_setting_for_phone_takeover() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Repeat("track".into())).await.is_err());
        assert_eq!(host.engine.lock().repeat, "track");
        host.spotify.lock().player.as_mut().unwrap()["repeat_state"] = json!("track");
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
        assert!(!remote(&core, |remote| remote.repeat_dirty));
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn returning_local_never_overwrites_newer_canonical_rows() {
        let (core, host) = owned(2).await;
        // A canonical write landed outside the cached projection.
        host.engine.lock().rows.push(track("newer"));
        host.engine.lock().revision += 1;
        core.select(&host, None).await.unwrap();
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "newer"]);
        assert_eq!(host.engine.lock().current, Some(0));
    }

    #[tokio::test]
    async fn clear_pause_failure_retries_while_preserving_explicit_empty_queue() {
        let (core, host) = owned(2).await;
        host.spotify.lock().write_failures.push_back(502);
        assert!(core.run(&host, Action::Queue(Vec::new(), 0, String::new(), false)).await.is_err());
        assert!(core.snapshot().unwrap().queue.is_empty());
        assert!(remote(&core, |remote| remote.pause_dirty && remote.polling));
        core.reconcile(&host).await;
        assert!(!remote(&core, |remote| remote.pause_dirty || remote.polling));
        assert!(host.engine.lock().rows.is_empty());
        assert!(core.snapshot().unwrap().error.is_empty());
    }

    #[tokio::test]
    async fn unavailable_direct_play_does_not_replace_the_current_canonical_queue() {
        let (core, host) = owned(2).await;
        let mut unavailable = track("blocked");
        unavailable.unavailable = true;
        assert!(core.run(&host, Action::Queue(vec![unavailable], 0, String::new(), false)).await.is_err());
        assert_eq!(core.snapshot().unwrap().current_uri, "spotify:track:t0");
        assert_eq!(host.engine.lock().rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t0", "t1"]);
        assert!(host.spotify.lock().writes().is_empty());
    }

    #[tokio::test]
    async fn moved_or_restricted_devices_suspend_without_starting_local_audio() {
        for restricted in [false, true] {
            let (core, host) = owned(2).await;
            let mut player = reading("spotify:track:t0", true, 2000);
            if restricted { player["device"]["is_restricted"] = json!(true); }
            else { player["device"]["id"] = json!("elsewhere"); }
            host.spotify.lock().player = Some(player);
            core.reconcile(&host).await;
            assert!(!remote(&core, |remote| remote.polling));
            assert_eq!(remote(&core, |remote| remote.kind()), Kind::Owned);
            assert!(!host.engine.lock().playing);
            assert!(host.spotify.lock().writes().is_empty());
            assert!(core.snapshot().unwrap().error.contains(if restricted { "controlling this device" } else { "moved away" }));
        }
    }

    /// The router owns the phone, which plays `t0` of a queue of `rows`.
    async fn owned(rows: usize) -> (Core, TestHost) {
        let (core, host) = fixture(rows, true).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:t0", true, 1000));
        // Spotify confirms the start, settling the router's own expectation.
        core.sync(&host).await.unwrap();
        assert!(remote(&core, |remote| remote.pending.is_none()));
        host.spotify.lock().log.clear();
        (core, host)
    }

    fn remote<R>(core: &Core, read: impl FnOnce(&mut Remote) -> R) -> R { read(core.remote.lock().as_mut().unwrap()) }

    fn window_uris(remote: &Remote) -> Vec<String> {
        match &remote.mode { Mode::Owned(window) => window.uris.clone(), _ => panic!("the router does not own the device") }
    }

    fn uris(body: &Value) -> Vec<&str> { body["uris"].as_array().unwrap().iter().map(|uri| uri.as_str().unwrap()).collect() }

    #[tokio::test]
    async fn selecting_a_playing_queue_hands_spotify_a_window_of_the_engine_order() {
        let (core, host) = fixture(3, true).await;
        host.engine.lock().position_ms = 4321;
        core.select(&host, Some("phone".into())).await.unwrap();
        let spotify = host.spotify.lock();
        assert_eq!(spotify.writes(), [
            "PUT me/player", "PUT me/player/shuffle?device_id=phone&state=false",
            "PUT me/player/repeat?device_id=phone&state=off", "PUT me/player/play?device_id=phone",
        ]);
        let play = spotify.last_play().unwrap();
        assert_eq!(play, json!({"uris": ["spotify:track:t0", "spotify:track:t1", "spotify:track:t2"], "offset": {"position": 0}, "position_ms": 4321}));
        assert!(host.engine.lock().ops.is_empty(), "the engine queue is not touched by a handover");
        assert_eq!(remote(&core, |remote| remote.state.upcoming.clone()), [1, 2]);
    }

    #[tokio::test]
    async fn a_stale_reading_after_next_is_retried_and_never_takes_control_away() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Next).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["POST me/player/next?device_id=phone"]);
        assert_eq!(remote(&core, |remote| remote.state.current_index), Some(1));
        // Spotify still reports the item before the skip.
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert_eq!(remote.state.current_index, Some(1));
            assert_eq!(remote.pending.as_ref().unwrap().retries, 1);
            assert_eq!(reconcile_delay(remote), Duration::from_millis(1500));
        });
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 0, "a stale reading is not an outside change");
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 400));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert!(remote.pending.is_none());
            assert_eq!(remote.state.current_index, Some(1));
            assert!(matches!(&remote.mode, Mode::Owned(window) if window.at == 1));
        });
        assert_eq!(host.engine.lock().current, Some(1));
    }

    #[tokio::test]
    async fn adding_while_the_device_plays_reaches_spotify_without_restarting_the_song() {
        let (core, host) = owned(2).await;
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(5));
        core.run(&host, Action::Add(track("added"), String::new())).await.unwrap();
        assert_eq!(host.engine.lock().ops, ["add added"]);
        let play = host.spotify.lock().last_play().expect("the short window is extended with the addition");
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t1", "spotify:track:added"]);
        assert!(play["position_ms"].as_u64().unwrap() >= 6000, "resumes where the song was, not from 0");
        assert_eq!(remote(&core, |remote| remote.state.upcoming.clone()), [1, 2]);

        // A long window already holds enough: the addition waits for its turn.
        let (core, host) = owned(30).await;
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Add(track("late"), String::new())).await.unwrap();
        assert!(host.spotify.lock().last_play().is_none());
        assert_eq!(remote(&core, |remote| remote.state.upcoming.last().copied()), Some(30));
    }

    #[tokio::test]
    async fn adding_while_the_phone_plays_its_own_queue_uses_spotifys_queue() {
        let (core, host) = owned(2).await;
        host.spotify.lock().player = Some(reading("spotify:track:phone", true, 30_000));
        core.sync(&host).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Foreign);
        host.spotify.lock().queue = vec![json!({"id": "added", "uri": "spotify:track:added", "name": "added", "duration_ms": 1000})];
        core.run(&host, Action::Add(track("added"), String::new())).await.unwrap();
        assert!(host.spotify.lock().writes().contains(&"POST me/player/queue?device_id=phone&uri=spotify%3Atrack%3Aadded".to_owned()));
        assert!(host.engine.lock().ops.is_empty(), "the phone's queue is not the engine's");
        assert_eq!(remote(&core, |remote| remote.state.queue.iter().map(|track| track.id.clone()).collect::<Vec<_>>()), ["phone", "added"]);
    }

    #[tokio::test]
    async fn a_transient_failure_backs_off_and_keeps_control() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        host.spotify.lock().failures.extend([502, 502]);
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert!(remote.polling);
            assert_eq!(remote.kind(), Kind::Owned);
            assert_eq!(remote.failures, 1);
            assert!(remote.state.error.is_empty(), "one failure is not worth a message");
            assert_eq!(reconcile_delay(remote), Duration::from_secs(2));
        });
        core.reconcile(&host).await;
        assert_eq!(remote(&core, |remote| reconcile_delay(remote)), Duration::from_secs(4));
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert_eq!(remote.failures, 0);
            assert!(remote.polling);
        });
        // A permanent failure still stops reconciling with a visible error.
        host.spotify.lock().failures.push_back(401);
        core.reconcile(&host).await;
        remote(&core, |remote| {
            assert!(!remote.polling);
            assert!(remote.state.error.contains("authorization expired"));
        });
    }

    #[tokio::test]
    async fn the_device_plays_through_the_window_and_the_end_of_the_queue_is_seen() {
        let (core, host) = owned(2).await;
        core.sync(&host).await.unwrap();
        // Spotify moved on by itself: nothing is sent, the view follows.
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 800));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.state.current_index, Some(1));
            assert!(remote.state.playing);
            assert!(remote.state.upcoming.is_empty());
        });
        assert!(host.spotify.lock().writes().is_empty());
        // With autoplay off a finished single item can read as paused at 0.
        remote(&core, |remote| remote.anchored -= Duration::from_secs(120));
        host.spotify.lock().player = Some(reading("spotify:track:t1", false, 0));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert!(!remote.state.playing);
            assert_eq!(remote.kind(), Kind::Owned);
            assert_eq!(reconcile_delay(remote), Duration::from_secs(60));
        });

        // With autoplay on, a song after the last entry is stopped, not adopted.
        let (core, host) = owned(2).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 800));
        core.sync(&host).await.unwrap();
        remote(&core, |remote| remote.anchored -= Duration::from_secs(120));
        host.spotify.lock().player = Some(reading("spotify:track:autoplay", true, 900));
        core.sync(&host).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Owned);
            assert!(!remote.state.playing);
            assert_eq!(remote.state.current_index, Some(1));
        });
        assert_eq!(host.spotify.lock().reads_of("me/player/queue"), 0);
    }

    #[tokio::test]
    async fn a_track_change_near_the_end_of_the_window_extends_it() {
        let (core, host) = owned(150).await;
        core.sync(&host).await.unwrap();
        assert_eq!(remote(&core, |remote| window_uris(remote).len()), WINDOW);
        host.spotify.lock().player = Some(reading("spotify:track:t91", true, 1200));
        core.sync(&host).await.unwrap();
        let play = host.spotify.lock().last_play().expect("eight entries left is too few");
        let sent = uris(&play);
        assert_eq!(sent.len(), 59);
        assert_eq!((sent[0], sent[58]), ("spotify:track:t91", "spotify:track:t149"));
        assert!((1200..1300).contains(&play["position_ms"].as_u64().unwrap()));
        // The extension's own reading is pending until Spotify reports it.
        assert!(remote(&core, |remote| remote.pending.is_some()));
    }

    #[tokio::test]
    async fn the_phone_starting_something_else_is_shown_without_touching_the_engine() {
        let (core, host) = owned(3).await;
        core.sync(&host).await.unwrap();
        host.spotify.lock().player = Some(reading("spotify:track:podcast", true, 30_000));
        host.spotify.lock().queue = vec![json!({"id": "next", "uri": "spotify:track:next", "name": "next", "duration_ms": 1000})];
        core.sync(&host).await.unwrap();
        remote(&core, |remote| {
            assert_eq!(remote.kind(), Kind::Foreign);
            assert_eq!(remote.state.current_uri, "spotify:track:podcast");
            assert_eq!(remote.state.upcoming, [1]);
            assert!(remote.state.queue_revision <= MAX_REVISION);
        });
        assert!(host.engine.lock().ops.is_empty());
        // No more window: later readings are only shown.
        host.spotify.lock().player = Some(reading("spotify:track:podcast", false, 31_000));
        core.sync(&host).await.unwrap();
        assert!(host.spotify.lock().last_play().is_none());
        // This computer keeps the local queue the phone never played.
        core.select(&host, None).await.unwrap();
        assert!(core.remote.lock().is_none());
        assert!(host.engine.lock().ops.is_empty());
        assert_eq!(*host.local_shown.lock(), 1);
    }

    #[tokio::test]
    async fn this_computer_resumes_where_the_device_played_the_engine_order() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 42_000));
        core.sync(&host).await.unwrap();
        host.spotify.lock().log.clear();
        core.select(&host, None).await.unwrap();
        assert_eq!(host.spotify.lock().writes(), ["PUT me/player/pause?device_id=phone"]);
        let engine = host.engine.lock();
        assert_eq!(engine.current, Some(1));
        assert!((42_000..43_000).contains(&engine.position_ms));
        assert_eq!(engine.rows.iter().map(|track| track.id.as_str()).collect::<Vec<_>>(), ["t0", "t1", "t2"]);
    }

    #[tokio::test]
    async fn a_paused_selection_waits_for_play_and_edits_stay_in_the_engine() {
        let (core, host) = fixture(3, false).await;
        core.select(&host, Some("phone".into())).await.unwrap();
        assert_eq!(remote(&core, |remote| remote.kind()), Kind::Prepared);
        assert!(!remote(&core, |remote| remote.polling), "nothing to watch before Play");
        core.run(&host, Action::Move(2, 1)).await.unwrap();
        assert_eq!(host.engine.lock().ops, ["move 2 1"]);
        assert!(host.spotify.lock().last_play().is_none());
        core.run(&host, Action::Play).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t0", "spotify:track:t2", "spotify:track:t1"]);
    }

    #[tokio::test]
    async fn removing_the_playing_row_continues_with_the_next_one() {
        let (core, host) = owned(3).await;
        host.spotify.lock().player = Some(reading("spotify:track:t1", true, 5000));
        core.sync(&host).await.unwrap();
        core.run(&host, Action::Remove(1)).await.unwrap();
        let play = host.spotify.lock().last_play().unwrap();
        assert_eq!(uris(&play), ["spotify:track:t2"]);
        assert_eq!(play["position_ms"], 0);
        assert_eq!(remote(&core, |remote| remote.state.current_index), Some(1));
    }

    #[test]
    fn the_order_from_the_device_is_read_off_the_engine_order() {
        let none = HashSet::new();
        // Sequential: what follows the device's row.
        assert_eq!(order_from(Some(0), &[1, 2, 3], false, "off", Some(2), 2, &none), Some(vec![3]));
        // A removed row resumes at its position, the engine's row included.
        assert_eq!(order_from(Some(2), &[3, 4], false, "off", None, 2, &none), Some(vec![2, 3, 4]));
        // Behind the engine's own row nothing is known.
        assert_eq!(order_from(Some(2), &[3], false, "off", Some(1), 1, &none), None);
        // Repeat walks one lap from after the device's row.
        assert_eq!(order_from(Some(0), &[1, 2, 3, 0], false, "context", Some(2), 2, &none), Some(vec![3, 0, 1, 2]));
        // Shuffle: the remaining draws, without the ones the device spent.
        let spent = HashSet::from([0, 7]);
        assert_eq!(order_from(Some(0), &[7, 3, 5, 1], true, "off", Some(3), 3, &spent), Some(vec![5, 1]));
    }

    #[test]
    fn a_window_is_resent_only_when_spotify_holds_the_wrong_order_or_too_little() {
        let window = |rows: &[usize], at: usize| Window { rows: rows.iter().copied().map(Some).collect(), uris: Vec::new(), at, stale: false };
        let long: Vec<usize> = (1..=20).collect();
        assert!(!window_differs(&window(&(0..=20).collect::<Vec<_>>(), 0), 0, &long));
        assert!(window_differs(&window(&(0..=20).collect::<Vec<_>>(), 0), 1, &long), "another row plays");
        let mut reordered: Vec<usize> = (0..=20).collect();
        reordered.swap(3, 4);
        assert!(window_differs(&window(&reordered, 0), 0, &long));
        assert!(window_differs(&window(&[0, 1, 2], 0), 0, &[1, 2, 3]), "low and the order goes on");
        assert!(!window_differs(&window(&[0, 1, 2], 0), 0, &[1, 2]), "low but nothing more to send");
    }

    #[test]
    fn edits_carry_rows_to_their_new_places() {
        assert_eq!((0..4).map(|row| Remap::Remove(1).apply(row)).collect::<Vec<_>>(), [Some(0), None, Some(1), Some(2)]);
        assert_eq!((0..4).map(|row| Remap::Move(3, 0).apply(row).unwrap()).collect::<Vec<_>>(), [1, 2, 3, 0]);
        assert_eq!((0..4).map(|row| Remap::Move(0, 2).apply(row).unwrap()).collect::<Vec<_>>(), [2, 0, 1, 3]);
    }

    #[test]
    fn positions_do_not_drift_while_paused_or_past_the_end() {
        let mut state = PlaybackState { playing: false, position_ms: 90000, duration_ms: 100000, ..PlaybackState::default() };
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 90000);
        state.playing = true;
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 100000);
        assert_eq!(projected_position(&state, Duration::from_secs(2)), 92000);
    }

    #[test]
    fn paused_devices_are_checked_less_and_less_often() {
        let mut remote = Remote::new(PlaybackState { playing: true, position_ms: 20000, duration_ms: 100000, ..PlaybackState::default() }, true);
        assert_eq!(reconcile_delay(&remote), HEARTBEAT);
        remote.state.position_ms = 99000;
        assert!(reconcile_delay(&remote) <= Duration::from_millis(1750));
        remote.state.playing = false;
        for (checks, seconds) in [(0, 30), (1, 60), (2, 120), (20, 120)] {
            remote.paused_checks = checks;
            assert_eq!(reconcile_delay(&remote), Duration::from_secs(seconds));
        }
    }

    #[test]
    fn the_phones_own_items_are_shown_with_their_details_and_duplicates() {
        let player: PlayerState = serde_json::from_value(json!({
            "device": device(), "is_playing": true, "progress_ms": 4321, "shuffle_state": false, "repeat_state": "off",
            "item": {"id":"episode", "uri":"spotify:episode:episode", "name":"New episode", "type":"episode", "duration_ms":500000, "images":[{"url":"cover"}], "show":{"id":"show", "name":"Show", "publisher":"Publisher"}}
        })).unwrap();
        let queue = vec![json!({"id":"a", "uri":"spotify:track:a", "name":"a", "duration_ms":100000}),
            json!({"id":"a", "uri":"spotify:track:a", "name":"a", "duration_ms":100000})];
        let mut remote = Remote::new(PlaybackState { queue: vec![track("old")], current_index: Some(0), queue_revision: 7, ..PlaybackState::default() }, true);
        remote.show_foreign(Some(&player), &queue);
        assert_eq!(remote.state.current_uri, "spotify:episode:episode");
        assert_eq!(remote.state.queue[0].artist_names, vec!["Publisher"]);
        assert_eq!(remote.state.queue[0].cover_url, "cover");
        assert_eq!(remote.state.duration_ms, 500000);
        assert_eq!(remote.state.upcoming, [1, 2]);
        assert_eq!(remote.state.queue_revision, 8);
        remote.show_foreign(None, &[]);
        assert_eq!(remote.state.current_index, None);
        assert_eq!(remote.state.current_uri, "");
    }
}
