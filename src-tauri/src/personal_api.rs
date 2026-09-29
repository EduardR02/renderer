//! Optional user-owned Spotify Web API grant. Playback credentials never enter this module.
//! Tokens, including rotated refresh tokens, live only in native OS credential storage.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use parking_lot::Mutex;
use rand::RngCore;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use url::Url;

use crate::app::{load_app_settings, save_app_settings, AppSettings, AppState};

const REDIRECT: &str = "http://127.0.0.1:5589/personal-api/callback";
const VAULT_SERVICE: &str = "SpotifyRenderer.PersonalApi";
const VAULT_USER: &str = "spotify-developer-grant";
const LIBRARY_SCOPES: &str = "user-library-read user-library-modify user-follow-read user-follow-modify";
const DEVICE_SCOPES: &str = "user-read-playback-state user-modify-playback-state";
const AUTH_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    client_id: String,
    account_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    scopes: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: String,
    token_type: String,
}

#[derive(Deserialize)]
struct TokenFailure {
    error: Option<String>,
}

#[derive(Deserialize)]
struct Identity {
    id: String,
}

#[derive(Deserialize, Serialize)]
pub struct Device {
    id: Option<String>,
    is_active: bool,
    is_private_session: bool,
    is_restricted: bool,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    volume_percent: Option<u8>,
    supports_volume: bool,
}

#[derive(Deserialize)]
struct Devices {
    devices: Vec<Device>,
}

#[derive(Serialize, Deserialize)]
pub struct SavedShowsPage {
    pub href: String,
    pub items: Vec<SavedShow>,
    pub limit: usize,
    pub next: Option<String>,
    pub offset: usize,
    pub previous: Option<String>,
    pub total: usize,
}

#[derive(Serialize, Deserialize)]
pub struct SavedShow {
    pub added_at: String,
    pub show: SavedShowInfo,
}

#[derive(Serialize, Deserialize)]
pub struct SavedShowInfo {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub description: String,
    pub images: Vec<ShowImage>,
    pub total_episodes: u32,
    #[serde(default)]
    pub publisher: String,
}

#[derive(Serialize, Deserialize)]
pub struct ShowImage {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub client_id: String,
    pub connected: bool,
    pub account_id: Option<String>,
    pub devices_enabled: bool,
    pub devices_authorized: bool,
    pub authorization_pending: bool,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Authorization {
    pub url: String,
}

struct Pending {
    id: u64,
    account_id: String,
    cancel: oneshot::Sender<()>,
}

#[derive(Default)]
struct Session {
    grant: Option<Grant>,
    loaded: bool,
    pending: Option<Pending>,
    serial: u64,
    error: Option<String>,
}

pub struct PersonalApi {
    http: Client,
    operation: AsyncMutex<()>,
    session: Mutex<Session>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn vault() -> Result<keyring::Entry, String> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        keyring::Entry::new(VAULT_SERVICE, VAULT_USER)
            .map_err(|_| "native credential store unavailable".to_owned())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err("native credential store unavailable on this platform".to_owned())
    }
}

fn load_grant() -> Result<Option<Grant>, String> {
    match vault()?.get_password() {
        Ok(serialized) => serde_json::from_str(&serialized)
            .map(Some)
            .map_err(|_| "stored Spotify grant is damaged; disconnect to clear it".to_owned()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("could not read the protected Spotify grant".to_owned()),
    }
}

fn save_grant(grant: &Grant) -> Result<(), String> {
    let serialized = serde_json::to_string(grant)
        .map_err(|_| "could not serialize the Spotify grant".to_owned())?;
    vault()?.set_password(&serialized)
        .map_err(|_| "could not save the protected Spotify grant".to_owned())
}

fn erase_grant() -> Result<(), String> {
    match vault()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("could not erase the protected Spotify grant".to_owned()),
    }
}

fn scopes_contain(scopes: &str, wanted: &str) -> bool {
    wanted.split_ascii_whitespace().all(|scope| scopes.split_ascii_whitespace().any(|held| held == scope))
}

fn status(session: &Session, settings: &AppSettings) -> Status {
    let grant = session.grant.as_ref().filter(|grant| grant.client_id == settings.personal_client_id);
    Status {
        client_id: settings.personal_client_id.clone(),
        connected: grant.is_some(),
        account_id: grant.map(|grant| grant.account_id.clone()),
        devices_enabled: settings.personal_devices_enabled,
        devices_authorized: grant.is_some_and(|grant| scopes_contain(&grant.scopes, DEVICE_SCOPES)),
        authorization_pending: session.pending.is_some(),
        error: session.error.clone(),
    }
}

