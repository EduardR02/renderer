//! One output owner for UI commands and system media keys. The engine remains
//! paused while Spotify supplies remote audio; its local state stays intact.
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::Method;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::app::AppState;
use crate::engine_client::EngineClient;
use crate::media_keys;
use crate::personal_api::{PersonalApi, PlayerState};
use crate::types::{PlaybackState, Track};

pub enum Action {
    Play, Pause, Next, Previous, Seek(u32), Volume(u8), Shuffle(bool), Repeat(String), Speed(f32),
    Queue(Vec<Track>, usize, String, bool), Index(usize), Add(Track, String), AddBatch(Vec<Track>, String), Remove(usize), Move(usize, usize),
    Preview(Track, Vec<renderer_engine::protocol::TimeRange>, Option<renderer_engine::protocol::LoopRange>, u32, u64),
    RestorePreview(u64),
}

struct Remote {
    state: PlaybackState,
    supports_volume: bool,
    excluded: Vec<String>,
    anchored: Instant,
    polling: bool,
    /// A paused local queue waits for explicit Play before Spotify starts it.
    prepared: bool,
    sync_soon: bool,
}

pub struct PlaybackRouter {
    client: Arc<EngineClient>,
    personal: Arc<PersonalApi>,
    operation: AsyncMutex<()>,
    remote: Mutex<Option<Remote>>,
    changed: Notify,
}

impl PlaybackRouter {
    pub fn new(client: Arc<EngineClient>, personal: Arc<PersonalApi>) -> Arc<Self> {
        Arc::new(Self { client, personal, operation: AsyncMutex::new(()), remote: Mutex::new(None), changed: Notify::new() })
    }

    pub fn is_remote(&self) -> bool { self.remote.lock().is_some() }

    pub fn snapshot(&self) -> Option<PlaybackState> {
        self.remote.lock().as_ref().map(|remote| {
            let mut state = remote.state.clone();
            state.position_ms = projected_position(&state, remote.anchored.elapsed());
            state
        })
    }

    fn publish(&self, app: &AppHandle) {
        let guard = self.remote.lock();
        if let Some(remote) = guard.as_ref() {
            let _ = app.emit("state", &remote.state);
            media_keys::update_state(&remote.state);
        }
    }

    pub fn report_error(&self, app: &AppHandle, error: &str) {
        let mut guard = self.remote.lock();
        if let Some(remote) = guard.as_mut() {
            remote.state.position_ms = projected_position(&remote.state, remote.anchored.elapsed());
            remote.anchored = Instant::now();
            remote.state.error = error.to_owned();
        }
        drop(guard);
        let _ = app.emit("playback-action-error", error);
        self.publish(app);
        self.changed.notify_one();
    }

    pub fn suspend(&self, app: &AppHandle, error: &str) {
        if let Some(remote) = self.remote.lock().as_mut() { remote.polling = false; }
        self.report_error(app, error);
    }

    pub async fn select(&self, app: &AppHandle, device_id: Option<String>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let result = self.select_locked(app, device_id).await;
        if let Err(error) = &result { self.report_error(app, error); }
        result
    }

