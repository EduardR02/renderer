mod app;
mod commands;
mod covers;
mod engine_client;
mod log;
mod types;

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;

use app::{AppState, data_dir, load_app_settings, load_tracks_cache};
use engine_client::EngineClient;

fn sync_autostart(app: &tauri::AppHandle, enabled: bool) {
    let manager = app.autolaunch();
    let actual = match manager.is_enabled() {
        Ok(value) => value,
        Err(error) => {
            log::warn(&format!(
                "could not read launch-at-login registration at startup: {error}"
            ));
            return;
        }
    };
    if actual == enabled {
        return;
    }

    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(error) = result {
        let action = if enabled { "enable" } else { "disable" };
        log::warn(&format!(
            "could not {action} launch-at-login registration at startup: {error}"
        ));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    log::init(app::logs_dir());
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::play,
            commands::pause,
            commands::next,
            commands::previous,
            commands::seek,
            commands::set_volume,
            commands::set_shuffle,
            commands::set_repeat,
            commands::set_playback_speed,
            commands::play_queue,
            commands::play_queue_index,
            commands::add_queue,
            commands::add_queue_batch,
            commands::remove_queue,
            commands::move_queue,
            commands::get_history,
            commands::clear_history,
            commands::get_track_edit,
            commands::save_track_edit,
            commands::delete_track_edit,
            commands::set_playlist_track_edit_enabled,
            commands::extract_track_waveform,
            commands::search,
            commands::browse_playlists,
            commands::browse_playlist,
            commands::browse_radio,
            commands::browse_playlist_recommendations,
            commands::browse_album,
            commands::browse_artist,
            commands::browse_artist_releases,
            commands::browse_artist_catalogue,
            commands::browse_liked_songs,
            commands::browse_track_credits,
            commands::browse_canvas,
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
            commands::get_cache_stats,
            commands::clear_cache,
            commands::get_app_settings,
            commands::set_audio_cache_limit,
            commands::set_launch_at_login,
            commands::set_start_minimized,
            commands::set_animated_canvas,
            commands::touch_playlist,
            commands::touch_playlist_activity,
        ])
        .setup(|app| {
            let startup_settings = load_app_settings();
            let startup_handle = app.handle().clone();
            sync_autostart(&startup_handle, startup_settings.launch_at_login);
            if startup_settings.start_minimized {
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(error) = window.show() {
                        log::warn(&format!("could not show the main window at startup: {error}"));
                    }
                    if let Err(error) = window.minimize() {
                        log::warn(&format!(
                            "could not minimize the main window at startup: {error}"
                        ));
                    }
                } else {
                    log::warn("could not find the main window to start minimized");
                }
            }

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
