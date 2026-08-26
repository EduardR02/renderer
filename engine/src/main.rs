mod audio;
mod auth;
mod browse;
mod customization;
mod edits;
mod engine;
mod history;
mod io;
mod resample;
mod time_stretch;
mod waveform;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use engine::{AuthSignal, Engine, PlayerSignal};
use io::{Input, ProtocolWriter};
use librespot_audio::AudioFetchParams;
use librespot_core::cache::Cache;
use spotify_playback_engine::protocol::{
    AlbumBrowse, ArtistBrowse, ArtistCataloguePage, ArtistReleasePage, Canvas, Command,
    LikedSongsPage, LikedUrisPage, PlaylistBrowse, PlaylistRecommendations, PlaylistRef,
    RadioBrowse, Response, SearchBrowse, TrackCredits, TrackWaveform,
};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use waveform::WaveformService;

const AUDIO_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
const POSITION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// A completed network round-trip (browse or playlist edit) produced off the
/// command loop. The loop turns it into a protocol response; request ids are
/// correlated by the UI, so out-of-order completion is safe.
enum BrowseOutcome {
    Playlists {
        request_id: String,
        result: Result<Vec<PlaylistRef>, String>,
    },
    Playlist {
        request_id: String,
        result: Result<PlaylistBrowse, String>,
    },
    Radio {
        request_id: String,
        result: Result<RadioBrowse, String>,
    },
    PlaylistRecommendations {
        request_id: String,
        result: Result<PlaylistRecommendations, String>,
    },
    Album {
        request_id: String,
        result: Result<AlbumBrowse, String>,
    },
    Artist {
        request_id: String,
        result: Result<ArtistBrowse, String>,
    },
    ArtistReleases {
        request_id: String,
        result: Result<ArtistReleasePage, String>,
    },
    ArtistCatalogue {
        request_id: String,
        result: Result<ArtistCataloguePage, String>,
    },
    LikedSongs {
        request_id: String,
        result: Result<LikedSongsPage, String>,
    },
    LikedUris {
        request_id: String,
        result: Result<LikedUrisPage, String>,
    },
    TrackCredits {
        request_id: String,
        result: Result<TrackCredits, String>,
    },
    Canvas {
        request_id: String,
        result: Result<Option<Canvas>, String>,
    },
    Search {
        request_id: String,
        result: Result<SearchBrowse, String>,
    },
    CreatePlaylist {
        request_id: String,
        result: Result<PlaylistRef, String>,
    },
    VoidEdit {
        request_id: String,
        kind: &'static str,
        result: Result<(), String>,
    },
}

/// Audio fetch tuning at engine startup (before any playback).
///
/// `read_ahead_during_playback` shrinks the streaming buffer from the
/// 5-second default so play/pause/seek feel immediate, at the cost of
/// jitter headroom. It is the operative knob for audible stalls: once a
/// CDN range fetch fails (observed: hyper `IncompleteMessage` / DataLoss
/// mid-body connection drops, librespot-audio receive_data), the player's
/// blocked read is woken and the range re-requested, but the playout
/// buffer drains while the retry cycle (failure detection + reconnect +
/// refill, typically 0.5-3 s) runs. 2 seconds underran before the retry
/// landed (audible ~2-5 s control/playback stalls); 3 seconds covers a
/// full retry cycle, so a single fast-failing fetch no longer goes
/// audible. The buffer is only used to cover jitter: it does not delay
/// initial playback start (the first packet read is served by the
/// initial 64 KiB fetch) and only adds ~one round trip to seek landings.
///
/// `download_timeout` drops from the 8-second default so a *silently*
/// stalled stream (accepted connection that never delivers) is detected
/// and surfaced quickly. It is intentionally NOT raised along with the
/// read-ahead: the observed failure mode fails fast (connection reset,
/// `IncompleteMessage`), which never reaches this timeout — every failed
/// range request notifies the waiting reader and re-arms the window, so
/// retries are not serialized through it. A longer timeout would only
/// delay the give-up on dead-hang connections, where 3 seconds is
/// already generous (any delivered chunk re-arms the window; 64 KiB at
/// 320 kbps arrives in ~1.6 s).
const AUDIO_READ_AHEAD_DURING_PLAYBACK: Duration = Duration::from_secs(3);
const AUDIO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(3);

