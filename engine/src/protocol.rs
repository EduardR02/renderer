use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    pub artist_id: String,
    pub album_id: String,
    pub album_name: String,
    pub cover_url: String,
    pub duration_ms: u32,
}

impl Default for TrackRef {
    fn default() -> Self {
        Self {
            id: String::new(),
            uri: String::new(),
            name: String::new(),
            artist_names: Vec::new(),
            artist_id: String::new(),
            album_id: String::new(),
            album_name: String::new(),
            cover_url: String::new(),
            duration_ms: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub request_id: String,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Status,
    PlayQueue {
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
    },
    Play,
    Pause,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    SetVolume {
        percent: u8,
    },
    SetShuffle {
        enabled: bool,
    },
    SetRepeat {
        mode: RepeatMode,
    },
    AddQueue {
        track: TrackRef,
    },
    RemoveQueue {
        index: usize,
    },
    MoveQueue {
        from: usize,
        to: usize,
    },
    Shutdown,
    /// Mints (or returns a cached) Web API access token via login5 so the UI
    /// can browse without its own OAuth app. Responded to with
    /// [`WebApiTokenResponse`].
    WebApiToken,
    /// Clears the cached credentials and tears the session down; the engine
    /// immediately reports `needs_login` (with a fresh authorize URL).
    Logout,
    /// Starts the OAuth flow on demand, using the authorize URL the engine
    /// published in its `needs_login` state. No-op while a session is live or
    /// a flow is already running.
    Login,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Authenticating,
    /// No usable session and no flow in flight: the UI must present the
    /// [`StateEvent::auth_url`] authorize link (Log in) to start one.
    NeedsLogin,
    Ready,
    Error,
}

#[derive(Serialize)]
pub struct Response<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_id: &'a str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
}

/// Response for [`Command::WebApiToken`]: carries the engine-minted login5
/// token on success (token fields are omitted on failure). `expires_in` is
/// the skew-adjusted remaining lifetime in seconds.
#[derive(Serialize)]
pub struct WebApiTokenResponse<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_id: &'a str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
}

#[derive(Serialize)]
pub struct StateEvent<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub ready: bool,
    pub auth_state: AuthState,
    /// Spotify OAuth authorize URL for the current/next login attempt. Set
    /// while the engine is `NeedsLogin` (waiting for the UI to open it) and
    /// while the flow it started is `Authenticating`; regenerated per attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<&'a str>,
    pub playing: bool,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub current_index: Option<usize>,
    pub current_uri: Option<&'a str>,
    pub queue: &'a [TrackRef],
    pub error: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuthState, Command, RepeatMode, Request, Response, StateEvent, TrackRef,
                WebApiTokenResponse};

    #[test]
    fn web_api_token_command_deserializes_from_the_line_protocol() {
        let request: Request = serde_json::from_value(json!({
            "request_id": "request-7",
            "type": "web_api_token",
        }))
        .unwrap();
        assert_eq!(request.request_id, "request-7");
        assert!(matches!(request.command, Command::WebApiToken));
    }

    #[test]
    fn web_api_token_response_serializes_the_token_contract() {
        let success = serde_json::to_value(WebApiTokenResponse {
            kind: "web_api_token",
            request_id: "request-7",
            ok: true,
            error: None,
            token_type: Some("Bearer"),
            access_token: Some("access-token-value"),
            expires_in: Some(3540),
        })
        .unwrap();
        assert_eq!(
            success,
            json!({
                "type": "web_api_token",
                "request_id": "request-7",
                "ok": true,
                "token_type": "Bearer",
                "access_token": "access-token-value",
                "expires_in": 3540
            })
        );
    }

    #[test]
    fn web_api_token_response_omits_token_fields_on_error() {
        let failure = serde_json::to_value(WebApiTokenResponse {
            kind: "web_api_token",
            request_id: "request-8",
            ok: false,
            error: Some("could not mint a Spotify Web API token: unavailable"),
            token_type: None,
            access_token: None,
            expires_in: None,
        })
        .unwrap();
        assert_eq!(
            failure,
            json!({
                "type": "web_api_token",
                "request_id": "request-8",
                "ok": false,
                "error": "could not mint a Spotify Web API token: unavailable"
            })
        );
    }


    #[test]
    fn response_serialization_omits_absent_errors_and_preserves_failures() {
        let success = serde_json::to_value(Response {
            kind: "response",
            request_id: "request-1",
            ok: true,
            error: None,
        })
        .unwrap();
        assert_eq!(
            success,
            json!({"type": "response", "request_id": "request-1", "ok": true})
        );

        let failure = serde_json::to_value(Response {
            kind: "response",
            request_id: "request-2",
            ok: false,
            error: Some("queue index 4 is out of range"),
        })
        .unwrap();
        assert_eq!(
            failure,
            json!({
                "type": "response",
                "request_id": "request-2",
                "ok": false,
                "error": "queue index 4 is out of range"
            })
        );
    }

    #[test]
    fn state_serialization_matches_the_cpp_line_protocol() {
        let track = TrackRef {
            id: "track-id".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Track".to_owned(),
            artist_names: vec!["Artist".to_owned()],
            duration_ms: 123_456,
            ..TrackRef::default()
        };
        let queue = [track];
        let state = serde_json::to_value(StateEvent {
            kind: "state",
            ready: true,
            auth_state: AuthState::Ready,
            auth_url: None,
            playing: true,
            position_ms: 7_500,
            duration_ms: 123_456,
            volume: 42,
            shuffle: false,
            repeat: RepeatMode::Context,
            current_index: Some(0),
            current_uri: Some("spotify:track:0123456789ABCDEFGHIJKL"),
            queue: &queue,
            error: None,
        })
        .unwrap();

        assert_eq!(state["type"], "state");
        assert_eq!(state["auth_state"], "ready");
        assert_eq!(state["repeat"], "context");
        assert_eq!(state["current_index"], 0);
        assert_eq!(state["current_uri"], queue[0].uri);
        assert_eq!(state["queue"][0]["artist_names"], json!(["Artist"]));
        assert_eq!(state["queue"][0]["duration_ms"], 123_456);
        assert!(state["error"].is_null());
        // The authorize URL is only present while a login attempt is pending.
        assert!(state["auth_url"].is_null());
    }

    #[test]
    fn needs_login_state_carries_the_oauth_authorize_url() {
        let state = serde_json::to_value(StateEvent {
            kind: "state",
            ready: false,
            auth_state: AuthState::NeedsLogin,
            auth_url: Some("https://accounts.spotify.com/authorize?state=abc"),
            playing: false,
            position_ms: 0,
            duration_ms: 0,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            current_index: None,
            current_uri: None,
            queue: &[],
            error: None,
        })
        .unwrap();
        assert_eq!(state["auth_state"], "needs_login");
        assert_eq!(
            state["auth_url"],
            "https://accounts.spotify.com/authorize?state=abc"
        );
    }

    #[test]
    fn login_and_logout_commands_deserialize_from_the_line_protocol() {
        let login: Request = serde_json::from_value(json!({
            "request_id": "request-9",
            "type": "login",
        }))
        .unwrap();
        assert!(matches!(login.command, Command::Login));

        let logout: Request = serde_json::from_value(json!({
            "request_id": "request-10",
            "type": "logout",
        }))
        .unwrap();
        assert!(matches!(logout.command, Command::Logout));
    }
}