fn emit(app: &AppHandle, session: &Session) {
    let _ = app.emit("personal-api-changed", status(session, &load_app_settings()));
}

fn active_username(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<Mutex<AppState>>();
    let state = state.lock();
    if state.playback.auth_state != "ready" || state.playback.username.is_empty() {
        return Err("sign in to playback before using the personal API".to_owned());
    }
    Ok(state.playback.username.clone())
}

fn check_account(app: &AppHandle, grant: &Grant) -> Result<(), String> {
    if active_username(app)? != grant.account_id {
        return Err("Spotify developer grant belongs to a different playback account; reconnect".to_owned());
    }
    Ok(())
}

fn random_urlsafe(bytes: usize) -> String {
    let mut data = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut data);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn refreshed_grant(grant: Grant, token: TokenResponse) -> Grant {
    Grant {
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or(grant.refresh_token),
        expires_at: now().saturating_add(token.expires_in),
        scopes: if token.scope.is_empty() { grant.scopes } else { token.scope },
        ..grant
    }
}

fn valid_client_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_uris(uris: &[String]) -> Result<(), String> {
    if uris.is_empty() || uris.len() > 40 {
        return Err("provide between 1 and 40 Spotify URIs".to_owned());
    }
    for uri in uris {
        let mut parts = uri.split(':');
        let kind = parts.next();
        let category = parts.next();
        let id = parts.next();
        if kind != Some("spotify")
            || !matches!(category, Some("track" | "album" | "episode" | "show" | "audiobook" | "artist" | "user" | "playlist"))
            || !id.is_some_and(|id| !id.is_empty() && id.len() <= 128
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
                    || category == Some("user") && (c == b'.' || c == b'@')))
            || parts.next().is_some()
        {
            return Err("invalid Spotify URI".to_owned());
        }
    }
    Ok(())
}

fn spotify_error(status: StatusCode) -> String {
    match status.as_u16() {
        401 => "Spotify authorization expired; reconnect the developer app".to_owned(),
        403 => "Spotify denied this operation (permission, subscription, or Developer Mode restriction)".to_owned(),
        429 => "Spotify rate limited this operation; try again later".to_owned(),
        500..=599 => "Spotify is temporarily unavailable; try again later".to_owned(),
        code => format!("Spotify request failed (HTTP {code})"),
    }
}

impl PersonalApi {
    pub fn new() -> Result<Arc<Self>, String> {
        let http = Client::builder().timeout(Duration::from_secs(15)).build()
            .map_err(|_| "could not create Spotify HTTP client".to_owned())?;
        Ok(Arc::new(Self { http, operation: AsyncMutex::new(()), session: Mutex::new(Session::default()) }))
    }

    fn ensure_loaded(&self) -> Result<(), String> {
        let mut session = self.session.lock();
        if !session.loaded {
            session.grant = load_grant()?;
            session.loaded = true;
        }
        Ok(())
    }

    pub async fn status(&self) -> Result<Status, String> {
        let _operation = self.operation.lock().await;
        self.ensure_loaded()?;
        Ok(status(&self.session.lock(), &load_app_settings()))
    }

    fn disconnect_locked(&self, app: &AppHandle) -> Result<Status, String> {
        // Only clear memory after the native deletion succeeds; never report disconnected
        // while a usable grant remains on disk.
        erase_grant()?;
        let mut session = self.session.lock();
        session.grant = None;
        session.loaded = true;
        if let Some(pending) = session.pending.take() {
            let _ = pending.cancel.send(());
        }
        session.serial = session.serial.wrapping_add(1);
        session.error = None;
        emit(app, &session);
        Ok(status(&session, &load_app_settings()))
    }

    pub async fn disconnect(&self, app: &AppHandle) -> Result<Status, String> {
        let _operation = self.operation.lock().await;
        self.disconnect_locked(app)
    }

