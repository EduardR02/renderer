mod audio;
mod auth;
mod browse;
mod edits;
mod engine;
mod io;
mod protocol;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use engine::{AuthSignal, Engine, PlayerSignal};
use io::{Input, ProtocolWriter};
use librespot_audio::AudioFetchParams;
use librespot_core::cache::Cache;
use protocol::{Command, Response};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

const AUDIO_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
const POSITION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Audio fetch tuning at engine startup (before any playback):
/// `read_ahead_during_playback` shrinks the streaming buffer from the
/// 5-second default to 2 seconds so play/pause/seek feel immediate, at the
/// cost of less jitter headroom; `download_timeout` drops from the
/// 8-second default to 3 seconds so a stalled stream is detected and
/// retried quickly instead of starving silently.
const AUDIO_READ_AHEAD_DURING_PLAYBACK: Duration = Duration::from_secs(2);
const AUDIO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3);

fn configure_audio_fetch() {
    let params = AudioFetchParams {
        read_ahead_during_playback: AUDIO_READ_AHEAD_DURING_PLAYBACK,
        download_timeout: AUDIO_DOWNLOAD_TIMEOUT,
        ..AudioFetchParams::default()
    };
    // The process sets this exactly once at startup; a second call (tests)
    // is a no-op by design.
    let _ = AudioFetchParams::set(params);
}

fn main() -> ExitCode {
    let state_directory = match parse_arguments(std::env::args_os().skip(1).collect()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("SpotifyPlaybackEngine: {error}");
            return ExitCode::from(2);
        }
    };
    let writer = match ProtocolWriter::capture_stdout() {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("SpotifyPlaybackEngine: could not capture protocol output: {error}");
            return ExitCode::FAILURE;
        }
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let (cache, temporary_directory, credentials_file) = match create_state(&state_directory) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("SpotifyPlaybackEngine: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("SpotifyPlaybackEngine: could not start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(
        writer,
        cache,
        temporary_directory,
        credentials_file,
    ));
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("SpotifyPlaybackEngine: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(
    writer: ProtocolWriter,
    cache: Cache,
    temporary_directory: PathBuf,
    credentials_file: PathBuf,
) -> Result<(), String> {
    let (input_sender, mut input_receiver) = mpsc::unbounded_channel();
    let (auth_sender, mut auth_receiver) = mpsc::unbounded_channel::<AuthSignal>();
    let (player_sender, mut player_receiver) = mpsc::unbounded_channel::<PlayerSignal>();
    io::spawn_input_reader(input_sender);

    configure_audio_fetch();

    let mut engine = Engine::new(
        writer,
        cache,
        temporary_directory,
        credentials_file,
    );
    engine.start_authentication(auth_sender.clone());
    engine.emit_state()?;

    let mut position_heartbeat = tokio::time::interval(POSITION_HEARTBEAT_INTERVAL);
    position_heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            input = input_receiver.recv() => {
                match input.unwrap_or(Input::Eof) {
                    Input::Request(request) => {
                        let request_id = request.request_id;
                        match request.command {
                            Command::Shutdown => {
                                let success = Ok(true);
                                engine.send_response(&request_id, &success)?;
                                engine.shutdown();
                                break;
                            }
                            Command::BrowsePlaylists { length } => {
                                let result = engine.browse_playlists(length).await;
                                engine.send_browse_response(&request_id, "browse_playlists", &result)?;
                            }
                            Command::BrowsePlaylist { id } => {
                                let result = engine.browse_playlist(&id).await;
                                engine.send_browse_response(&request_id, "browse_playlist", &result)?;
                            }
                            Command::BrowseAlbum { id } => {
                                let result = engine.browse_album(&id).await;
                                engine.send_browse_response(&request_id, "browse_album", &result)?;
                            }
                            Command::BrowseArtist { id } => {
                                let result = engine.browse_artist(&id).await;
                                engine.send_browse_response(&request_id, "browse_artist", &result)?;
                            }
                            Command::BrowseSearch { query, limit } => {
                                let result = engine.browse_search(&query, limit).await;
                                engine.send_browse_response(&request_id, "browse_search", &result)?;
                            }
                            Command::EditCreatePlaylist { name } => {
                                let result = engine.edit_create_playlist(&name).await;
                                engine.send_browse_response(&request_id, "edit_create_playlist", &result)?;
                            }
                            Command::EditRenamePlaylist { id, name } => {
                                let result = engine.edit_rename_playlist(&id, &name).await;
                                engine.send_edit_response(&request_id, "edit_rename_playlist", &result)?;
                            }
                            Command::EditDeletePlaylist { id } => {
                                let result = engine.edit_delete_playlist(&id).await;
                                engine.send_edit_response(&request_id, "edit_delete_playlist", &result)?;
                            }
                            Command::EditAddPlaylistTracks { id, uris } => {
                                let result = engine.edit_add_playlist_tracks(&id, &uris).await;
                                engine.send_edit_response(&request_id, "edit_add_playlist_tracks", &result)?;
                            }
                            Command::EditRemovePlaylistTracks { id, uris } => {
                                let result = engine.edit_remove_playlist_tracks(&id, &uris).await;
                                engine.send_edit_response(&request_id, "edit_remove_playlist_tracks", &result)?;
                            }
                            Command::EditReorderPlaylistTracks { id, from, to } => {
                                let result = engine.edit_reorder_playlist_tracks(&id, from, to).await;
                                engine.send_edit_response(&request_id, "edit_reorder_playlist_tracks", &result)?;
                            }
                            command => {
                                let result = engine.process_command(command, &auth_sender).await;
                                engine.send_response(&request_id, &result)?;
                                if matches!(result, Ok(true)) {
                                    engine.emit_state()?;
                                }
                            }
                        }
                    }
                    Input::Invalid { request_id, error } => {
                        engine.writer().send(&Response {
                            kind: "response",
                            request_id: &request_id,
                            ok: false,
                            error: Some(&error),
                        })?;
                    }
                    Input::Eof => {
                        engine.shutdown();
                        break;
                    }
                }
            }
            signal = auth_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_auth_signal(signal, player_sender.clone()) {
                        engine.emit_state()?;
                    }
                }
            }
            signal = player_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_player_signal(signal) {
                        engine.emit_state()?;
                    }
                }
            }
            _ = position_heartbeat.tick() => {
                if engine.tick_position() {
                    engine.emit_state()?;
                }
            }
        }
    }
    Ok(())
}