    async fn select_locked(&self, app: &AppHandle, device_id: Option<String>) -> Result<(), String> {
        let Some(device_id) = device_id.filter(|id| !id.is_empty()) else {
            return self.return_local(app).await;
        };
        if self.remote.lock().as_ref().is_some_and(|r| r.state.output_device_id == device_id) {
            // Reselecting a paused prepared queue must not adopt an unrelated
            // remote session before Renderer has started that queue.
            if self.remote.lock().as_ref().is_some_and(|r| r.prepared) { return Ok(()); }
            return self.sync_locked(app).await;
        }
        let device = self.personal.devices(app).await?.into_iter()
            .find(|device| device.id.as_deref() == Some(&device_id))
            .ok_or("Spotify device is unavailable; open Spotify there and refresh devices")?;
        if device.is_restricted { return Err("Spotify does not allow controlling this device".into()); }
        let mut state = self.snapshot().unwrap_or_else(|| app.state::<Mutex<AppState>>().lock().playback.clone());
        if state.preview { return Err("Return to This computer and finish the track preview before selecting another output".into()); }
        let held_exclusions = self.remote.lock().as_ref().map(|remote| remote.excluded.clone());
        let excluded = if let Some(excluded) = held_exclusions {
            excluded
        } else if let Some(id) = state.current_index.and_then(|index| state.queue.get(index))
            .and_then(|track| track.context.strip_prefix("playlist:")) {
            self.client.browse_playlist(id).await?.excluded_track_ids
        } else { Vec::new() };
        // Await the actual engine pause before any remote transfer or start.
        self.client.pause().await?;
        self.personal.transfer(app, device_id.clone(), false).await?;
        state.output_device_id = device_id;
        state.output_device_name = device.name;
        state.buffering = false;
        state.playback_speed = 1.0;
        state.audible_playback_speed = 1.0;
        state.queue_revision = 0;
        state.error.clear();
        let was_playing = state.playing;
        state.playing = false;
        let prepared = !was_playing && state.current_index.is_some();
        let remote = Remote { state, supports_volume: device.supports_volume, excluded, anchored: Instant::now(), polling: !prepared, prepared, sync_soon: false };
        *self.remote.lock() = Some(remote);
        self.publish(app);
        self.changed.notify_one();
        if was_playing {
            let state = self.snapshot().expect("selection installed remote state");
            if let Some(index) = state.current_index {
                let excluded = self.remote.lock().as_ref().unwrap().excluded.clone();
                self.start_queue(app, &state, index, state.position_ms, &excluded).await?;
                {
                    let mut guard = self.remote.lock();
                    let remote = guard.as_mut().unwrap();
                    remote.state.playing = true;
                    remote.sync_soon = true;
                }
                self.publish(app);
                self.changed.notify_one();
                return Ok(());
            }
        }
        if prepared { Ok(()) } else { self.sync_locked(app).await }
    }

    async fn return_local(&self, app: &AppHandle) -> Result<(), String> {
        let Some(mut state) = self.snapshot() else { return Ok(()); };
        // Always unlock the selector, even if a vanished device cannot be stopped.
        // Returning is deliberately paused, never an automatic local fallback.
        let stopped = self.command(app, &state, Method::PUT, "pause", &[], None).await;
        state.playing = false;
        state.output_device_id.clear();
        state.output_device_name.clear();
        state.error = stopped.as_ref().err().map(|error| format!("Could not pause the previous output: {error}. Stop it in Spotify before resuming here.")).unwrap_or_default();
        let restored = if let Some(index) = state.current_index {
            self.client.restore_queue(&state.queue, index, state.position_ms, "").await
        } else { self.client.pause().await };
        *self.remote.lock() = None;
        self.changed.notify_one();
        let _ = app.emit("state", &state);
        media_keys::update_state(&state);
        // Engine restoration emits its own authoritative local state.
        restored?;
        stopped
    }

    /// Disconnect never resumes local audio or leaves a dormant remote timer.
    pub async fn disconnect(&self, app: &AppHandle) {
        let _operation = self.operation.lock().await;
        if let Err(error) = self.return_local(app).await { self.report_error(app, &error); }
    }

    async fn command(&self, app: &AppHandle, state: &PlaybackState, method: Method, endpoint: &str, query: &[(&str, String)], body: Option<Value>) -> Result<(), String> {
        self.personal.player_command(app, &state.output_device_id, method, endpoint, query, body).await
    }

    async fn start_queue(&self, app: &AppHandle, state: &PlaybackState, index: usize, position_ms: u32, excluded: &[String]) -> Result<(), String> {
        let body = queue_body(&state.queue, index, position_ms, excluded)?;
        self.command(app, state, Method::PUT, "play", &[], Some(body)).await
    }

