use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::Session;
use librespot_playback::config::{
    AudioFormat, Bitrate, NormalisationMethod, NormalisationType, PlayerConfig, VolumeCtrl,
};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot_playback::player::{Player, PlayerEventChannel};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use url::Url;

const OAUTH_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const OAUTH_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:5588/login";
const OAUTH_SCOPES: &[&str] = &["streaming", "user-read-private"];
const OAUTH_SUCCESS_MESSAGE: &str =
    "SpotifyPlaybackEngine is authenticated. You can close this browser tab.";
/// An abandoned login attempt (browser never redirected) must not wedge the
/// engine in `Authenticating` forever; after this long the flow fails and the
/// engine returns to `NeedsLogin` with a fresh URL.
const OAUTH_LISTENER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const AUDIO_START_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PlaybackHandles {
    pub player: Arc<Player>,
    pub events: PlayerEventChannel,
    pub mixer: Arc<SoftMixer>,
    pub session: Session,
    pub volume_percent: u8,
}

/// A prepared OAuth authorization-code + PKCE attempt. The authorize URL is
/// generated up front (fresh CSRF state and PKCE challenge per attempt) so the
/// engine can publish it in its `needs_login` state before the flow runs; the
/// client and verifier are kept so the `login` command completes the exact
/// attempt whose URL the UI opened.
pub struct PendingAuth {
    client: BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    verifier: PkceCodeVerifier,
    pub auth_url: String,
}

/// Builds a fresh OAuth attempt and its authorize URL. Pure local work: no
/// network, no browser, no listener. Each call regenerates the URL (new CSRF
/// state and PKCE challenge), satisfying "regenerated per attempt".
pub fn prepare_oauth() -> Result<PendingAuth, String> {
    let client = basic_client()?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let request_scopes: Vec<Scope> = OAUTH_SCOPES
        .iter()
        .map(|scope| Scope::new((*scope).to_owned()))
        .collect();
    let (auth_url, _csrf) = client
        .authorize_url(CsrfToken::new_random)
        .add_scopes(request_scopes)
        .set_pkce_challenge(pkce_challenge)
        .url();
    Ok(PendingAuth {
        client,
        verifier: pkce_verifier,
        auth_url: auth_url.to_string(),
    })
}

/// Connects with the cached credentials (if any) and creates playback. Fails
/// when no credentials are cached or they are rejected; the caller decides
/// whether that means `NeedsLogin` (the flow is never started implicitly).
pub async fn connect_cached(
    cache: Cache,
    temporary_directory: PathBuf,
    normalisation: Arc<AtomicBool>,
) -> Result<PlaybackHandles, String> {
    let credentials = cache
        .credentials()
        .ok_or_else(|| "no cached Spotify credentials".to_owned())?;
    let mut session_config = SessionConfig::default();
    session_config.tmp_dir = temporary_directory;
    let session = Session::new(session_config, Some(cache.clone()));
    match session.connect(credentials, false).await {
        Ok(()) => create_playback_latest(session, cache, normalisation).await,
        Err(error) => {
            eprintln!("cached Spotify credentials were rejected: {error}");
            session.shutdown();
            Err(format!("cached Spotify credentials were rejected: {error}"))
        }
    }
}

/// Runs the OAuth flow for a prepared attempt: prints the authorize URL (the
/// UI opens it), waits for the loopback callback, exchanges the code, connects
/// the session, and creates playback.
pub async fn complete_oauth(
    cache: Cache,
    temporary_directory: PathBuf,
    pending: PendingAuth,
    normalisation: Arc<AtomicBool>,
) -> Result<PlaybackHandles, String> {
    eprintln!("Browse to: {}", pending.auth_url);
    let access_token = tokio::task::spawn_blocking(move || run_oauth_flow(pending))
        .await
        .map_err(|error| format!("Spotify OAuth worker failed: {error}"))??;

    let mut session_config = SessionConfig::default();
    session_config.tmp_dir = temporary_directory;
    let session = Session::new(session_config, Some(cache.clone()));
    if let Err(error) = session
        .connect(Credentials::with_access_token(access_token), true)
        .await
    {
        session.shutdown();
        return Err(format!("Spotify authentication failed: {error}"));
    }
    create_playback_latest(session, cache, normalisation).await
}