/// The tuned [`AudioFetchParams`]. Pure so tests can pin the tuning
/// values; the process-wide `OnceLock` is only touched by
/// [`configure_audio_fetch`].
fn audio_fetch_params() -> AudioFetchParams {
    AudioFetchParams {
        read_ahead_during_playback: AUDIO_READ_AHEAD_DURING_PLAYBACK,
        download_timeout: AUDIO_DOWNLOAD_TIMEOUT,
        ..AudioFetchParams::default()
    }
}

fn configure_audio_fetch() {
    // The process sets this exactly once at startup; a second call (tests)
    // is a no-op by design.
    let _ = AudioFetchParams::set(audio_fetch_params());
}

fn main() -> ExitCode {
    let (state_directory, log_file, audio_cache_limit_bytes, normalisation) =
        match parse_arguments(std::env::args_os().skip(1).collect()) {
            Ok(arguments) => arguments,
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
    // Installed after the logger: every panic (any thread, including
    // librespot's player thread) is appended to the engine log file so a
    // future death is diagnosable even when stderr is gone.
    install_panic_hook(log_file);

    let (cache, temporary_directory, credentials_file) =
        match create_state(&state_directory, audio_cache_limit_bytes) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("SpotifyPlaybackEngine: {error}");
                return ExitCode::FAILURE;
            }
        };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
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
        state_directory,
        normalisation,
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
    state_directory: PathBuf,
    normalisation: bool,
) -> Result<(), String> {
    let (input_sender, mut input_receiver) = mpsc::unbounded_channel();
    let (auth_sender, mut auth_receiver) = mpsc::unbounded_channel::<AuthSignal>();
    let (player_sender, mut player_receiver) = mpsc::unbounded_channel::<PlayerSignal>();
    let (browse_sender, mut browse_receiver) = mpsc::unbounded_channel::<BrowseOutcome>();
    let (audio_sender, mut audio_receiver) = mpsc::unbounded_channel();
    io::spawn_input_reader(input_sender);

    configure_audio_fetch();
    audio::install_signal_sender(audio_sender);
    let (mut waveform_service, mut waveform_receiver) =
        WaveformService::new(cache.clone(), &state_directory)?;

    let mut engine = Engine::new(
        writer,
        cache,
        temporary_directory,
        credentials_file,
        state_directory,
        normalisation,
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
                                let cancelled: Result<TrackWaveform, String> =
                                    Err("waveform service is shutting down".to_owned());
                                for pending_id in waveform_service.shutdown() {
                                    engine.send_browse_response(
                                        &pending_id,
                                        "get_track_waveform",
                                        &cancelled,
                                    )?;
                                }
                                let success = Ok(true);
                                engine.send_response(&request_id, &success)?;
                                engine.shutdown();
                                break;
                            }
                            Command::GetHistory => {
                                let result = engine.history();
                                engine.send_browse_response(&request_id, "history", &result)?;
                            }
                            Command::ClearHistory => {
                                let result = engine.clear_history();
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::GetTrackWaveform { track_id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        waveform_service.request(request_id, track_id, session);
                                    }
                                    Err(error) => {
                                        let result: Result<TrackWaveform, String> = Err(error);
                                        engine.send_browse_response(
                                            &request_id,
                                            "get_track_waveform",
                                            &result,
                                        )?;
                                    }
                                }
                            }
                            Command::CancelTrackWaveform { track_id } => {
                                let cancelled: Result<TrackWaveform, String> =
                                    Err("waveform request was cancelled".to_owned());
                                for pending_id in waveform_service.cancel(&track_id) {
                                    engine.send_browse_response(
                                        &pending_id,
                                        "get_track_waveform",
                                        &cancelled,
                                    )?;
                                }
                                let success = Ok(true);
                                engine.send_response(&request_id, &success)?;
                            }
                            Command::GetTrackEdit { track_id, playlist_id } => {
                                let result: Result<_, String> =
                                    Ok(engine.track_edit_status(&track_id, playlist_id.as_deref()));
                                engine.send_browse_response(&request_id, "get_track_edit", &result)?;
                            }
                            Command::SaveTrackEdit {
                                track_id,
                                duration_ms,
                                cuts,
                                loop_range,
                            } => {
                                let result =
                                    engine.save_track_edit(track_id, duration_ms, cuts, loop_range);
                                engine.send_browse_response(&request_id, "save_track_edit", &result)?;
                            }
                            Command::DeleteTrackEdit { track_id } => {
                                let result = engine
                                    .delete_track_edit(&track_id)
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::SetPlaylistTrackEditEnabled {
                                playlist_id,
                                track_id,
                                enabled,
                            } => {
                                let result = engine
                                    .set_playlist_track_edit_enabled(
                                        &playlist_id,
                                        &track_id,
                                        enabled,
                                    )
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                            }
                            Command::SetPlaylistTrackExcluded {
                                playlist_id,
                                track_id,
                                excluded,
                            } => {
                                let result = engine
                                    .set_playlist_track_excluded(
                                        &playlist_id,
                                        &track_id,
                                        excluded,
                                    )
                                    .map(|()| true);
                                engine.send_response(&request_id, &result)?;
                            }
                            // Browse and edit commands run their network work
                            // off the loop: the session clone is handed to a
                            // spawned task whose outcome is dispatched from
                            // the browse_receiver arm below. Playback
                            // commands (volume/pause/seek/next/previous)
                            // therefore stay prompt even while a slow browse
                            // is in flight. When no session is available the
                            // error is answered immediately through the same
                            // outcome channel.
                            Command::BrowsePlaylists { length } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::playlists_browse(&session, length).await;
                                            let _ = sender.send(BrowseOutcome::Playlists { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Playlists { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowsePlaylist { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::playlist_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::Playlist { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Playlist { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseRadio { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::radio_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::Radio { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Radio { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowsePlaylistRecommendations { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::playlist_recommendations_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::PlaylistRecommendations { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::PlaylistRecommendations {
                                            request_id,
                                            result: Err(error),
                                        });
                                    }
                                }
                            }
                            Command::BrowseAlbum { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::album_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::Album { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Album { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseArtist { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::artist_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::Artist { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Artist { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseArtistReleases { id, release_types, offset, limit } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::artist_releases_browse(
                                                &session,
                                                &id,
                                                &release_types,
                                                offset,
                                                limit,
                                            )
                                            .await;
                                            let _ = sender.send(BrowseOutcome::ArtistReleases { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::ArtistReleases { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseArtistCatalogue { id, release_types, offset, limit } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::artist_catalogue_browse(
                                                &session,
                                                &id,
                                                &release_types,
                                                offset,
                                                limit,
                                            )
                                            .await;
                                            let _ = sender.send(BrowseOutcome::ArtistCatalogue { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::ArtistCatalogue { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseLikedSongs { cursor } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::liked_songs_browse(
                                                &session,
                                                cursor.as_deref(),
                                            )
                                            .await;
                                            let _ = sender.send(BrowseOutcome::LikedSongs {
                                                request_id,
                                                result,
                                            });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::LikedSongs {
                                            request_id,
                                            result: Err(error),
                                        });
                                    }
                                }
                            }
                            Command::BrowseLikedUris { cursor } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::liked_song_uris_browse(
                                                &session,
                                                cursor.as_deref(),
                                            )
                                            .await;
                                            let _ = sender.send(BrowseOutcome::LikedUris {
                                                request_id,
                                                result,
                                            });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::LikedUris {
                                            request_id,
                                            result: Err(error),
                                        });
                                    }
                                }
                            }
                            Command::BrowseTrackCredits { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::track_credits_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::TrackCredits { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::TrackCredits { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::BrowseCanvas { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::canvas_browse(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::Canvas { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Canvas {
                                            request_id,
                                            result: Err(error),
                                        });
                                    }
                                }
                            }
                            Command::BrowseSearch { query, limit } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = browse::search_browse(&session, &query, limit).await;
                                            let _ = sender.send(BrowseOutcome::Search { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::Search { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::EditCreatePlaylist { name } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::create_playlist(&session, &name).await;
                                            let _ = sender.send(BrowseOutcome::CreatePlaylist { request_id, result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::CreatePlaylist { request_id, result: Err(error) });
                                    }
                                }
                            }
                            Command::EditRenamePlaylist { id, name } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::rename_playlist(&session, &id, &name).await;
                                            let _ = sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_rename_playlist", result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_rename_playlist", result: Err(error) });
                                    }
                                }
                            }
                            Command::EditDeletePlaylist { id } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::delete_playlist(&session, &id).await;
                                            let _ = sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_delete_playlist", result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_delete_playlist", result: Err(error) });
                                    }
                                }
                            }
                            Command::EditAddPlaylistTracks { id, uris } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::add_tracks(&session, &id, &uris).await;
                                            let _ = sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_add_playlist_tracks", result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_add_playlist_tracks", result: Err(error) });
                                    }
                                }
                            }
                            Command::EditRemovePlaylistTracks { id, uris } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::remove_tracks(&session, &id, &uris).await;
                                            let _ = sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_remove_playlist_tracks", result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_remove_playlist_tracks", result: Err(error) });
                                    }
                                }
                            }
                            Command::EditReorderPlaylistTracks { id, from, to } => {
                                match engine.browse_session_clone() {
                                    Ok(session) => {
                                        let sender = browse_sender.clone();
                                        tokio::spawn(async move {
                                            let result = edits::reorder_tracks(&session, &id, from, to).await;
                                            let _ = sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_reorder_playlist_tracks", result });
                                        });
                                    }
                                    Err(error) => {
                                        let _ = browse_sender.send(BrowseOutcome::VoidEdit { request_id, kind: "edit_reorder_playlist_tracks", result: Err(error) });
                                    }
                                }
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
                        waveform_service.shutdown();
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
            signal = audio_receiver.recv() => {
                if let Some(signal) = signal {
                    if engine.on_audio_signal(signal) {
                        engine.emit_state()?;
                    }
                }
            }
            outcome = browse_receiver.recv() => {
                if let Some(outcome) = outcome {
                    match outcome {
                        BrowseOutcome::Playlists { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_playlists", &result)?;
                        }
                        BrowseOutcome::Playlist {
                            request_id,
                            result,
                        } => {
                            let result = result.and_then(|mut browse| {
                                browse.excluded_track_ids =
                                    engine.playlist_excluded_track_ids(&browse.id)?;
                                Ok(browse)
                            });
                            engine.send_browse_response(&request_id, "browse_playlist", &result)?;
                        }
                        BrowseOutcome::Radio { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_radio", &result)?;
                        }
                        BrowseOutcome::PlaylistRecommendations { request_id, result } => {
                            engine.send_browse_response(
                                &request_id,
                                "browse_playlist_recommendations",
                                &result,
                            )?;
                        }
                        BrowseOutcome::Album { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_album", &result)?;
                        }
                        BrowseOutcome::Artist { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_artist", &result)?;
                        }
                        BrowseOutcome::ArtistReleases { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_artist_releases", &result)?;
                        }
                        BrowseOutcome::ArtistCatalogue { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_artist_catalogue", &result)?;
                        }
                        BrowseOutcome::LikedSongs { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_liked_songs", &result)?;
                        }
                        BrowseOutcome::LikedUris { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_liked_uris", &result)?;
                        }
                        BrowseOutcome::TrackCredits { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_track_credits", &result)?;
                        }
                        BrowseOutcome::Canvas { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_canvas", &result)?;
                        }
                        BrowseOutcome::Search { request_id, result } => {
                            engine.send_browse_response(&request_id, "browse_search", &result)?;
                        }
                        BrowseOutcome::CreatePlaylist { request_id, result } => {
                            engine.send_browse_response(&request_id, "edit_create_playlist", &result)?;
                        }
                        BrowseOutcome::VoidEdit { request_id, kind, result } => {
                            engine.send_edit_response(&request_id, kind, &result)?;
                        }
                    }
                }
            }
            outcome = waveform_receiver.recv() => {
                if let Some(outcome) = outcome {
                    if let Some((request_ids, result)) = waveform_service.complete(outcome) {
                        for request_id in request_ids {
                            engine.send_browse_response(
                                &request_id,
                                "get_track_waveform",
                                &result,
                            )?;
                        }
                    }
                }
            }
            _ = position_heartbeat.tick() => {
                // Catches a session librespot invalidated on its own, which is
                // otherwise invisible until a track refuses to load.
                if engine.tick_session_health(&auth_sender) {
                    engine.emit_state()?;
                }
                // A track finishes caching mid-listen, so the download mark has
                // to be able to appear without reopening the list it is in.
                if engine.refresh_cached_marks() {
                    engine.emit_state()?;
                }
                if engine.tick_position() {
                    // Scalar playhead sync: O(1) regardless of queue size.
                    // Real changes (track, queue, volume, play/pause, ...)
                    // still emit the full state through the other arms.
                    engine.emit_position()?;
                }
            }
        }
    }
    Ok(())
}