    pub async fn configure(&self, app: &AppHandle, client_id: String) -> Result<Status, String> {
        let client_id = client_id.trim();
        if !client_id.is_empty() && !valid_client_id(client_id) {
            return Err("Spotify Client ID must be 32 hexadecimal characters".to_owned());
        }
        let _operation = self.operation.lock().await;
        let mut settings = load_app_settings();
        if settings.personal_client_id != client_id {
            self.disconnect_locked(app)?;
            settings.personal_client_id = client_id.to_owned();
            settings.personal_devices_enabled = false;
            save_app_settings(&settings)?;
            emit(app, &self.session.lock());
        }
        Ok(status(&self.session.lock(), &settings))
    }

    pub async fn set_devices_enabled(&self, app: &AppHandle, enabled: bool) -> Result<Status, String> {
        let _operation = self.operation.lock().await;
        let mut settings = load_app_settings();
        if enabled && settings.personal_client_id.is_empty() {
            return Err("configure your Spotify Client ID first".to_owned());
        }
        if settings.personal_devices_enabled != enabled {
            settings.personal_devices_enabled = enabled;
            save_app_settings(&settings)?;
            emit(app, &self.session.lock());
        }
        self.ensure_loaded()?;
        Ok(status(&self.session.lock(), &settings))
    }