/// Blocking OAuth half of [`complete_oauth`]: loopback listener + token
/// exchange. Runs on the blocking pool so the async engine loop is never
/// stalled by the callback wait.
fn run_oauth_flow(pending: PendingAuth) -> Result<String, String> {
    let code = wait_for_oauth_code()?;
    let http_client = reqwest::blocking::Client::new();
    let response = pending
        .client
        .exchange_code(code)
        .set_pkce_verifier(pending.verifier)
        .request(&http_client)
        .map_err(|error| format!("Spotify OAuth token exchange failed: {error}"))?;
    Ok(response.access_token().secret().to_string())
}

/// Waits for the browser redirect to `OAUTH_REDIRECT_URI` and returns the
/// authorization code from its query string. The listener answers with the
/// success page and terminates after the first callback, mirroring the
/// librespot client; unlike it, a stalled attempt fails after
/// [`OAUTH_LISTENER_TIMEOUT`] instead of blocking forever.
fn wait_for_oauth_code() -> Result<AuthorizationCode, String> {
    let address = oauth_listener_addr()?;
    let listener = TcpListener::bind(address).map_err(|error| {
        format!("could not bind the Spotify OAuth callback listener on {address}: {error}")
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure the OAuth callback listener: {error}"))?;
    let deadline = Instant::now() + OAUTH_LISTENER_TIMEOUT;
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(accepted) => break accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("Spotify login timed out; click Log in to start again".to_owned());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(format!("Spotify OAuth callback failed: {error}")),
        }
    };

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("could not read the Spotify OAuth callback: {error}"))?;
    let request_path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "the Spotify OAuth callback carried no request path".to_owned())?;
    let code = extract_oauth_code(request_path)?;

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
        OAUTH_SUCCESS_MESSAGE.len(),
        OAUTH_SUCCESS_MESSAGE
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| format!("could not answer the Spotify OAuth callback: {error}"))?;
    Ok(code)
}

fn extract_oauth_code(request_path: &str) -> Result<AuthorizationCode, String> {
    let redirect = format!("http://127.0.0.1{request_path}");
    let url = Url::parse(&redirect)
        .map_err(|error| format!("malformed Spotify OAuth callback: {error}"))?;
    url.query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, code)| AuthorizationCode::new(code.into_owned()))
        .ok_or_else(|| "the Spotify OAuth callback carried no authorization code".to_owned())
}

fn oauth_listener_addr() -> Result<SocketAddr, String> {
    let url = Url::parse(OAUTH_REDIRECT_URI)
        .map_err(|error| format!("invalid OAuth redirect URI: {error}"))?;
    url.socket_addrs(|| None)
        .ok()
        .and_then(|mut addresses| addresses.pop())
        .ok_or_else(|| format!("OAuth redirect URI has no listenable socket: {OAUTH_REDIRECT_URI}"))
}

fn basic_client() -> Result<
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    String,
> {
    let client_id = SessionConfig::default().client_id;
    Ok(BasicClient::new(ClientId::new(client_id))
        .set_auth_uri(
            AuthUrl::new(OAUTH_AUTHORIZE_URL.to_owned())
                .map_err(|_| "invalid Spotify OAuth authorize URL".to_owned())?,
        )
        .set_token_uri(
            TokenUrl::new(OAUTH_TOKEN_URL.to_owned())
                .map_err(|_| "invalid Spotify OAuth token URL".to_owned())?,
        )
        .set_redirect_uri(
            RedirectUrl::new(OAUTH_REDIRECT_URI.to_owned())
                .map_err(|_| "invalid Spotify OAuth redirect URI".to_owned())?,
        ))
}