/// Parses `--state-dir <path>` (required), `--log-file <path>` (optional,
/// the diagnostic log the parent redirects stderr to; the panic hook appends
/// there too), `--audio-cache-limit-mb <n>` (optional) and
/// `--normalisation <true|false>` (optional, default off). Accepts the
/// arguments in any order and tolerates extra pairs, so older launchers that
/// only pass `--state-dir` keep working.
fn parse_arguments(
    arguments: Vec<OsString>,
) -> Result<(PathBuf, Option<PathBuf>, Option<u64>, bool), String> {
    let mut state_directory: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut audio_cache_limit_bytes = Some(AUDIO_CACHE_LIMIT_BYTES);
    let mut audio_cache_limit_seen = false;
    let mut normalisation = false;
    let mut index = 0usize;
    while index < arguments.len() {
        let name = arguments[index].to_string_lossy().into_owned();
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{name} requires a value"))?;
        match name.as_str() {
            "--state-dir" => {
                if state_directory.is_some() {
                    return Err("--state-dir given more than once".to_owned());
                }
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("--state-dir must be an absolute path".to_owned());
                }
                state_directory = Some(path);
            }
            "--log-file" => {
                log_file = Some(PathBuf::from(value));
            }
            "--audio-cache-limit-mb" => {
                if audio_cache_limit_seen {
                    return Err("--audio-cache-limit-mb given more than once".to_owned());
                }
                audio_cache_limit_seen = true;
                let mb = value.to_string_lossy().parse::<u64>().map_err(|_| {
                    "--audio-cache-limit-mb must be a non-negative integer".to_owned()
                })?;
                audio_cache_limit_bytes = if mb == 0 {
                    None
                } else {
                    Some(
                        mb.checked_mul(1024 * 1024)
                            .ok_or_else(|| "--audio-cache-limit-mb is too large".to_owned())?,
                    )
                };
            }
            "--normalisation" => {
                normalisation = match value.to_string_lossy().as_ref() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "--normalisation must be \"true\" or \"false\", got {other}"
                        ));
                    }
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 2;
    }
    let state_directory = state_directory.ok_or_else(|| {
        "usage: SpotifyPlaybackEngine.exe --state-dir <absolute-app-owned-path>".to_owned()
    })?;
    Ok((
        state_directory,
        log_file,
        audio_cache_limit_bytes,
        normalisation,
    ))
}