    pub async fn run(&self, app: &AppHandle, action: Action) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let remote = self.remote.lock().as_ref().map(|remote| remote.prepared);
        let changes_queue = matches!(action, Action::Queue(..) | Action::Index(_) | Action::Add(..) | Action::AddBatch(..) | Action::Remove(_) | Action::Move(..));
        let starts_prepared = remote == Some(true) && matches!(action, Action::Play | Action::Next | Action::Previous);
        let result = if remote.is_none() {
            self.run_local(action).await
        } else if changes_queue || starts_prepared {
            self.run_remote(app, self.snapshot().expect("operation holds selected output"), action).await
        } else {
            self.run_remote_control(app, action, remote.unwrap()).await
        };
        if let Err(error) = &result { self.report_error(app, error); }
        result
    }

    async fn run_remote_control(&self, app: &AppHandle, action: Action, prepared: bool) -> Result<(), String> {
        // Pause/seek/volume requests must not clone a large playlist queue.
        let (mut state, supports_volume) = {
            let guard = self.remote.lock();
            let remote = guard.as_ref().unwrap();
            (PlaybackState {
                output_device_id: remote.state.output_device_id.clone(),
                playing: remote.state.playing,
                position_ms: projected_position(&remote.state, remote.anchored.elapsed()),
                duration_ms: remote.state.duration_ms,
                volume: remote.state.volume,
                shuffle: remote.state.shuffle,
                repeat: remote.state.repeat.clone(),
                ..PlaybackState::default()
            }, remote.supports_volume)
        };
        match action {
            Action::Play => {
                self.client.pause().await?;
                self.command(app, &state, Method::PUT, "play", &[], None).await?;
                state.playing = true;
            }
            Action::Pause => {
                if !prepared { self.command(app, &state, Method::PUT, "pause", &[], None).await?; }
                state.playing = false;
            }
            Action::Next => { self.command(app, &state, Method::POST, "next", &[], None).await?; }
            Action::Previous => { self.command(app, &state, Method::POST, "previous", &[], None).await?; }
            Action::Seek(ms) => {
                state.position_ms = if state.duration_ms == 0 { ms } else { ms.min(state.duration_ms) };
                if !prepared { self.command(app, &state, Method::PUT, "seek", &[("position_ms", state.position_ms.to_string())], None).await?; }
            }
            Action::Volume(percent) => {
                if percent > 100 { return Err("volume percent must be between 0 and 100".into()); }
                if !supports_volume { return Err("This Spotify device does not support remote volume control".into()); }
                self.command(app, &state, Method::PUT, "volume", &[("volume_percent", percent.to_string())], None).await?;
                state.volume = percent;
            }
            Action::Shuffle(enabled) => {
                self.command(app, &state, Method::PUT, "shuffle", &[("state", enabled.to_string())], None).await?;
                state.shuffle = enabled;
            }
            Action::Repeat(mode) => {
                if !matches!(mode.as_str(), "off" | "context" | "track") { return Err("invalid repeat mode".into()); }
                self.command(app, &state, Method::PUT, "repeat", &[("state", mode.clone())], None).await?;
                state.repeat = mode;
            }
            Action::Speed(_) | Action::Preview(..) | Action::RestorePreview(_) => return Err("Playback speed and track-editor previews require This computer; Spotify devices play original audio".into()),
            _ => unreachable!("queue changes use the queue transaction"),
        }
        let mut guard = self.remote.lock();
        let remote = guard.as_mut().unwrap();
        let plan_changed = remote.state.shuffle != state.shuffle || remote.state.repeat != state.repeat;
        remote.state.playing = state.playing;
        remote.state.position_ms = state.position_ms;
        remote.state.volume = state.volume;
        remote.state.shuffle = state.shuffle;
        remote.state.repeat = state.repeat;
        remote.state.error.clear();
        remote.anchored = Instant::now();
        remote.polling = !prepared;
        remote.sync_soon = !prepared;
        if plan_changed { remote.state.upcoming = sequential_upcoming(&remote.state, &remote.excluded); }
        let mut payload = json!({ "playing": remote.state.playing, "position_ms": remote.state.position_ms,
            "volume": remote.state.volume, "shuffle": remote.state.shuffle, "repeat": remote.state.repeat, "error": "" });
        if plan_changed { payload["upcoming"] = json!(remote.state.upcoming); }
        let _ = app.emit("state", payload);
        media_keys::update_state(&remote.state);
        drop(guard);
        self.changed.notify_one();
        Ok(())
    }

    async fn run_local(&self, action: Action) -> Result<(), String> {
        match action {
            Action::Play => self.client.play().await,
            Action::Pause => self.client.pause().await,
            Action::Next => self.client.next().await,
            Action::Previous => self.client.previous().await,
            Action::Seek(ms) => self.client.seek(ms).await,
            Action::Volume(percent) => self.client.set_volume(percent).await,
            Action::Shuffle(enabled) => self.client.set_shuffle(enabled).await,
            Action::Repeat(mode) => self.client.set_repeat(&mode).await,
            Action::Speed(speed) => self.client.set_playback_speed(speed).await,
            Action::Queue(queue, index, context, automatic) => self.client.play_queue(&queue, index, 0, &context, automatic).await,
            Action::Index(index) => self.client.play_queue_index(index).await,
            Action::Add(track, context) => self.client.add_queue(&track, &context).await,
            Action::AddBatch(tracks, context) => self.client.add_queue_batch(&tracks, &context).await,
            Action::Remove(index) => self.client.remove_queue(index).await,
            Action::Move(from, to) => self.client.move_queue(from, to).await,
            Action::Preview(track, cuts, loop_range, ms, lease) => self.client.preview_track_edit(&track, &cuts, loop_range, ms, lease).await,
            Action::RestorePreview(lease) => self.client.restore_preview(lease).await,
        }
    }

    async fn run_remote(&self, app: &AppHandle, mut state: PlaybackState, action: Action) -> Result<(), String> {
        let mut excluded = self.remote.lock().as_ref().unwrap().excluded.clone();
        match action {
            Action::Play => {
                self.client.pause().await?;
                state.playing = true;
            }
            Action::Next | Action::Previous => {
                let current = state.current_index.ok_or("No track is selected")?;
                if matches!(action, Action::Next) {
                    if let Some(index) = state.upcoming.first().copied() {
                        state.current_index = Some(index);
                        state.playing = true;
                    } else { state.playing = false; }
                } else {
                    state.current_index = Some(if state.position_ms >= 3000 { current } else { current.saturating_sub(1) });
                    state.playing = true;
                }
                state.position_ms = 0;
            }
            Action::Queue(mut queue, index, context, automatic) => {
                for track in &mut queue { if track.context.is_empty() { track.context.clone_from(&context); } }
                excluded = if let Some(id) = context.strip_prefix("playlist:") {
                    self.client.browse_playlist(id).await?.excluded_track_ids
                } else { Vec::new() };
                if automatic && queue.get(index).is_some_and(|track| excluded.contains(&track.id)) {
                    return Err("The requested automatic start is excluded from this playlist".into());
                }
                self.client.pause().await?;
                state.queue = queue;
                state.current_index = Some(index);
                state.position_ms = 0;
                state.playing = true;
            }
            Action::Index(index) => { state.current_index = Some(index); state.position_ms = 0; state.playing = true; }
            Action::Add(mut track, context) => {
                if track.context.is_empty() { track.context = context; }
                state.queue.push(track);
                if state.current_index.is_none() { state.current_index = Some(0); state.position_ms = 0; }
            }
            Action::AddBatch(mut tracks, context) => {
                for track in &mut tracks { if track.context.is_empty() { track.context.clone_from(&context); } }
                state.queue.extend(tracks);
                if state.current_index.is_none() && !state.queue.is_empty() {
                    state.current_index = Some(0);
                    state.position_ms = 0;
                }
            }
            Action::Remove(index) => remove_row(&mut state, index)?,
            Action::Move(from, to) => move_row(&mut state, from, to)?,
            _ => unreachable!("scalar controls use the scalar transaction"),
        }
        if let Some(index) = state.current_index {
            if state.playing {
                self.start_queue(app, &state, index, state.position_ms, &excluded).await?;
            } else {
                // Spotify cannot edit a paused queue in place. Validate and
                // stage the edited queue until Play, without an audible blip.
                queue_body(&state.queue, index, state.position_ms, &excluded)?;
            }
            let track = &state.queue[index];
            state.current_uri.clone_from(&track.uri);
            state.duration_ms = track.duration_ms;
            state.queue_revision = 0;
        } else {
            self.command(app, &state, Method::PUT, "pause", &[], None).await?;
            state.playing = false;
            state.current_uri.clear();
            state.duration_ms = 0;
            state.position_ms = 0;
        }
        state.error.clear();
        state.upcoming = sequential_upcoming(&state, &excluded);
        {
            let mut guard = self.remote.lock();
            let remote = guard.as_mut().unwrap();
            remote.state = state;
            remote.excluded = excluded;
            remote.anchored = Instant::now();
            remote.prepared = !remote.state.playing && remote.state.current_index.is_some();
            remote.polling = !remote.prepared;
            remote.sync_soon = remote.polling;
        }
        self.publish(app);
        self.changed.notify_one();
        // Commands publish confirmed intent immediately. The next active-only
        // sync reconciles delayed Spotify state without undoing a seek/play reply.
        Ok(())
    }

    async fn sync_locked(&self, app: &AppHandle) -> Result<(), String> {
        if !self.is_remote() { return Ok(()); }
        let Some(player) = self.personal.player_state(app).await? else {
            return Err("Spotify reports no playback on the selected device; open Spotify there or select This computer".into());
        };
        let spotify_queue = if player.shuffle_state { Some(self.personal.player_queue(app).await?) } else { None };
        let mut guard = self.remote.lock();
        let Some(remote) = guard.as_mut() else { return Ok(()); };
        let state = &mut remote.state;
        if player.device.id.as_deref() != Some(&state.output_device_id) {
            return Err("Spotify playback moved away from the selected device; reselect a device or This computer".into());
        }
        let identity_changed = player.item.as_ref().and_then(|item| item["uri"].as_str()).unwrap_or_default() != state.current_uri;
        let plan_changed = identity_changed || state.shuffle != player.shuffle_state || state.repeat != player.repeat_state;
        let queue_changed = adopt_player(state, &player)?;
        if let Some(queue) = spotify_queue {
            state.upcoming = upcoming_from_spotify(state, &queue);
        } else if plan_changed {
            state.upcoming = sequential_upcoming(state, &remote.excluded);
        }
        state.error.clear();
        remote.supports_volume = player.device.supports_volume;
        remote.anchored = Instant::now();
        remote.polling = true;
        remote.sync_soon = false;
        if queue_changed || identity_changed {
            let _ = app.emit("state", &*state);
        } else {
            // Scalar reconciliation neither clones nor serializes a long queue.
            let mut payload = json!({ "playing": state.playing, "position_ms": state.position_ms,
                "duration_ms": state.duration_ms, "volume": state.volume, "shuffle": state.shuffle,
                "repeat": state.repeat, "current_index": state.current_index,
                "output_device_id": state.output_device_id, "output_device_name": state.output_device_name, "error": "" });
            if plan_changed || state.shuffle { payload["upcoming"] = json!(state.upcoming); }
            let _ = app.emit("state", payload);
        }
        media_keys::update_state(state);
        Ok(())
    }

    pub async fn watch(self: Arc<Self>, app: AppHandle) {
        loop {
            let delay = self.remote.lock().as_ref().filter(|r| r.polling)
                .map(|r| if r.sync_soon { Duration::from_millis(750) } else { Duration::from_secs(if r.state.playing { 5 } else { 15 }) });
            let Some(delay) = delay else { self.changed.notified().await; continue; };
            tokio::select! {
                _ = self.changed.notified() => continue,
                _ = tokio::time::sleep(delay) => {},
            }
            let _operation = self.operation.lock().await;
            if !self.remote.lock().as_ref().is_some_and(|r| r.polling) { continue; }
            if let Err(error) = self.sync_locked(&app).await {
                self.suspend(&app, &error);
            }
        }
    }
}