/// Player configuration for the standalone engine.
///
/// `position_update_interval` is intentionally left at its `None` default: the
/// UI projects position locally between engine events, and the engine emits a
/// single position heartbeat every two seconds while playing. librespot must
/// not stream 250 ms `PositionChanged` events.
///
/// Normalisation, when enabled, is deliberately the *basic* kind: one constant
/// per-track gain from Spotify's embedded ReplayGain-style tags, clamped so it
/// can never push a sample past full scale. That makes it pure attenuation —
/// mathematically transparent in this float pipeline — and it never constructs
/// librespot's dynamic limiter, which would shave transients on boosted quiet
/// material. `Track` type keeps every track at the same perceived level.
fn player_config(normalisation: bool) -> PlayerConfig {
    PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        normalisation,
        normalisation_type: NormalisationType::Track,
        normalisation_method: NormalisationMethod::Basic,
        normalisation_pregain_db: 0.0,
        // librespot defaults this to a triangular ditherer, which only ever
        // runs in its fixed-point conversions (`f64_to_s16` and friends). The
        // sink takes the float path, where `Converter::f64_to_f32` is a plain
        // cast and the ditherer is never consulted — but constructing one still
        // logs "Converting with ditherer: tpdf", which reads like the audio is
        // being dithered when nothing of the sort is happening.
        ditherer: None,
        ..PlayerConfig::default()
    }
}

fn mixer_config() -> MixerConfig {
    MixerConfig {
        volume_ctrl: VolumeCtrl::Cubic(60.0),
        ..MixerConfig::default()
    }
}

async fn create_playback_latest(
    session: Session,
    cache: Cache,
    normalisation: Arc<AtomicBool>,
) -> Result<PlaybackHandles, String> {
    loop {
        let enabled = normalisation.load(Ordering::Acquire);
        let handles = match create_playback(session.clone(), cache.clone(), enabled).await {
            Ok(handles) => handles,
            Err(error) => {
                session.shutdown();
                return Err(error);
            }
        };
        if normalisation.load(Ordering::Acquire) == enabled {
            return Ok(handles);
        }
        // The preference changed while the player was opening. Keep the
        // authenticated session and retry only player construction.
        handles.player.stop();
    }
}