/// One panic report: thread, payload, location, and a captured backtrace.
/// Pure so the hook logic is unit-testable without panicking.
fn format_panic_report(thread: &str, info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "Box<dyn Any>".to_owned()
    };
    let location = info
        .location()
        .map(|location| format!("{location}"))
        .unwrap_or_else(|| "unknown location".to_owned());
    let backtrace = std::backtrace::Backtrace::capture();
    format!("thread '{thread}' panicked at {location}:\n{payload}\nstack backtrace:\n{backtrace}")
}

/// Appends one panic report to the engine log file. Called from the panic
/// hook, so it must never panic itself: every fallible step is ignored.
fn append_panic_to_log(path: &std::path::Path, report: &str) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{report}");
        let _ = file.flush();
    }
}

/// Installs the process panic hook: every panic in any thread prints the
/// usual stderr report (so the parent's redirected stderr still shows it)
/// and appends a copy to the engine log file. The hook never panics; a
/// log-file write failure is silently ignored.
fn install_panic_hook(log_file: Option<PathBuf>) {
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_owned();
        let report = format_panic_report(&thread, info);
        eprintln!("{report}");
        if let Some(path) = &log_file {
            append_panic_to_log(path, &report);
        }
    }));
}

/// Marker-file version of the audio cache layout. When the marker is absent
/// or stale, every cached audio file is dropped once (a one-time cleanup of
/// corrupt/truncated entries from earlier builds, which decode-fail with
/// Symphonia "end of stream" and wedge next/prev/shuffle) and the new layout
/// is recorded.
const AUDIO_CACHE_VERSION: &str = "2";

