//! Windows media-key integration: System Media Transport Controls (SMTC).
//!
//! Registers the main window as a media source so hardware play/pause/next/
//! previous keys control playback while the app is unfocused — the same
//! contract Chrome and Spotify fulfill. Registration also surfaces the
//! session in Quick Settings and on the lock screen; whether Windows draws
//! any overlay is its own decision and none of ours.
//!
//! Shape: one dedicated thread owns the `MediaControls` (the COM object
//! behind SMTC is happiest single-threaded), fed by a channel from the state
//! consumer. Button presses arrive on a system thread inside the attached
//! callback and are forwarded to the engine client as spawned async tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use crate::engine_client::EngineClient;
use crate::log;
use crate::types::PlaybackState;

use std::pin::Pin;

/// Last-known playing flag, so the overlay's toggle button maps to the right
/// engine command even though its press arrives on a foreign thread.
static PLAYING: AtomicBool = AtomicBool::new(false);

/// Published only after the controls attached successfully; every update
/// helper below becomes a no-op until then, so a failed registration costs
/// nothing beyond one warning at startup.
static UPDATES: OnceLock<Sender<Update>> = OnceLock::new();

enum Update {
    /// A full engine state: track identity plus transport status.
    State {
        playing: bool,
        position_ms: u32,
        duration_ms: u32,
        uri: String,
        title: String,
        artist: String,
        album: String,
        cover: String,
    },
    /// A scalar heartbeat: only the playhead moved.
    Position(u32),
    /// Nothing playable (logged out, empty queue).
    Stopped,
}

/// Attaches SMTC to the main window and starts the update thread. Failures
/// are warnings, never fatal: a player without media keys still plays.
pub fn init(client: Arc<EngineClient>, hwnd: *mut std::ffi::c_void) {
    // A window handle is pointer-sized by definition; `usize` is its Send-able
    // suitcase for the thread hop. Converted before the closure so the raw
    // pointer itself is never captured.
    let hwnd = hwnd as usize;
    let (sender, receiver) = channel::<Update>();

    let result = std::thread::Builder::new()
        .name("smtc".to_owned())
        .spawn(move || run_controls(client, hwnd, sender, receiver));
    if let Err(error) = result {
        log::warn(&format!("could not start the media-key thread: {error}"));
    }
}

fn run_controls(
    client: Arc<EngineClient>,
    hwnd: usize,
    sender: Sender<Update>,
    receiver: std::sync::mpsc::Receiver<Update>,
) {
    let config = PlatformConfig {
        dbus_name: "spotify-renderer",
        display_name: "Spotify Renderer",
        hwnd: Some(hwnd as *mut std::ffi::c_void),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => {
            log::warn(&format!("could not register media keys (SMTC): {error:?}"));
            return;
        }
    };

    let handler_client = client.clone();
    if let Err(error) = controls.attach(move |event| handle_event(event, &handler_client)) {
        log::warn(&format!("could not attach media-key handlers: {error:?}"));
        return;
    }
    log::info("media keys registered (SMTC)");

    // Registration raced the first engine state: if that line already passed,
    // nothing would re-send this track's metadata until the NEXT change (a
    // paused queue emits no heartbeats). One status request guarantees a full
    // state lands after attach.
    let refresh = client.clone();
    tauri::async_runtime::spawn(async move {
        let _ = refresh.request("status", serde_json::Value::Null).await;
    });

    if UPDATES.set(sender).is_err() {
        return;
    }

    // Metadata only changes with the track; resending it on every heartbeat
    // makes Windows repaint the flyout for nothing. Transport status and the
    // playhead position are cheap and sent every time.
    let mut last_uri = String::new();
    for update in receiver {
        let result = match update {
            Update::State {
                playing,
                position_ms,
                duration_ms,
                uri,
                title,
                artist,
                album,
                cover,
            } => {
                if uri != last_uri {
                    let metadata = MediaMetadata {
                        title: Some(&title),
                        artist: Some(&artist),
                        album: Some(&album),
                        cover_url: (!cover.is_empty()).then_some(cover.as_str()),
                        duration: Some(Duration::from_millis(duration_ms as u64)),
                    };
                    if let Err(error) = controls.set_metadata(metadata) {
                        log::warn(&format!("could not update media metadata: {error:?}"));
                    }
                    last_uri = uri;
                }
                controls.set_playback(transport(playing, position_ms))
            }
            Update::Position(position_ms) => controls
                .set_playback(transport(PLAYING.load(Ordering::Relaxed), position_ms)),
            Update::Stopped => {
                last_uri.clear();
                controls.set_playback(MediaPlayback::Stopped)
            }
        };
        if let Err(error) = result {
            log::warn(&format!("could not update media session: {error:?}"));
        }
    }
}

fn transport(playing: bool, position_ms: u32) -> MediaPlayback {
    let progress = Some(MediaPosition(Duration::from_millis(position_ms as u64)));
    if playing {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    }
}

fn handle_event(event: MediaControlEvent, client: &Arc<EngineClient>) {
    use MediaControlEvent as E;
    let client = client.clone();
    // Each `async move` block is its own anonymous type, so the arms must
    // erase to one boxed future for the match to typecheck.
    let task: Option<Pin<Box<dyn std::future::Future<Output = ()> + Send>>> = match event {
        E::Play => Some(Box::pin(async move { let _ = client.play().await; })),
        E::Pause | E::Stop => Some(Box::pin(async move { let _ = client.pause().await; })),
        E::Toggle => {
            if PLAYING.load(Ordering::Relaxed) {
                Some(Box::pin(async move { let _ = client.pause().await; }))
            } else {
                Some(Box::pin(async move { let _ = client.play().await; }))
            }
        }
        E::Next => Some(Box::pin(async move { let _ = client.next().await; })),
        E::Previous => Some(Box::pin(async move { let _ = client.previous().await; })),
        _ => None,
    };
    if let Some(task) = task {
        tauri::async_runtime::spawn(task);
    }
}

/// Mirrors one full engine state into the media session. Cheap: a channel
/// send; the owning thread deduplicates metadata against the previous track.
pub fn update_state(state: &PlaybackState) {
    PLAYING.store(state.playing, Ordering::Relaxed);
    let Some(sender) = UPDATES.get() else {
        return;
    };
    let Some(track) = state.current_index.and_then(|index| state.queue.get(index)) else {
        let _ = sender.send(Update::Stopped);
        return;
    };
    let _ = sender.send(Update::State {
        playing: state.playing,
        position_ms: state.position_ms,
        duration_ms: track_duration(state.duration_ms, track.duration_ms),
        uri: state.current_uri.clone(),
        title: track.name.clone(),
        artist: track.artist_names.join(", "),
        album: track.album_name.clone(),
        cover: track.cover_url.clone(),
    });
}

/// Mirrors a position heartbeat. Skipped entirely while paused: a frozen
/// playhead is already what the last transport update described.
pub fn update_position(position_ms: u32) {
    if !PLAYING.load(Ordering::Relaxed) {
        return;
    }
    if let Some(sender) = UPDATES.get() {
        let _ = sender.send(Update::Position(position_ms));
    }
}

fn track_duration(fallback_ms: u32, track_ms: u32) -> u32 {
    if track_ms > 0 { track_ms } else { fallback_ms }
}
