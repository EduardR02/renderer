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
    /// User playlist library via the spclient rootlist (first `length`
    /// entries). Responded to with a `browse_playlists` message.
    BrowsePlaylists {
        length: usize,
    },
    /// Playlist metadata and tracks via `/playlist/v2/playlist/{id}`.
    /// Responded to with a `browse_playlist` message.
    BrowsePlaylist {
        id: String,
    },
    /// Album metadata and tracks via the extended-metadata album endpoint.
    /// Responded to with a `browse_album` message.
    BrowseAlbum {
        id: String,
    },
    /// Artist metadata, top tracks, and albums via the extended-metadata
    /// artist endpoint. Responded to with a `browse_artist` message.
    BrowseArtist {
        id: String,
    },
    /// Search via the spclient searchview endpoint. Responded to with a
    /// `browse_search` message.
    BrowseSearch {
        query: String,
        limit: usize,
    },
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

/// Playlist reference inside browse responses. `owner_id` is the owning
/// user's Spotify username; `owner_name` is their display name when the
/// source carries it (the rootlist only exposes usernames, so it is empty
/// there).
#[derive(Serialize)]
pub struct PlaylistRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub owner_id: String,
    pub owner_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u32>,
}

/// Payload of a successful [`Command::BrowsePlaylist`] response. `revision`
/// is the playlist4 revision hex-encoded (the value Web API edits call the
/// snapshot id).
#[derive(Serialize)]
pub struct PlaylistBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub owner_id: String,
    pub owner_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    pub tracks: Vec<TrackRef>,
}

/// Album reference inside browse responses.
#[derive(Serialize)]
pub struct AlbumRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

/// Payload of a successful [`Command::BrowseAlbum`] response.
#[derive(Serialize)]
pub struct AlbumBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    pub tracks: Vec<TrackRef>,
}

/// Artist reference inside browse responses.
#[derive(Serialize)]
pub struct ArtistRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait_url: Option<String>,
}

/// Payload of a successful [`Command::BrowseArtist`] response.
#[derive(Serialize)]
pub struct ArtistBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait_url: Option<String>,
    pub top_tracks: Vec<TrackRef>,
    pub albums: Vec<AlbumRef>,
}

/// Payload of a successful [`Command::BrowseSearch`] response.
#[derive(Serialize)]
pub struct SearchBrowse {
    pub tracks: Vec<TrackRef>,
    pub albums: Vec<AlbumRef>,
    pub artists: Vec<ArtistRef>,
}