    pub async fn authorize(self: &Arc<Self>, app: AppHandle, enable_devices: bool) -> Result<Authorization, String> {
        let _operation = self.operation.lock().await;
        let settings = load_app_settings();
        if !valid_client_id(&settings.personal_client_id) {
            return Err("configure your own Spotify developer Client ID first".to_owned());
        }
        if enable_devices && !settings.personal_devices_enabled {
            return Err("enable Spotify Connect before requesting its permissions".to_owned());
        }
        let playback_account = active_username(&app)?;
        if self.session.lock().pending.is_some() {
            return Err("Spotify authorization is already in progress".to_owned());
        }
        // A fixed redirect URI must be registered in the user's developer dashboard.
        // Bind first: a port conflict cannot result in an uncatchable browser redirect.
        let listener = TcpListener::bind("127.0.0.1:5589").await
            .map_err(|_| "personal API callback port 5589 is unavailable".to_owned())?;
        let verifier = random_urlsafe(32);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_urlsafe(32);
        let mut url = Url::parse("https://accounts.spotify.com/authorize").expect("static authorization URL");
        let scopes = if enable_devices { format!("{LIBRARY_SCOPES} {DEVICE_SCOPES}") } else { LIBRARY_SCOPES.to_owned() };
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &settings.personal_client_id)
            .append_pair("redirect_uri", REDIRECT)
            .append_pair("scope", &scopes)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge)
            .append_pair("state", &state);
        let (cancel, receiver) = oneshot::channel();
        let id = {
            let mut session = self.session.lock();
            session.serial = session.serial.wrapping_add(1);
            let id = session.serial;
            session.pending = Some(Pending { id, cancel, account_id: playback_account.clone() });
            session.error = None;
            emit(&app, &session);
            id
        };
        let api = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let result = tokio::select! {
                result = await_callback(listener, &state) => result.and_then(|result| result),
                _ = receiver => return,
                _ = tokio::time::sleep(AUTH_TIMEOUT) => Err("Spotify authorization timed out".to_owned()),
            };
            let _operation = api.operation.lock().await;
            if api.session.lock().pending.as_ref().map(|pending| pending.id) != Some(id) {
                return;
            }
            let result = match result {
                Ok(code) => api.complete_authorization(&app, &settings.personal_client_id, &playback_account, &verifier, &code, enable_devices).await,
                Err(error) => Err(error),
            };
            let mut session = api.session.lock();
            if session.pending.as_ref().map(|pending| pending.id) == Some(id) {
                session.pending = None;
                session.error = result.err();
                emit(&app, &session);
            }
        });
        Ok(Authorization { url: url.into() })
    }

    async fn token(&self, params: &[(&str, &str)]) -> Result<TokenResponse, String> {
        let response = self.http.post("https://accounts.spotify.com/api/token")
            .form(params).send().await.map_err(|_| "Spotify token service is unreachable".to_owned())?;
        if !response.status().is_success() {
            if response.status() == StatusCode::BAD_REQUEST {
                let failure: Option<TokenFailure> = response.json().await.ok();
                if failure.and_then(|failure| failure.error).as_deref() == Some("invalid_grant") {
                    return Err("Spotify authorization expired; reconnect the developer app".to_owned());
                }
                return Err("Spotify rejected the developer authorization request".to_owned());
            }
            return Err(spotify_error(response.status()));
        }
        let token: TokenResponse = response.json().await
            .map_err(|_| "Spotify returned an invalid token response".to_owned())?;
        if !token.token_type.eq_ignore_ascii_case("Bearer")
            || token.access_token.is_empty()
            || token.expires_in == 0
            || token.refresh_token.as_deref() == Some("")
        {
            return Err("Spotify returned an invalid token response".to_owned());
        }
        Ok(token)
    }

    async fn complete_authorization(&self, app: &AppHandle, client_id: &str, playback_account: &str, verifier: &str, code: &str, enable_devices: bool) -> Result<(), String> {
        let token = self.token(&[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", REDIRECT), ("client_id", client_id), ("code_verifier", verifier)]).await?;
        let Some(refresh_token) = token.refresh_token else {
            return Err("Spotify did not issue a renewable authorization".to_owned());
        };
        let identity = self.identity(&token.access_token).await?;
        if identity.id.is_empty() || identity.id != playback_account || active_username(app)? != identity.id {
            return Err("developer authorization must use the current playback account".to_owned());
        }
        if load_app_settings().personal_client_id != client_id {
            return Err("Spotify Client ID changed during authorization".to_owned());
        }
        let grant = Grant { client_id: client_id.to_owned(), account_id: identity.id, access_token: token.access_token,
            refresh_token, expires_at: now().saturating_add(token.expires_in), scopes: token.scope };
        if !scopes_contain(&grant.scopes, LIBRARY_SCOPES) {
            return Err("Spotify did not grant the requested library permissions".to_owned());
        }
        if enable_devices && !scopes_contain(&grant.scopes, DEVICE_SCOPES) {
            return Err("Spotify did not grant the requested Connect permissions".to_owned());
        }
        save_grant(&grant)?;
        let mut session = self.session.lock();
        session.grant = Some(grant);
        session.loaded = true;
        Ok(())
    }

    async fn identity(&self, token: &str) -> Result<Identity, String> {
        let response = self.http.get("https://api.spotify.com/v1/me").bearer_auth(token)
            .send().await.map_err(|_| "Spotify account verification is unreachable".to_owned())?;
        if !response.status().is_success() {
            return Err(spotify_error(response.status()));
        }
        response.json().await.map_err(|_| "Spotify returned an invalid account profile".to_owned())
    }

    // Called only while `operation` is locked: refresh rotation and requests cannot race
    // with disconnect, reauthorization, or another rotation.
    async fn access_token(&self, app: &AppHandle, devices: bool) -> Result<String, String> {
        self.ensure_loaded()?;
        let settings = load_app_settings();
        if devices && !settings.personal_devices_enabled {
            return Err("Spotify Connect is disabled".to_owned());
        }
        let grant = self.session.lock().grant.clone().filter(|grant| grant.client_id == settings.personal_client_id)
            .ok_or_else(|| "connect your own Spotify developer app first".to_owned())?;
        check_account(app, &grant)?;
        if devices && !scopes_contain(&grant.scopes, DEVICE_SCOPES) {
            return Err("authorize Spotify Connect permissions to use devices".to_owned());
        }
        if !scopes_contain(&grant.scopes, LIBRARY_SCOPES) {
            return Err("reconnect to grant Spotify library permissions".to_owned());
        }
        if grant.expires_at > now().saturating_add(30) {
            return Ok(grant.access_token);
        }
        let token = self.token(&[("grant_type", "refresh_token"), ("refresh_token", &grant.refresh_token), ("client_id", &grant.client_id)]).await;
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                if error.contains("authorization expired") {
                    self.disconnect_locked(app)?;
                }
                return Err(error);
            }
        };
        let updated = refreshed_grant(grant, token);
        // Persist the rotated token before exposing it to a call; failure is not a
        // valid reason to make an unrecoverable grant appear functional.
        save_grant(&updated)?;
        check_account(app, &updated)?;
        let access_token = updated.access_token.clone();
        self.session.lock().grant = Some(updated);
        Ok(access_token)
    }

    async fn request(&self, app: &AppHandle, devices: bool, method: Method, path: &str, uris: Option<&[String]>, body: Option<serde_json::Value>) -> Result<reqwest::Response, String> {
        let token = self.access_token(app, devices).await?;
        let mut url = Url::parse("https://api.spotify.com/v1/").expect("static API URL").join(path).expect("static API route");
        if let Some(uris) = uris {
            url.query_pairs_mut().append_pair("uris", &uris.join(","));
        }
        check_account(app, self.session.lock().grant.as_ref().expect("access_token validated grant"))?;
        let mut request = self.http.request(method, url).bearer_auth(token);
        if let Some(body) = body { request = request.json(&body); }
        let response = request.send().await.map_err(|_| "Spotify Web API is unreachable".to_owned())?;
        if !response.status().is_success() {
            return Err(spotify_error(response.status()));
        }
        Ok(response)
    }

    pub async fn contains(&self, app: &AppHandle, uris: Vec<String>) -> Result<Vec<bool>, String> {
        validate_uris(&uris)?;
        let _operation = self.operation.lock().await;
        let result: Vec<bool> = self.request(app, false, Method::GET, "me/library/contains", Some(&uris), None).await?
            .json().await.map_err(|_| "Spotify returned an invalid membership response".to_owned())?;
        if result.len() != uris.len() {
            return Err("Spotify returned incomplete library membership".to_owned());
        }
        Ok(result)
    }

    pub async fn set_saved(&self, app: &AppHandle, uris: Vec<String>, saved: bool) -> Result<(), String> {
        validate_uris(&uris)?;
        let _operation = self.operation.lock().await;
        let method = if saved { Method::PUT } else { Method::DELETE };
        self.request(app, false, method, "me/library", Some(&uris), None).await?;
        Ok(())
    }

    /// User-owned grant only; no request is made until the subscribed-podcasts
    /// surface explicitly asks for a page.
    pub async fn saved_shows(&self, app: &AppHandle, offset: usize, limit: usize) -> Result<SavedShowsPage, String> {
        if !(1..=50).contains(&limit) {
            return Err("saved shows page size must be between 1 and 50".to_owned());
        }
        let _operation = self.operation.lock().await;
        let path = format!("me/shows?offset={offset}&limit={limit}");
        self.request(app, false, Method::GET, &path, None, None).await?
            .json().await.map_err(|_| "Spotify returned an invalid saved shows page".to_owned())
    }

    pub async fn devices(&self, app: &AppHandle) -> Result<Vec<Device>, String> {
        let _operation = self.operation.lock().await;
        let devices: Devices = self.request(app, true, Method::GET, "me/player/devices", None, None).await?
            .json().await.map_err(|_| "Spotify returned an invalid devices response".to_owned())?;
        Ok(devices.devices)
    }

    pub async fn transfer(&self, app: &AppHandle, device_id: String, play: bool) -> Result<(), String> {
        if device_id.is_empty() || device_id.len() > 256 || device_id.chars().any(char::is_control) {
            return Err("invalid Spotify device ID".to_owned());
        }
        let _operation = self.operation.lock().await;
        self.request(app, true, Method::PUT, "me/player", None, Some(serde_json::json!({"device_ids": [device_id], "play": play}))).await?;
        Ok(())
    }

    pub async fn account_changed(&self, app: &AppHandle, username: &str) {
        let _operation = self.operation.lock().await;
        let needs_disconnect = self.ensure_loaded().is_ok()
            && self.session.lock().grant.as_ref().is_some_and(|grant| grant.account_id != username);
        if needs_disconnect {
            // A previously authorized account must never silently become the new
            // playback account's library or Connect identity.
            if self.disconnect_locked(app).is_err() {
                let mut session = self.session.lock();
                session.error = Some("could not erase the previous account's protected grant".to_owned());
                emit(app, &session);
            }
        } else {
            let mut session = self.session.lock();
            if session.pending.as_ref().is_some_and(|pending| pending.account_id != username) {
                if let Some(pending) = session.pending.take() {
                    let _ = pending.cancel.send(());
                }
                session.error = Some("playback account changed during authorization".to_owned());
                emit(app, &session);
            }
        }
    }
}