pub async fn create_playback(
    session: Session,
    cache: Cache,
    normalisation: bool,
) -> Result<PlaybackHandles, String> {
    let mixer = Arc::new(
        SoftMixer::open(mixer_config())
            .map_err(|error| format!("could not initialize software volume: {error}"))?,
    );
    let cached_volume = cache.volume().unwrap_or(u16::MAX / 2);
    mixer.set_volume(cached_volume);

    let (audio_ready_tx, audio_ready_rx) = std_mpsc::sync_channel(1);
    let config = player_config(normalisation);
    // Volume is applied by the rodio sink (see audio::set_sink_volume), not
    // per decoded packet, so a transport volume change is audible on the
    // next output callback instead of after the write-ahead buffer plays
    // out. The player's volume getter is therefore a no-op (always 1.0);
    // the SoftMixer is kept purely as the volume store/persistence.
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || {
        // Custom immediate-stop rodio sink: pause/stop must not drain the
        // buffered queue before silencing the output.
        // F32, not S16: WASAPI's shared-mode engine is float internally, and
        // the sink's own path is float end to end, so asking cpal for 16-bit
        // would insert an undithered quantisation that nothing downstream
        // wants. The device's mix format is what actually gets opened.
        let sink = crate::audio::open_default_sink(AudioFormat::F32);
        crate::audio::set_sink_volume(cached_volume);
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
        return Err(error);
    }
    if player.is_invalid() {
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
    use super::{
        extract_oauth_code, mixer_config, oauth_listener_addr, player_config, prepare_oauth,
        OAUTH_REDIRECT_URI,
    };
    use librespot_playback::config::VolumeCtrl;
    use librespot_playback::mixer::softmixer::SoftMixer;
    use librespot_playback::mixer::Mixer;

    #[test]
    fn mixer_uses_the_same_cubic_sixty_db_curve_as_the_sink() {
        let config = mixer_config();
        assert!(
            matches!(config.volume_ctrl, VolumeCtrl::Cubic(db_range) if db_range == 60.0),
            "unexpected volume control: {:?}",
            config.volume_ctrl
        );

        let mixer = SoftMixer::open(config).expect("soft mixer opens with cubic control");
        for volume in [0u16, 1, 32768, 49151, u16::MAX] {
            mixer.set_volume(volume);
            let audible = mixer.get_soft_volume().attenuation_factor();
            let normalized = f64::from(volume) / f64::from(u16::MAX);
            let expected = if volume == 0 {
                0.0
            } else {
                (0.1 + 0.9 * normalized).powi(3)
            };
            assert!(
                (audible - expected).abs() <= 1e-12,
                "mixer mapping mismatch at raw volume {volume}: {audible} != {expected}"
            );
        }
    }
    #[test]
    fn player_config_does_not_poll_position() {
        // Event cadence: the engine emits state on transitions plus a 2 s
        // position heartbeat while playing. librespot must not be configured
        // to stream a 250 ms PositionChanged event for the UI to poll.
        assert!(player_config(false).position_update_interval.is_none());
    }

    #[test]
    fn normalisation_is_attenuation_only_when_enabled_and_absent_when_disabled() {
        let enabled = player_config(true);
        assert!(enabled.normalisation, "the flag must reach the player");
        assert_eq!(
            enabled.normalisation_method,
            librespot_playback::config::NormalisationMethod::Basic,
            "basic mode is constant gain only; the dynamic limiter must never run"
        );
        assert_eq!(
            enabled.normalisation_type,
            librespot_playback::config::NormalisationType::Track,
            "track gain keeps every track at the same perceived level"
        );
        assert_eq!(
            enabled.normalisation_pregain_db, 0.0,
            "no pregain: quiet tracks stay at their natural level"
        );

        let disabled = player_config(false);
        assert!(!disabled.normalisation, "off stays fully off");
        assert!(
            disabled.gapless,
            "enabling normalisation must not disturb the other player settings"
        );
    }

    #[test]
    fn prepared_oauth_attempt_carries_a_spotify_authorize_url() {
        let attempt = prepare_oauth().expect("oauth attempt prepares without network");
        assert!(
            attempt
                .auth_url
                .starts_with("https://accounts.spotify.com/authorize?"),
            "unexpected authorize URL: {}",
            attempt.auth_url
        );
        assert!(attempt.auth_url.contains("code_challenge="));
        assert!(attempt.auth_url.contains("state="));
        assert!(attempt.auth_url.contains("redirect_uri="));
        assert!(attempt.auth_url.contains("&scope=streaming"));
    }

    #[test]
    fn each_prepared_attempt_regenerates_the_authorize_url() {
        // A fresh CSRF state and PKCE challenge per attempt: re-login must
        // never reuse a URL that a previous attempt (or its browser tab) saw.
        let first = prepare_oauth().expect("first attempt");
        let second = prepare_oauth().expect("second attempt");
        assert_ne!(first.auth_url, second.auth_url);
    }

    #[test]
    fn oauth_listener_addr_is_the_loopback_callback_port() {
        let address = oauth_listener_addr().expect("redirect URI has a socket");
        assert_eq!(address.to_string(), "127.0.0.1:5588");
        assert!(OAUTH_REDIRECT_URI.contains("127.0.0.1:5588"));
    }

    #[test]
    fn oauth_callback_code_extracts_from_the_redirect_path() {
        let code = extract_oauth_code("/login?code=abc123&state=xyz").expect("code present");
        assert_eq!(code.secret(), "abc123");
        assert!(
            extract_oauth_code("/login?error=access_denied").is_err(),
            "a denied callback carries no code"
        );
        assert!(extract_oauth_code("/login").is_err());
    }
}