fn projected_position(state: &PlaybackState, elapsed: Duration) -> u32 {
    let position = if state.playing { state.position_ms.saturating_add(elapsed.as_millis().min(u32::MAX as u128) as u32) } else { state.position_ms };
    if state.duration_ms == 0 { position } else { position.min(state.duration_ms) }
}

fn queue_body(queue: &[Track], index: usize, position_ms: u32, excluded: &[String]) -> Result<Value, String> {
    let current = queue.get(index).ok_or("queue index is out of range")?;
    if current.unavailable { return Err("This item is unavailable for playback".into()); }
    let mut uris = Vec::with_capacity(queue.len());
    let mut offset = 0;
    for (row, track) in queue.iter().enumerate() {
        if track.unavailable || row != index && excluded.contains(&track.id) { continue; }
        let id = track.uri.strip_prefix("spotify:track:").or_else(|| track.uri.strip_prefix("spotify:episode:"));
        if !id.is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric())) {
            return Err("Remote playback requires a Spotify track or episode URI".into());
        }
        if row == index { offset = uris.len(); }
        uris.push(track.uri.as_str());
    }
    // Never silently truncate the user's queue. Spotify's own request limits
    // are reported as visible API failures.
    Ok(json!({ "uris": uris, "offset": { "position": offset }, "position_ms": position_ms }))
}