async fn answer(stream: &mut TcpStream, success: bool) {
    let (status, body) = if success {
        ("200 OK", "Spotify authorization received. You can return to the app.")
    } else {
        ("400 Bad Request", "Spotify authorization was not accepted. Return to the app and try again.")
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn await_callback(listener: TcpListener, expected_state: &str) -> Result<Result<String, String>, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|_| "personal API callback listener failed".to_owned())?;
        let request_line = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = [0_u8; 4096];
            let mut length = 0;
            while length < bytes.len() {
                let read = stream.read(&mut bytes[length..]).await.ok()?;
                if read == 0 { return None; }
                length += read;
                if let Some(end) = bytes[..length].iter().position(|&byte| byte == b'\n') {
                    return std::str::from_utf8(&bytes[..end]).ok().map(str::to_owned);
                }
            }
            None
        }).await.ok().flatten();
        let target = request_line.as_deref()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.split_whitespace().next())
            .filter(|target| target.starts_with("/personal-api/callback?"));
        let url = target.and_then(|target| Url::parse(&format!("http://127.0.0.1:5589{target}")).ok());
        let Some(url) = url.filter(|url| url.host_str() == Some("127.0.0.1")
            && url.port() == Some(5589) && url.path() == "/personal-api/callback") else {
            answer(&mut stream, false).await;
            continue;
        };
        let matches_state = url.query_pairs().any(|(name, value)| name == "state" && value == expected_state);
        if !matches_state {
            answer(&mut stream, false).await;
            continue;
        }
        let code = url.query_pairs().find(|(name, _)| name == "code").map(|(_, value)| value.into_owned());
        let failure = url.query_pairs().any(|(name, _)| name == "error");
        answer(&mut stream, code.is_some() && !failure).await;
        return Ok(if failure { Err("Spotify authorization was declined".to_owned()) }
            else { code.filter(|code| !code.is_empty()).ok_or_else(|| "Spotify authorization response had no code".to_owned()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_validation_rejects_malformed_and_oversized_batches() {
        assert!(validate_uris(&["spotify:artist:abc123".into(), "spotify:user:person_2".into()]).is_ok());
        assert!(validate_uris(&["spotify:track:a?uris=spotify:track:b".into()]).is_err());
        assert!(validate_uris(&vec!["spotify:track:abc".into(); 41]).is_err());
        assert!(validate_uris(&[]).is_err());
    }

    #[test]
    fn scope_matching_uses_whole_scope_names() {
        assert!(scopes_contain("user-library-read user-follow-read", "user-follow-read"));
        assert!(!scopes_contain("user-library-read user-follow-readonly", "user-follow-read"));
    }

    #[test]
    fn rotated_refresh_grants_preserve_identity_and_replace_secret() {
        let grant = Grant {
            client_id: "owned-app".into(),
            account_id: "user-1".into(),
            access_token: "old access".into(),
            refresh_token: "old refresh".into(),
            expires_at: 0,
            scopes: LIBRARY_SCOPES.into(),
        };
        let token = TokenResponse {
            access_token: "new access".into(),
            refresh_token: Some("new refresh".into()),
            expires_in: 3600,
            scope: String::new(),
            token_type: "Bearer".into(),
        };
        let rotated = refreshed_grant(grant.clone(), token);
        assert_eq!(rotated.refresh_token, "new refresh");
        assert_eq!(rotated.account_id, "user-1");
        assert_eq!(rotated.scopes, LIBRARY_SCOPES);
        let unrotated = refreshed_grant(grant, TokenResponse {
            access_token: "next access".into(),
            refresh_token: None,
            expires_in: 3600,
            scope: String::new(),
            token_type: "Bearer".into(),
        });
        assert_eq!(unrotated.refresh_token, "old refresh");
    }

    #[test]
    fn refresh_response_without_scope_keeps_previous_permissions() {
        let grant = Grant {
            client_id: "owned-app".into(),
            account_id: "user-1".into(),
            access_token: "old".into(),
            refresh_token: "refresh".into(),
            expires_at: 0,
            scopes: LIBRARY_SCOPES.into(),
        };
        let token: TokenResponse = serde_json::from_str(
            r#"{"access_token":"new","token_type":"Bearer","expires_in":3600}"#
        ).unwrap();
        let updated = refreshed_grant(grant, token);
        assert_eq!(updated.scopes, LIBRARY_SCOPES);
        assert_eq!(updated.refresh_token, "refresh");
    }

    #[tokio::test]
    async fn listener_rejects_wrong_state_and_accepts_matching_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(await_callback(listener, "expected"));
        for (state, expected) in [("wrong", false), ("expected", true)] {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream.write_all(format!("GET /personal-api/callback?state={state}&code=approved HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
            let mut bytes = [0; 512];
            let size = stream.read(&mut bytes).await.unwrap();
            assert_eq!(std::str::from_utf8(&bytes[..size]).unwrap().contains("200 OK"), expected);
        }
        assert_eq!(task.await.unwrap().unwrap().unwrap(), "approved");
    }
}