fn parse_arguments(arguments: Vec<OsString>) -> Result<PathBuf, String> {
    if arguments.len() != 2 || arguments[0] != "--state-dir" {
        return Err("usage: SpotifyPlaybackEngine.exe --state-dir <absolute-app-owned-path>".to_owned());
    }
    let path = PathBuf::from(&arguments[1]);
    if !path.is_absolute() {
        return Err("--state-dir must be an absolute path".to_owned());
    }
    Ok(path)
}

fn create_state(
    state_directory: &std::path::Path,
) -> Result<(Cache, PathBuf, PathBuf), String> {
    std::fs::create_dir_all(state_directory)
        .map_err(|error| format!("could not create state directory: {error}"))?;
    let credentials = state_directory.join("credentials");
    let volume = state_directory.join("volume");
    let audio = state_directory.join("audio");
    let temporary = state_directory.join("tmp");
    std::fs::create_dir_all(&temporary)
        .map_err(|error| format!("could not create temporary directory: {error}"))?;
    let cache = Cache::new(
        Some(credentials),
        Some(volume),
        Some(audio),
        Some(AUDIO_CACHE_LIMIT_BYTES),
    )
    .map_err(|error| format!("could not initialize the app-owned cache: {error}"))?;
    // librespot stores credentials as credentials.json inside the credentials
    // directory; `logout` removes it (Cache offers no removal API).
    let credentials_file = state_directory
        .join("credentials")
        .join("credentials.json");
    Ok((cache, temporary, credentials_file))
}
