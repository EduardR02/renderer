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

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::engine_client::EngineClient;
use crate::log;
use crate::types::PlaybackState;

/// Last-known playing flag, so the overlay's toggle button maps to the right
/// engine command even though its press arrives on a foreign thread.
static PLAYING: AtomicBool = AtomicBool::new(false);
static POSITION_MS: AtomicU64 = AtomicU64::new(0);
static DURATION_MS: AtomicU64 = AtomicU64::new(0);

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
        dbus_name: "renderer",
        display_name: "Renderer",
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
    #[cfg(windows)]
    if let Err(error) = disable_unsupported_seek_buttons(hwnd) {
        log::warn(&format!(
            "could not disable unsupported fast-forward/rewind controls: {error}"
        ));
    }

    // Publish the sender before asking for status. Otherwise a fast response
    // can arrive in the gap and be dropped, leaving SMTC stale until the next
    // full engine state.
    if UPDATES.set(sender).is_err() {
        return;
    }
    log::info("media keys registered (SMTC)");

    let refresh = client.clone();
    tauri::async_runtime::spawn(async move {
        let _ = refresh.request("status", serde_json::Value::Null).await;
    });

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
            Update::Position(position_ms) => {
                controls.set_playback(transport(PLAYING.load(Ordering::Relaxed), position_ms))
            }
            Update::Stopped => {
                PLAYING.store(false, Ordering::Relaxed);
                last_uri.clear();
                #[cfg(windows)]
                if let Err(error) = clear_windows_metadata(hwnd) {
                    log::warn(&format!("could not clear media metadata: {error}"));
                }
                #[cfg(not(windows))]
                if let Err(error) = controls.set_metadata(MediaMetadata::default()) {
                    log::warn(&format!("could not clear media metadata: {error:?}"));
                }
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
    let task: Option<Pin<Box<dyn Future<Output = ()> + Send>>> = match event {
        E::Play => Some(Box::pin(async move {
            let _ = client.play().await;
        })),
        E::Pause | E::Stop => Some(Box::pin(async move {
            let _ = client.pause().await;
        })),
        E::Toggle => {
            if PLAYING.load(Ordering::Relaxed) {
                Some(Box::pin(async move {
                    let _ = client.pause().await;
                }))
            } else {
                Some(Box::pin(async move {
                    let _ = client.play().await;
                }))
            }
        }
        E::Next => Some(Box::pin(async move {
            let _ = client.next().await;
        })),
        E::Previous => Some(Box::pin(async move {
            let _ = client.previous().await;
        })),
        E::SetPosition(position) => {
            let position_ms = absolute_seek_target(duration_ms(position.0));
            Some(Box::pin(async move {
                let _ = client.seek(position_ms).await;
            }))
        }
        E::SeekBy(direction, amount) => {
            let position_ms = relative_seek_target(direction, duration_ms(amount));
            Some(Box::pin(async move {
                let _ = client.seek(position_ms).await;
            }))
        }
        // Souvlaki exposes Windows' indeterminate FF/RW buttons as `Seek`.
        // They have no honest target, so those buttons are disabled at attach.
        E::Seek(_) => None,
        _ => None,
    };
    if let Some(task) = task {
        tauri::async_runtime::spawn(task);
    }
}

fn duration_ms(duration: Duration) -> u32 {
    duration.as_millis().min(u32::MAX as u128) as u32
}

fn absolute_seek_target(position_ms: u32) -> u32 {
    let duration = DURATION_MS.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
    if duration == 0 {
        position_ms
    } else {
        position_ms.min(duration)
    }
}

fn relative_seek_target(direction: SeekDirection, amount_ms: u32) -> u32 {
    // Reserve the playhead before dispatching the asynchronous seek. SMTC can
    // deliver several relative seeks before the engine emits a new state; a
    // plain load would make each request start from the same old position.
    let mut position = POSITION_MS.load(Ordering::Relaxed);
    loop {
        let position_ms = position.min(u32::MAX as u64) as u32;
        let duration = DURATION_MS.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
        let target = match direction {
            SeekDirection::Forward => position_ms.saturating_add(amount_ms),
            SeekDirection::Backward => position_ms.saturating_sub(amount_ms),
        };
        let target = if duration == 0 {
            target
        } else {
            target.min(duration)
        };
        match POSITION_MS.compare_exchange_weak(
            position,
            target as u64,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return target,
            Err(current) => position = current,
        }
    }
}

#[cfg(windows)]
fn windows_controls(
    hwnd: usize,
) -> windows::core::Result<windows::Media::SystemMediaTransportControls> {
    use windows::Media::SystemMediaTransportControls;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

    let interop = windows::core::factory::<
        SystemMediaTransportControls,
        ISystemMediaTransportControlsInterop,
    >()?;
    unsafe { interop.GetForWindow(HWND(hwnd as isize)) }
}

#[cfg(windows)]
fn disable_unsupported_seek_buttons(hwnd: usize) -> windows::core::Result<()> {
    let controls = windows_controls(hwnd)?;
    controls.SetIsFastForwardEnabled(false)?;
    controls.SetIsRewindEnabled(false)
}

#[cfg(windows)]
fn clear_windows_metadata(hwnd: usize) -> windows::core::Result<()> {
    let updater = windows_controls(hwnd)?.DisplayUpdater()?;
    updater.ClearAll()?;
    updater.Update()?;
    Ok(())
}

/// Mirrors one full engine state into the media session. Cheap: a channel
/// send; the owning thread deduplicates metadata against the previous track.
pub fn update_state(state: &PlaybackState) {
    PLAYING.store(state.playing, Ordering::Relaxed);
    POSITION_MS.store(state.position_ms as u64, Ordering::Relaxed);
    let Some(sender) = UPDATES.get() else {
        return;
    };
    let Some(track) = state.current_index.and_then(|index| state.queue.get(index)) else {
        DURATION_MS.store(0, Ordering::Relaxed);
        let _ = sender.send(Update::Stopped);
        return;
    };
    let duration_ms = track_duration(state.duration_ms, track.duration_ms);
    DURATION_MS.store(duration_ms as u64, Ordering::Relaxed);
    let _ = sender.send(Update::State {
        playing: state.playing,
        position_ms: state.position_ms,
        duration_ms,
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
    POSITION_MS.store(position_ms as u64, Ordering::Relaxed);
    if !PLAYING.load(Ordering::Relaxed) {
        return;
    }
    if let Some(sender) = UPDATES.get() {
        let _ = sender.send(Update::Position(position_ms));
    }
}

/// Clears transport and metadata immediately while the engine is unavailable.
pub fn update_disconnected() {
    PLAYING.store(false, Ordering::Relaxed);
    POSITION_MS.store(0, Ordering::Relaxed);
    DURATION_MS.store(0, Ordering::Relaxed);
    if let Some(sender) = UPDATES.get() {
        let _ = sender.send(Update::Stopped);
    }
}

fn track_duration(compiled_ms: u32, source_ms: u32) -> u32 {
    if compiled_ms > 0 {
        compiled_ms
    } else {
        source_ms
    }
}
