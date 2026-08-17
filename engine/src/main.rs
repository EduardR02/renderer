mod auth;
mod engine;
mod io;
mod protocol;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use engine::{AuthSignal, Engine, PlayerSignal};
use io::{Input, ProtocolWriter};
use librespot_core::cache::Cache;
use protocol::{Command, Response};
use tokio::sync::mpsc;

const AUDIO_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

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

    let (cache, temporary_directory) = match create_state(&state_directory) {
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
    let result = runtime.block_on(run(writer, cache, temporary_directory));
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
) -> Result<(), String> {
    let (input_sender, mut input_receiver) = mpsc::unbounded_channel();
    let (auth_sender, mut auth_receiver) = mpsc::unbounded_channel::<AuthSignal>();
    let (player_sender, mut player_receiver) = mpsc::unbounded_channel::<PlayerSignal>();
    io::spawn_input_reader(input_sender);

    let mut engine = Engine::new(writer, cache, temporary_directory);
    engine.start_authentication(auth_sender.clone());
    engine.emit_state()?;

    loop {
        tokio::select! {
            input = input_receiver.recv() => {
                match input.unwrap_or(Input::Eof) {
                    Input::Request(request) => {
                        let request_id = request.request_id;
                        if matches!(&request.command, Command::Shutdown) {
                            let success = Ok(true);
                            engine.send_response(&request_id, &success)?;
                            engine.shutdown();
                            break;
                        }
                        let result = engine.process_command(request.command, &auth_sender);
                        engine.send_response(&request_id, &result)?;
                        if matches!(result, Ok(true)) {
                            engine.emit_state()?;
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

fn create_state(state_directory: &std::path::Path) -> Result<(Cache, PathBuf), String> {
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
    Ok((cache, temporary))
}
