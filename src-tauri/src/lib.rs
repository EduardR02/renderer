mod app;
mod commands;
mod covers;
mod engine_client;
mod log;
mod types;

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::Manager;

use app::{AppState, data_dir, load_tracks_cache};
use engine_client::EngineClient;

/// Makes WebView2 composite in software.
///
/// WebView2 rasterizes the page into a GPU texture and then hands that texture
/// to DirectComposition to put on screen. The second step can fail on its own
/// while the first keeps working: the window paints its own background and the
/// page never appears over it, which reads as a black window even though the
/// DOM is intact. GPU/display state shifting under a live process is what
/// triggers it — driver reset, hybrid-GPU switch, display topology change, or
/// a Remote Desktop session attaching its virtual adapter.
///
/// Software compositing sidesteps that failure entirely and costs close to
/// nothing here: this UI is static apart from a progress bar redrawing a few
/// hundred pixels 4x a second, and skipping the GPU path drops a process, a
/// D3D context, and its surface memory. An existing value is left alone so the
/// flag stays overridable from the environment.
#[cfg(windows)]
fn force_software_compositing() {
    const VAR: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
    if std::env::var_os(VAR).is_none() {
        std::env::set_var(VAR, "--disable-gpu-compositing");
    }
}

#[cfg(not(windows))]
fn force_software_compositing() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    log::init(app::logs_dir());
    force_software_compositing();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::play,
            commands::pause,
            commands::next,
            commands::previous,
            commands::seek,
            commands::set_volume,
            commands::set_shuffle,
            commands::set_repeat,
            commands::play_queue,
            commands::add_queue,
            commands::remove_queue,
            commands::move_queue,
            commands::search,
            commands::browse_playlists,
            commands::browse_playlist,
            commands::browse_album,
            commands::browse_artist,
            commands::create_playlist,
            commands::rename_playlist,
            commands::delete_playlist,
            commands::add_playlist_tracks,
            commands::remove_playlist_tracks,
            commands::reorder_playlist_tracks,
            commands::status,
            commands::login,
            commands::logout,
            commands::get_state,
            commands::get_cover,
        ])
        .setup(|app| {
            let state = Mutex::new(AppState::new(data_dir()));
            let dir = state.lock().data_dir.clone();
            state.lock().tracks_cache = load_tracks_cache(&dir);
            app.manage(state);

            // Spawn the playback engine and keep it alive across crashes.
            let client = EngineClient::start();
            app.manage(client.clone());
            let supervisor = client.clone();
            tauri::async_runtime::spawn(async move { supervisor.supervise().await });
            // Re-request status shortly after startup so a state line always
            // lands after the event consumer has subscribed, even if the
            // engine's very first line beat it.
            let status_client = client.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = status_client
                    .request("status", serde_json::Value::Null)
                    .await;
            });

            // Mirror engine state into AppState and the frontend.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(commands::consume_states(app_handle));
            Ok(())
        })
        // cover://<sha1hex> serves cached cover art bytes to <img> tags.
        .register_uri_scheme_protocol("cover", |_context, request| {
            // Windows serves custom schemes as `http://cover.localhost/<hex>`,
            // where the hash is the path; elsewhere it stays `cover://<hex>`,
            // where the hash is the host. Concatenating the two produced
            // "cover.localhost<hex>" here, which matched no cached file — so
            // cover art had never once resolved on Windows.
            let uri = request.uri();
            let path = uri.path().trim_start_matches('/');
            let hex = if path.is_empty() {
                uri.host().unwrap_or_default()
            } else {
                path
            };
            covers::serve_cover(hex)
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(client) = app_handle.try_state::<Arc<EngineClient>>() {
                    client.shutdown_engine();
                }
            }
        });
}