fn sequential_upcoming(state: &PlaybackState, excluded: &[String]) -> Vec<usize> {
    if state.shuffle { return Vec::new(); }
    let Some(index) = state.current_index else { return Vec::new(); };
    let included = |row: &usize| !state.queue[*row].unavailable && !excluded.contains(&state.queue[*row].id);
    let mut upcoming: Vec<_> = (index + 1..state.queue.len()).filter(included).collect();
    if state.repeat == "context" { upcoming.extend((0..index).filter(included)); }
    upcoming
}

fn upcoming_from_spotify(state: &PlaybackState, queue: &[Value]) -> Vec<usize> {
    let mut upcoming = Vec::new();
    for item in queue {
        let uri = item["uri"].as_str().unwrap_or_default();
        if let Some(index) = state.queue.iter().enumerate().find_map(|(i, track)|
            (track.uri == uri && Some(i) != state.current_index && !upcoming.contains(&i)).then_some(i)) {
            upcoming.push(index);
        }
    }
    upcoming
}

fn adopt_player(state: &mut PlaybackState, player: &PlayerState) -> Result<bool, String> {
    if let Some(item) = player.item.as_ref() {
        if item["uri"].as_str().is_none() { return Err("Spotify playback item has no URI".into()); }
    }
    state.playing = player.is_playing;
    state.position_ms = player.progress_ms.unwrap_or(0);
    state.volume = player.device.volume_percent.unwrap_or(state.volume);
    state.output_device_name.clone_from(&player.device.name);
    state.shuffle = player.shuffle_state;
    state.repeat.clone_from(&player.repeat_state);
    let Some(item) = player.item.as_ref() else {
        state.current_uri.clear(); state.current_index = None; state.duration_ms = 0; return Ok(false);
    };
    let uri = item["uri"].as_str().ok_or("Spotify playback item has no URI")?;
    let changed = uri != state.current_uri;
    // Keep the current occurrence of duplicate songs. On a transition prefer
    // the next occurrence, then wrap, rather than jumping to the first match.
    let index = if !changed { state.current_index.filter(|&i| state.queue.get(i).is_some_and(|t| t.uri == uri)) } else { None }
        .or_else(|| {
            let start = state.current_index.map_or(0, |i| i + 1);
            (start..state.queue.len()).chain(0..start.min(state.queue.len())).find(|&i| state.queue[i].uri == uri)
        });
    let mut queue_changed = false;
    state.current_index = Some(if let Some(index) = index { index } else {
        // Playback changed in the official app (or Spotify autoplay). Do not
        // label that audio as the old Renderer song or fabricate its queue.
        state.queue = vec![track_from_player(item)?];
        state.queue_revision = 0;
        queue_changed = true;
        0
    });
    uri.clone_into(&mut state.current_uri);
    state.duration_ms = item["duration_ms"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
    Ok(queue_changed)
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

fn remove_row(state: &mut PlaybackState, index: usize) -> Result<(), String> {
    if index >= state.queue.len() { return Err("queue index is out of range".into()); }
    state.queue.remove(index);
    if let Some(current) = state.current_index {
        state.current_index = if state.queue.is_empty() { None } else if index < current { Some(current - 1) } else { Some(current.min(state.queue.len() - 1)) };
        if index == current { state.position_ms = 0; }
    }
    Ok(())
}

fn move_row(state: &mut PlaybackState, from: usize, to: usize) -> Result<(), String> {
    if from >= state.queue.len() || to >= state.queue.len() { return Err("queue index is out of range".into()); }
    let track = state.queue.remove(from);
    state.queue.insert(to, track);
    state.current_index = state.current_index.map(|current| {
        if current == from { to } else if from < current && current <= to { current - 1 }
        else if to <= current && current < from { current + 1 } else { current }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: &str) -> Track { Track { id: id.into(), uri: format!("spotify:track:{id}"), name: id.into(), duration_ms: 100_000, ..Track::default() } }
    #[test]
    fn remote_queue_preserves_requested_offset_and_skips_unavailable_and_excluded_rows() {
        let mut queue = vec![track("a"), track("b"), track("c"), track("d")];
        queue[0].unavailable = true;
        let body = queue_body(&queue, 2, 3210, &["b".into(), "c".into()]).unwrap();
        assert_eq!(body, json!({"uris": ["spotify:track:c", "spotify:track:d"], "offset": {"position": 0}, "position_ms": 3210}));
        assert!(queue_body(&queue, 0, 0, &[]).is_err());
        assert!(queue_body(&queue, 4, 0, &[]).is_err());
        queue[1].uri = "file:local-audio".into();
        assert!(queue_body(&queue, 2, 0, &[]).is_err());
    }
    #[test]
    fn moving_and_removing_duplicate_tracks_preserves_the_playing_occurrence() {
        let mut state = PlaybackState { queue: vec![track("a"), track("b"), track("a")], current_index: Some(2), position_ms: 5000, ..PlaybackState::default() };
        move_row(&mut state, 2, 0).unwrap();
        assert_eq!(state.current_index, Some(0));
        assert_eq!(state.position_ms, 5000);
        remove_row(&mut state, 1).unwrap();
        assert_eq!(state.current_index, Some(0));
        remove_row(&mut state, 0).unwrap();
        assert_eq!(state.current_index, Some(0));
        assert_eq!(state.position_ms, 0);
        remove_row(&mut state, 0).unwrap();
        assert_eq!(state.current_index, None);
    }
    #[test]
    fn official_device_audio_replaces_stale_renderer_metadata_and_adopts_episode_details() {
        let player: PlayerState = serde_json::from_value(json!({
            "device": {"id":"phone", "name":"Phone", "type":"Smartphone", "is_active":true, "is_private_session":false, "is_restricted":false, "volume_percent":35, "supports_volume":true},
            "is_playing":true, "progress_ms":4321, "shuffle_state":false, "repeat_state":"off",
            "item":{"id":"episode", "uri":"spotify:episode:episode", "name":"New episode", "type":"episode", "duration_ms":500000, "images":[{"url":"cover"}], "show":{"id":"show", "name":"Show", "publisher":"Publisher"}}
        })).unwrap();
        let mut state = PlaybackState { queue: vec![track("old")], current_index: Some(0), current_uri: "spotify:track:old".into(), ..PlaybackState::default() };
        assert!(adopt_player(&mut state, &player).unwrap());
        assert_eq!(state.current_uri, "spotify:episode:episode");
        assert_eq!(state.queue[0].name, "New episode");
        assert_eq!(state.queue[0].artist_names, vec!["Publisher"]);
        assert_eq!(state.queue[0].cover_url, "cover");
        assert_eq!(state.duration_ms, 500000);
        assert_eq!(state.position_ms, 4321);
        assert!(state.playing);
    }

    #[test]
    fn remote_state_keeps_duplicate_occurrences_and_clears_absent_current_metadata() {
        let mut player: PlayerState = serde_json::from_value(json!({
            "device": {"id":"phone", "name":"Phone", "type":"Smartphone", "is_active":true, "is_private_session":false, "is_restricted":false, "volume_percent":35, "supports_volume":true},
            "is_playing":true, "progress_ms":1000, "shuffle_state":false, "repeat_state":"off",
            "item":{"id":"a", "uri":"spotify:track:a", "name":"a", "type":"track", "duration_ms":100000}
        })).unwrap();
        let mut state = PlaybackState { queue: vec![track("a"), track("b"), track("a")], current_index: Some(1), current_uri: "spotify:track:b".into(), ..PlaybackState::default() };
        assert!(!adopt_player(&mut state, &player).unwrap());
        assert_eq!(state.current_index, Some(2));
        player.progress_ms = Some(8000);
        assert!(!adopt_player(&mut state, &player).unwrap());
        assert_eq!(state.current_index, Some(2));
        player.item = None;
        adopt_player(&mut state, &player).unwrap();
        assert_eq!(state.current_index, None);
        assert_eq!(state.current_uri, "");
        assert_eq!(state.duration_ms, 0);
    }

    #[test]
    fn paused_and_end_of_track_positions_do_not_drift_on_remote_output() {
        let mut state = PlaybackState { playing: false, position_ms: 90000, duration_ms: 100000, ..PlaybackState::default() };
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 90000);
        state.playing = true;
        assert_eq!(projected_position(&state, Duration::from_secs(60)), 100000);
        assert_eq!(projected_position(&state, Duration::from_secs(2)), 92000);
    }
}