/// Envelope for every `browse_*` response: the payload travels in `data` on
/// success, error text only on failure. `kind` matches the command name.
#[derive(Serialize)]
pub struct BrowseResponse<'a, T: Serialize> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_id: &'a str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<&'a T>,
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
    /// Spotify username of the live session, present once authenticated.
    /// The UI uses it for Web API calls that need a user id (`/v1/users/…`)
    /// instead of the removed `/v1/me` round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
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

    use super::{AuthState, BrowseResponse, Command, PlaylistRef, RepeatMode, Request, Response,
                SearchBrowse, StateEvent, TrackRef, WebApiTokenResponse};

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
            username: Some("alice".to_owned()),
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
            username: None,
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

    #[test]
    fn browse_commands_deserialize_from_the_line_protocol() {
        let playlists: Request = serde_json::from_value(json!({
            "request_id": "request-11",
            "type": "browse_playlists",
            "length": 50,
        }))
        .unwrap();
        assert!(matches!(
            playlists.command,
            Command::BrowsePlaylists { length: 50 }
        ));

        let playlist: Request = serde_json::from_value(json!({
            "request_id": "request-12",
            "type": "browse_playlist",
            "id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            playlist.command,
            Command::BrowsePlaylist { ref id } if id == "0123456789ABCDEFGHIJKL"
        ));

        let album: Request = serde_json::from_value(json!({
            "request_id": "request-13",
            "type": "browse_album",
            "id": "0abcdefghijklmnopqrstu",
        }))
        .unwrap();
        assert!(matches!(album.command, Command::BrowseAlbum { .. }));

        let artist: Request = serde_json::from_value(json!({
            "request_id": "request-14",
            "type": "browse_artist",
            "id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(artist.command, Command::BrowseArtist { .. }));

        let search: Request = serde_json::from_value(json!({
            "request_id": "request-15",
            "type": "browse_search",
            "query": "fire & ice",
            "limit": 10,
        }))
        .unwrap();
        assert!(matches!(
            search.command,
            Command::BrowseSearch { ref query, limit: 10 } if query == "fire & ice"
        ));
    }

    #[test]
    fn browse_response_serializes_the_data_envelope() {
        let playlists = vec![PlaylistRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:playlist:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Road Trip".to_owned(),
            owner_id: "alice".to_owned(),
            owner_name: String::new(),
            cover_url: Some("https://i.scdn.co/image/0123".to_owned()),
            track_count: Some(42),
        }];
        let payload = SearchBrowse {
            tracks: vec![TrackRef {
                id: "2abcdefghijklmnopqrstu".to_owned(),
                uri: "spotify:track:2abcdefghijklmnopqrstu".to_owned(),
                name: "Track".to_owned(),
                ..TrackRef::default()
            }],
            albums: Vec::new(),
            artists: Vec::new(),
        };

        let success = serde_json::to_value(BrowseResponse {
            kind: "browse_search",
            request_id: "request-15",
            ok: true,
            error: None,
            data: Some(&payload),
        })
        .unwrap();
        assert_eq!(
            success,
            json!({
                "type": "browse_search",
                "request_id": "request-15",
                "ok": true,
                "data": {
                    "tracks": [{
                        "id": "2abcdefghijklmnopqrstu",
                        "uri": "spotify:track:2abcdefghijklmnopqrstu",
                        "name": "Track",
                        "artist_names": [],
                        "artist_id": "",
                        "album_id": "",
                        "album_name": "",
                        "cover_url": "",
                        "duration_ms": 0
                    }],
                    "albums": [],
                    "artists": []
                }
            })
        );

        let failure = serde_json::to_value(BrowseResponse::<SearchBrowse> {
            kind: "browse_search",
            request_id: "request-15",
            ok: false,
            error: Some("search request failed: unavailable"),
            data: None,
        })
        .unwrap();
        assert_eq!(
            failure,
            json!({
                "type": "browse_search",
                "request_id": "request-15",
                "ok": false,
                "error": "search request failed: unavailable"
            })
        );
        assert!(failure.get("data").is_none());

        // browse_playlists carries its payload as a bare array in `data`.
        let lists = serde_json::to_value(BrowseResponse {
            kind: "browse_playlists",
            request_id: "request-11",
            ok: true,
            error: None,
            data: Some(&playlists),
        })
        .unwrap();
        assert_eq!(
            lists["data"],
            json!([{
                "id": "0123456789ABCDEFGHIJKL",
                "uri": "spotify:playlist:0123456789ABCDEFGHIJKL",
                "name": "Road Trip",
                "owner_id": "alice",
                "owner_name": "",
                "cover_url": "https://i.scdn.co/image/0123",
                "track_count": 42
            }])
        );
    }

    #[test]
    fn state_serialization_carries_the_username_while_logged_in() {
        let state = serde_json::to_value(StateEvent {
            kind: "state",
            ready: true,
            auth_state: AuthState::Ready,
            auth_url: None,
            username: Some("alice".to_owned()),
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
        assert_eq!(state["username"], "alice");
        let logged_out = serde_json::to_value(StateEvent {
            kind: "state",
            ready: false,
            auth_state: AuthState::NeedsLogin,
            auth_url: None,
            username: None,
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
        assert!(logged_out.get("username").is_none());
    }
}