/// Brings the audio cache directory up to the current layout version: on a
/// version change the directory is wiped (files and subdirectories) except
/// for the marker itself, then the marker is (re)written. Idempotent and
/// cheap on steady-state starts.
fn version_audio_cache(state_directory: &std::path::Path) -> Result<(), String> {
    let audio = state_directory.join("audio");
    std::fs::create_dir_all(&audio)
        .map_err(|error| format!("could not create audio cache directory: {error}"))?;
    let marker = audio.join("cache-version");
    let expected = format!("{AUDIO_CACHE_VERSION}\n");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(expected.as_str()) {
        for entry in std::fs::read_dir(&audio)
            .map_err(|error| format!("could not read audio cache directory: {error}"))?
            .flatten()
        {
            let path = entry.path();
            if path == marker {
                continue;
            }
            let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let result = if is_directory {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(error) = result {
                eprintln!(
                    "could not clear stale audio cache entry {}: {error}",
                    path.display()
                );
            }
        }
        std::fs::write(&marker, expected)
            .map_err(|error| format!("could not write the audio cache version marker: {error}"))?;
    }
    Ok(())
}

fn create_state(
    state_directory: &std::path::Path,
    audio_cache_limit_bytes: Option<u64>,
) -> Result<(Cache, PathBuf, PathBuf), String> {
    std::fs::create_dir_all(state_directory)
        .map_err(|error| format!("could not create state directory: {error}"))?;
    let credentials = state_directory.join("credentials");
    let volume = state_directory.join("volume");
    let audio = state_directory.join("audio");
    let temporary = state_directory.join("tmp");
    std::fs::create_dir_all(&temporary)
        .map_err(|error| format!("could not create temporary directory: {error}"))?;
    sweep_temporary_directory(&temporary);
    version_audio_cache(state_directory)?;
    let cache = Cache::new(
        Some(credentials),
        Some(volume),
        Some(audio),
        audio_cache_limit_bytes,
    )
    .map_err(|error| format!("could not initialize the app-owned cache: {error}"))?;
    // librespot stores credentials as credentials.json inside the credentials
    // directory; `logout` removes it (Cache offers no removal API).
    let credentials_file = state_directory.join("credentials").join("credentials.json");
    Ok((cache, temporary, credentials_file))
}

/// Deletes everything left in the download scratch directory.
///
/// librespot streams each track into a temporary file here and only moves it
/// into the audio cache once it is complete, so anything abandoned part-way —
/// a skip, a stall, a quit mid-fetch — stays behind forever. Nothing else
/// removes them, and they do not count against the audio cache's size limit,
/// so the directory grows without bound: 94 MB of orphans accumulated in two
/// days of ordinary use on the reference machine, all of it invisible to `ls`
/// because librespot names them `.tmpXXXXXX`.
///
/// Startup is the one moment this is unconditionally safe: the engine has not
/// begun fetching, so every file present is by definition abandoned by a
/// previous run. Failures are ignored — a file that cannot be removed is a
/// wasted megabyte, not a reason to refuse to start.
fn sweep_temporary_directory(temporary: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(temporary) else {
        return;
    };
    let mut removed = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            bytes += size;
        }
    }
    if removed > 0 {
        eprintln!(
            "cleared {removed} abandoned download(s) from the scratch directory ({:.1} MB)",
            bytes as f64 / (1024.0 * 1024.0)
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct ScratchDir {
        directory: PathBuf,
    }

    impl ScratchDir {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "sr_engine_cache_test_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            Self { directory }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn audio_cache_version_bump_clears_stale_entries_once() {
        let scratch = ScratchDir::new();
        // An old build's layout: corrupt audio files plus a format
        // subdirectory, and no version marker.
        let audio = scratch.directory.join("audio");
        fs::create_dir_all(audio.join("ab")).expect("fixture subdir");
        fs::write(audio.join("ab").join("deadbeef"), b"truncated").expect("fixture file");
        fs::write(audio.join("0123456789abcdef"), b"junk").expect("fixture file");

        version_audio_cache(&scratch.directory).expect("version bump succeeds");
        assert!(
            !audio.join("0123456789abcdef").exists(),
            "stale file cleared"
        );
        assert!(!audio.join("ab").exists(), "stale subdirectory cleared");
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker written"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );

        // Steady state: a matching marker leaves new entries untouched.
        fs::write(audio.join("fresh-entry"), b"data").expect("fresh entry");
        version_audio_cache(&scratch.directory).expect("steady-state start");
        assert!(audio.join("fresh-entry").exists(), "fresh entries survive");

        // A later layout version clears everything again (one-time per bump).
        fs::write(audio.join("cache-version"), "1\n").expect("stale marker");
        version_audio_cache(&scratch.directory).expect("second bump");
        assert!(
            !audio.join("fresh-entry").exists(),
            "old-layout entries cleared"
        );
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker rewritten"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );
    }

    #[test]
    fn audio_cache_marker_is_matched_exactly_not_prefixwise() {
        let scratch = ScratchDir::new();
        // The marker is compared with exact content (`"2\n"`), so any
        // deviation — no trailing newline, CRLF, extra text — counts as a
        // different layout and wipes the cache. The engine always writes the
        // exact form, so this only fires if something else touched the file;
        // wiping is the safe failure mode (worst case: one refetch).
        let audio = scratch.directory.join("audio");
        fs::create_dir_all(&audio).expect("fixture dir");
        fs::write(audio.join("cache-version"), AUDIO_CACHE_VERSION).expect("no-newline marker");
        fs::write(audio.join("fresh-entry"), b"data").expect("fixture entry");

        version_audio_cache(&scratch.directory).expect("mismatched marker wipes");
        assert!(
            !audio.join("fresh-entry").exists(),
            "a no-newline marker must be treated as stale"
        );
        assert_eq!(
            fs::read_to_string(audio.join("cache-version")).expect("marker rewritten"),
            format!("{AUDIO_CACHE_VERSION}\n")
        );
    }

    #[test]
    fn audio_fetch_params_pin_the_tuned_headroom_and_timeout() {
        let params = audio_fetch_params();
        // Tuning decision (see AUDIO_READ_AHEAD_DURING_PLAYBACK): the buffer
        // must cover one full fetch-failure retry cycle (fast-failing CDN
        // errors, observed as hyper IncompleteMessage/DataLoss) or the
        // underrun becomes an audible multi-second stall.
        assert_eq!(
            params.read_ahead_during_playback,
            Duration::from_secs(3),
            "buffer headroom covers a fetch retry cycle"
        );
        // The timeout only caps *silent* stalls: fast failures re-arm the
        // wait window via the download-status condvar, so they never reach
        // it, and a shorter give-up is strictly better for dead hangs.
        assert_eq!(
            params.download_timeout,
            Duration::from_secs(3),
            "dead-hang detection must stay quick"
        );
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(|arg| OsString::from(*arg)).collect()
    }

    #[test]
    fn parse_arguments_requires_state_dir_and_accepts_an_optional_log_file() {
        let (state, log, cache_limit, normalisation) =
            parse_arguments(os(&["--state-dir", "C:\\sr\\engine"])).expect("state dir alone");
        assert_eq!(state, PathBuf::from("C:\\sr\\engine"));
        assert!(log.is_none(), "log file is optional");
        assert_eq!(cache_limit, Some(AUDIO_CACHE_LIMIT_BYTES));
        assert!(!normalisation, "normalisation defaults to off");

        let (state, log, cache_limit, _) = parse_arguments(os(&[
            "--state-dir",
            "C:\\sr\\engine",
            "--log-file",
            "C:\\sr\\logs\\playback_engine.log",
            "--audio-cache-limit-mb",
            "4096",
        ]))
        .expect("both flags");
        assert_eq!(state, PathBuf::from("C:\\sr\\engine"));
        assert_eq!(
            log,
            Some(PathBuf::from("C:\\sr\\logs\\playback_engine.log"))
        );
        assert_eq!(cache_limit, Some(4096 * 1024 * 1024));

        // Flag order must not matter.
        let (state, log, cache_limit, _) = parse_arguments(os(&[
            "--audio-cache-limit-mb",
            "0",
            "--log-file",
            "C:\\sr\\logs\\playback_engine.log",
            "--state-dir",
            "C:\\sr\\engine",
        ]))
        .expect("log file first");
        assert_eq!(state, PathBuf::from("C:\\sr\\engine"));
        assert!(log.is_some());
        assert_eq!(cache_limit, None, "zero selects an unlimited cache");
    }

    #[test]
    fn parse_arguments_reads_the_normalisation_flag() {
        let (_, _, _, normalisation) = parse_arguments(os(&[
            "--state-dir",
            "C:\\sr\\engine",
            "--normalisation",
            "true",
        ]))
        .expect("normalisation on");
        assert!(normalisation);

        let (_, _, _, normalisation) = parse_arguments(os(&[
            "--state-dir",
            "C:\\sr\\engine",
            "--normalisation",
            "false",
        ]))
        .expect("explicit off");
        assert!(!normalisation);

        assert!(
            parse_arguments(os(&[
                "--state-dir",
                "C:\\sr\\engine",
                "--normalisation",
                "maybe",
            ]))
            .is_err(),
            "a non-boolean value is rejected rather than silently ignored"
        );
    }

    #[test]
    fn parse_arguments_rejects_relative_state_dir_unknown_flags_and_duplicates() {
        assert!(parse_arguments(os(&[])).is_err(), "state dir required");
        assert!(
            parse_arguments(os(&["--state-dir"])).is_err(),
            "missing value rejected"
        );
        assert!(
            parse_arguments(os(&["--state-dir", "relative\\engine"])).is_err(),
            "relative state dir rejected"
        );
        assert!(
            parse_arguments(os(&["--state-dir", "C:\\a", "--bogus", "x"])).is_err(),
            "unknown flag rejected"
        );
        assert!(
            parse_arguments(os(&["--state-dir", "C:\\a", "--state-dir", "C:\\b",])).is_err(),
            "duplicate state dir rejected"
        );
        assert!(
            parse_arguments(os(&[
                "--state-dir",
                "C:\\a",
                "--audio-cache-limit-mb",
                "wat",
            ]))
            .is_err(),
            "cache limit must be numeric"
        );
        assert!(
            parse_arguments(os(&[
                "--state-dir",
                "C:\\a",
                "--audio-cache-limit-mb",
                "1024",
                "--audio-cache-limit-mb",
                "2048",
            ]))
            .is_err(),
            "duplicate cache limit rejected"
        );
    }

    /// The panic hook is process-global: tests that install it must not run
    /// concurrently with each other (or with tests that intentionally panic,
    /// which would hit a half-installed hook).
    static PANIC_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn panic_report_names_the_thread_payload_and_location() {
        let _guard = PANIC_HOOK_TEST_LOCK.lock().expect("hook test lock");
        // Capture the report produced for a real panic, then assert on it
        // after the unwind completes (asserting inside the hook would
        // double-panic and abort the process).
        let report = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let captured = {
            let report = std::sync::Arc::clone(&report);
            std::panic::catch_unwind(|| {
                std::panic::set_hook(Box::new(move |info| {
                    let built = format_panic_report("test-thread", info);
                    *report.lock().expect("report lock") = Some(built);
                }));
                panic!("test payload");
            })
        };
        let _ = std::panic::take_hook(); // drop the test hook
        assert!(captured.is_err());
        let report = report
            .lock()
            .expect("report lock")
            .take()
            .expect("hook ran");
        assert!(report.contains("panicked at"));
        assert!(report.contains("test payload"));
        assert!(report.contains("stack backtrace"));
        assert!(report.contains("main.rs"), "location in the report");
    }

    #[test]
    fn panic_hook_appends_the_report_to_the_engine_log_file() {
        let _guard = PANIC_HOOK_TEST_LOCK.lock().expect("hook test lock");
        let scratch = ScratchDir::new();
        // Mirror production: the app creates the log directory before the
        // engine starts (the hook opens the file with create(true), which
        // cannot create missing parent directories).
        fs::create_dir_all(&scratch.directory).expect("scratch dir created");
        let log = scratch.directory.join("playback_engine.log");
        let previous = std::panic::take_hook();
        install_panic_hook(Some(log.clone()));
        let result = std::panic::catch_unwind(|| panic!("diagnosable failure"));
        std::panic::set_hook(previous);
        assert!(result.is_err(), "the panic still unwinds normally");

        let contents = fs::read_to_string(&log).expect("panic report appended");
        assert!(contents.contains("panicked at"), "report header present");
        assert!(
            contents.contains("diagnosable failure"),
            "payload present: {contents}"
        );
        assert!(
            contents.contains("panic_hook_appends_the_report_to_the_engine_log_file"),
            "call-site location present"
        );

        // A second panic appends, never overwrites.
        let previous = std::panic::take_hook();
        install_panic_hook(Some(log.clone()));
        let result = std::panic::catch_unwind(|| panic!("second failure"));
        std::panic::set_hook(previous);
        assert!(result.is_err());
        let contents = fs::read_to_string(&log).expect("second report appended");
        assert_eq!(
            contents.matches("panicked at").count(),
            2,
            "both reports kept"
        );
    }
}
