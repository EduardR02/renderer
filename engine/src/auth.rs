use std::sync::{Arc, mpsc as std_mpsc};
use std::time::Duration;

use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::Session;
use librespot_oauth::OAuthClientBuilder;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, Bitrate, PlayerConfig};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{Player, PlayerEventChannel};

const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:5588/login";
const OAUTH_SCOPES: &[&str] = &["streaming", "user-read-private"];
const AUDIO_START_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PlaybackHandles {
    pub player: Arc<Player>,
    pub events: PlayerEventChannel,
    pub mixer: Arc<SoftMixer>,
    pub session: Session,
    pub volume_percent: u8,
}

pub async fn authenticate(
    cache: Cache,
    temporary_directory: std::path::PathBuf,
) -> Result<PlaybackHandles, String> {
    let mut session_config = SessionConfig::default();
    session_config.tmp_dir = temporary_directory;

    if let Some(credentials) = cache.credentials() {
        let session = Session::new(session_config.clone(), Some(cache.clone()));
        match session.connect(credentials, false).await {
            Ok(()) => return create_playback(session, cache).await,
            Err(error) => {
                eprintln!("cached Spotify credentials were rejected: {error}");
                session.shutdown();
            }
        }
    }

    let client_id = session_config.client_id.clone();
    let token = tokio::task::spawn_blocking(move || {
        let client = OAuthClientBuilder::new(&client_id, OAUTH_REDIRECT_URI, OAUTH_SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message(
                "SpotifyPlaybackEngine is authenticated. You can close this browser tab.",
            )
            .build()
            .map_err(|error| format!("could not initialize Spotify OAuth: {error}"))?;
        client
            .get_access_token()
            .map_err(|error| format!("Spotify OAuth failed: {error}"))
    })
    .await
    .map_err(|error| format!("Spotify OAuth worker failed: {error}"))??;

    let session = Session::new(session_config, Some(cache.clone()));
    if let Err(error) = session
        .connect(Credentials::with_access_token(token.access_token), true)
        .await
    {
        session.shutdown();
        return Err(format!("Spotify authentication failed: {error}"));
    }
    create_playback(session, cache).await
}

/// Player configuration for the standalone engine.
///
/// `position_update_interval` is intentionally left at its `None` default: the
/// UI projects position locally between engine events, and the engine emits a
/// single position heartbeat every two seconds while playing. librespot must
/// not stream 250 ms `PositionChanged` events.
fn player_config() -> PlayerConfig {
    PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        normalisation: false,
        ..PlayerConfig::default()
    }
}

async fn create_playback(session: Session, cache: Cache) -> Result<PlaybackHandles, String> {
    let mixer = Arc::new(
        SoftMixer::open(MixerConfig::default())
            .map_err(|error| format!("could not initialize software volume: {error}"))?,
    );
    let cached_volume = cache.volume().unwrap_or(u16::MAX / 2);
    mixer.set_volume(cached_volume);

    let backend = audio_backend::find(Some("rodio".to_owned()))
        .ok_or_else(|| "the rodio audio backend is not compiled in".to_owned())?;
    let (audio_ready_tx, audio_ready_rx) = std_mpsc::sync_channel(1);
    let config = player_config();
    let player = Player::new(config, session.clone(), mixer.get_soft_volume(), move || {
        let sink = backend(None, AudioFormat::S16);
        let _ = audio_ready_tx.send(());
        sink
    });

    let audio_result = tokio::task::spawn_blocking(move || {
        audio_ready_rx
            .recv_timeout(AUDIO_START_TIMEOUT)
            .map_err(|error| format!("WASAPI/rodio output did not initialize: {error}"))
    })
    .await
    .map_err(|error| format!("audio initialization worker failed: {error}"))?;
    if let Err(error) = audio_result {
        session.shutdown();
        return Err(error);
    }
    if player.is_invalid() {
        session.shutdown();
        return Err("WASAPI/rodio player terminated during initialization".to_owned());
    }

    let events = player.get_player_event_channel();
    player.emit_volume_changed_event(cached_volume);
    Ok(PlaybackHandles {
        player,
        events,
        mixer,
        session,
        volume_percent: volume_to_percent(cached_volume),
    })
}

pub fn percent_to_volume(percent: u8) -> u16 {
    ((u32::from(percent) * u32::from(u16::MAX) + 50) / 100) as u16
}

fn volume_to_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

#[cfg(test)]
mod tests {
    use super::player_config;

    #[test]
    fn player_config_does_not_poll_position() {
        // Event cadence: the engine emits state on transitions plus a 2 s
        // position heartbeat while playing. librespot must not be configured
        // to stream a 250 ms PositionChanged event for the UI to poll.
        assert!(player_config().position_update_interval.is_none());
    }
}
