use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to [`Self::artist_names`]: same length, same
    /// order, so the two zip index-for-index and every credited artist can be
    /// linked, not just the primary.
    ///
    /// Both lists are built from one pass over the same source list on every
    /// path that produces a `TrackRef`, so the lengths cannot diverge. An
    /// *individual* entry can still be empty when an artist has no resolvable
    /// id, so a name is only a link when its id is non-empty.
    ///
    /// [`Self::artist_id`] stays as the primary artist and is unchanged.
    pub artist_ids: Vec<String>,
    pub artist_id: String,
    pub album_id: String,
    pub album_name: String,
    pub cover_url: String,
    pub duration_ms: u32,
    /// Lifetime Spotify play count when the browse surface supplies it.
    /// Album rows and artist Popular rows do; playlists/search/queue do not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u64>,
    /// Unix timestamp in milliseconds when this item was added to a
    /// playlist. This is populated only by playlist browsing; tracks from
    /// albums, search, and playback queues leave it absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<i64>,
    /// A stable, server- or account-policy restriction. Transient playback
    /// failures never set this field.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unavailable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}
impl Default for TrackRef {
    fn default() -> Self {
        Self {
            id: String::new(),
            uri: String::new(),
            name: String::new(),
            artist_names: Vec::new(),
            artist_ids: Vec::new(),
            artist_id: String::new(),
            album_id: String::new(),
            album_name: String::new(),
            cover_url: String::new(),
            duration_ms: 0,
            play_count: None,
            added_at: None,
            unavailable: false,
            unavailable_reason: None,
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
    /// Installs a paused queue/playhead without asking librespot to load audio.
    /// The current item is loaded only when a later `play` arrives.
    RestoreQueue {
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
    },
    PlayQueueIndex {
        index: usize,
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
    AddQueueBatch {
        tracks: Vec<TrackRef>,
    },
    RemoveQueue {
        index: usize,
    },
    MoveQueue {
        from: usize,
        to: usize,
    },
    Shutdown,
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
    /// Song or artist radio with server-ranked playable tracks. Plain ids use
    /// inspired-by song radio; an `artist:` prefix selects Apollo artist radio.
    /// Responded to with a `browse_radio` message.
    BrowseRadio {
        id: String,
    },
    /// Optional, on-demand recommendations for one playlist. Ordinary
    /// playlist browse never invokes this endpoint.
    BrowsePlaylistRecommendations {
        id: String,
    },
    /// Album metadata/tracks plus play counts from the official pathfinder
    /// album query.
    /// Responded to with a `browse_album` message.
    BrowseAlbum {
        id: String,
    },
    /// Artist metadata, top tracks, releases, Popular play counts, and the
    /// optional pathfinder overview. Responded to with `browse_artist`.
    BrowseArtist {
        id: String,
    },
    /// One bounded page of an artist's release artwork/metadata.
    BrowseArtistReleases {
        id: String,
        #[serde(default)]
        release_types: Vec<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_browse_page_size")]
        limit: usize,
    },
    /// A bounded number of complete releases for the expanded catalogue view.
    BrowseArtistCatalogue {
        id: String,
        #[serde(default)]
        release_types: Vec<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_catalogue_release_page_size")]
        limit: usize,
    },
    /// One authenticated page of the user's Saved Tracks collection.
    BrowseLikedSongs {
        #[serde(default)]
        cursor: Option<String>,
    },
    /// Search via the spclient searchview endpoint. Responded to with a
    /// `browse_search` message.
    BrowseSearch {
        query: String,
        limit: usize,
    },
    /// Songwriter/producer/performer credits through the official pathfinder
    /// grouped-credits query. Responded to with a `browse_track_credits`
    /// message. Fetched on demand only.
    BrowseTrackCredits {
        id: String,
    },
    /// Creates a playlist via the spclient playlist4 create endpoint and adds
    /// it to the user's rootlist. Responded to with an `edit_create_playlist`
    /// message carrying the new playlist's [`PlaylistRef`].
    EditCreatePlaylist {
        name: String,
    },
    /// Renames a playlist via a playlist4 UPDATE_LIST_ATTRIBUTES change.
    /// Responded to with an `edit_rename_playlist` message.
    EditRenamePlaylist {
        id: String,
        name: String,
    },
    /// Unfollows (removes) a playlist from the user's rootlist via a rootlist
    /// REM change. Responded to with an `edit_delete_playlist` message.
    EditDeletePlaylist {
        id: String,
    },
    /// Appends tracks to a playlist via a playlist4 ADD change. Responded to
    /// with an `edit_add_playlist_tracks` message.
    EditAddPlaylistTracks {
        id: String,
        uris: Vec<String>,
    },
    /// Removes tracks by URI from a playlist via a playlist4 REM change
    /// (`items_as_key`). Responded to with an `edit_remove_playlist_tracks`
    /// message.
    EditRemovePlaylistTracks {
        id: String,
        uris: Vec<String>,
    },
    /// Moves one track to the position `to` (insert before `to`, Web API
    /// reorder semantics) via a playlist4 MOV change. Responded to with an
    /// `edit_reorder_playlist_tracks` message.
    EditReorderPlaylistTracks {
        id: String,
        from: usize,
        to: usize,
    },
    /// Clears the cached credentials and tears the session down; the engine
    /// immediately reports `needs_login` (with a fresh authorize URL).
    Logout,
    /// Starts the OAuth flow on demand, using the authorize URL the engine
    /// published in its `needs_login` state. No-op while a session is live or
    /// a flow is already running.
    Login,
}

fn default_browse_page_size() -> usize {
    20
}

fn default_catalogue_release_page_size() -> usize {
    4
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

/// Playlist reference inside browse responses. `owner_id` is the owning
/// user's Spotify username; `owner_name` is their display name when the
/// source carries it (the rootlist only exposes usernames, so it is empty
/// there).
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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

/// A playlist-like radio context. `tracks[0]` is always the first playable
/// item in server rank order; recommendations follow without repeats.
///
/// `seed_kind` is `"track"` for the inspired-by song route and `"artist"` for
/// the Apollo artist station route. Artist radio has no playable artist seed,
/// so `seed` is the station's first ranked track and `seed_artist` carries the
/// label shown by the client.
///
/// Radio responses intentionally carry no follower, like, or listener count.
/// The inspired-by/Apollo payloads expose aggregate-looking fields whose
/// semantics are not confirmed for this context; rendering them would invent
/// a metric rather than report a known value.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RadioBrowse {
    pub seed: TrackRef,
    pub tracks: Vec<TrackRef>,
    pub seed_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_artist: Option<ArtistRef>,
}

/// Optional recommendations displayed below a playlist. This payload is
/// intentionally separate from [`PlaylistBrowse`] so opening a playlist has
/// zero radio-Apollo cost.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PlaylistRecommendations {
    pub playlist_id: String,
    pub tracks: Vec<TrackRef>,
}

/// Album reference inside browse responses.
///
/// `year` is the release year and is deliberately the only date field: the
/// metadata protobuf leaves month and day absent for most releases, and
/// librespot substitutes 1 January for them, so a full date would be
/// indistinguishable from a genuine New Year's Day release. The year itself
/// is always real, and absent (rather than 0) when the release carries no
/// date at all.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AlbumRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to [`Self::artist_names`], preserving one
    /// independently navigable entry for every album-header artist.
    ///
    /// Older persisted/frontend payloads may omit this field; serde's
    /// `default` on the containing type makes that a plain-text fallback.
    pub artist_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
}

/// Payload of a successful [`Command::BrowseAlbum`] response.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AlbumBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to [`Self::artist_names`]; empty entries remain
    /// plain text in the frontend rather than becoming false links.
    pub artist_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    pub tracks: Vec<TrackRef>,
}

/// Artist reference inside browse responses.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtistRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait_url: Option<String>,
}

/// An artist's resolved release page, grouped the way the official client
/// groups it. The vectors contain only the requested page, never the full
/// catalogue.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistReleases {
    pub albums: Vec<AlbumRef>,
    pub singles: Vec<AlbumRef>,
    pub compilations: Vec<AlbumRef>,
    pub appears_on: Vec<AlbumRef>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistReleaseCounts {
    pub albums: usize,
    pub singles: usize,
    pub compilations: usize,
    pub appears_on: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistReleasePage {
    pub releases: ArtistReleases,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistCataloguePage {
    pub releases: Vec<AlbumBrowse>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

/// One city in an artist's listener ranking. Every field comes from the
/// artist-overview document; absent listener counts are omitted rather than
/// represented as zero.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistTopCity {
    pub city: String,
    pub country: String,
    pub region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listeners: Option<u64>,
}

/// Optional facts and recommendations that augment the metadata4 artist page.
///
/// This stays a separate object because the pathfinder overview document is a
/// best-effort enhancement. Metadata4 can still supply biography, popularity,
/// and related artists when every persisted-query hash has rotated.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ArtistOverview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popularity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followers: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_listeners: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_rank: Option<u32>,
    pub top_cities: Vec<ArtistTopCity>,
    pub popular_releases: Vec<AlbumRef>,
    pub related_artists: Vec<ArtistRef>,
}

/// Payload of a successful [`Command::BrowseArtist`] response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtistBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait_url: Option<String>,
    pub top_tracks: Vec<TrackRef>,
    pub releases: ArtistReleases,
    pub release_counts: ArtistReleaseCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub releases_next_offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<ArtistOverview>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LikedSongsPage {
    pub tracks: Vec<TrackRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One contributor in a track's credits.
///
/// `id` is the validated Spotify artist id from the source credit URI and is
/// retained for identity/diagnostics only. External contributor pages come
/// finished in `url`; the songwriter id namespace is not the artist one.
/// `subroles` are the service's own labels (`"composer"`, `"lyricist"`,
/// `"producer"`, `"main artist"`, ...) and may be empty.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CreditArtist {
    pub id: String,
    pub uri: String,
    /// External page for this contributor, supplied finished by the service —
    /// for writers, their `artists.spotify.com/songwriter/<id>` page. Empty
    /// when the service gives none. Never constructed here: the songwriter id
    /// space is not the artist one and nothing in the payload maps between
    /// them.
    pub url: String,
    pub name: String,
    pub subroles: Vec<String>,
}
/// One source-provided role group of a track's credits, e.g. Artist or
/// Composition & Lyrics. `title` is passed through exactly as the service
/// spells it rather than mapped to an enum: the set is not fixed, and an
/// unknown group is still worth showing.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CreditRole {
    pub title: String,
    pub artists: Vec<CreditArtist>,
}

/// Payload of a successful [`Command::BrowseTrackCredits`] response.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackCredits {
    pub track_uri: String,
    pub track_name: String,
    pub roles: Vec<CreditRole>,
    /// The licensor the credits came from, e.g. `Republic Records`. Empty when
    /// the service does not supply one.
    pub source: String,
}

/// Payload of a successful [`Command::BrowseSearch`] response.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SearchBrowse {
    pub tracks: Vec<TrackRef>,
    pub albums: Vec<AlbumRef>,
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub playlists: Vec<PlaylistRef>,
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

/// The engine's 2-second position heartbeat: only the scalar playhead data
/// the frontend projects and clamps against. Emitted only while playing.
/// Full [`StateEvent`]s — queue included — are reserved for real changes
/// (track/queue/volume/shuffle/repeat/duration, play/pause), so a heartbeat
/// never serializes or clones the queue: its cost is O(1) in queue length.
#[derive(Serialize)]
pub struct PositionEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub position_ms: u32,
    pub duration_ms: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AuthState, ArtistRef, BrowseResponse, Command, PlaylistRecommendations, PlaylistRef,
        PositionEvent, RadioBrowse, RepeatMode, Request, Response, SearchBrowse, StateEvent,
        TrackRef,
    };

    #[test]
    fn edit_commands_deserialize_from_the_line_protocol() {
        let create: Request = serde_json::from_value(json!({
            "request_id": "request-16",
            "type": "edit_create_playlist",
            "name": "Road Trip",
        }))
        .unwrap();
        assert!(matches!(
            create.command,
            Command::EditCreatePlaylist { ref name } if name == "Road Trip"
        ));

        let rename: Request = serde_json::from_value(json!({
            "request_id": "request-17",
            "type": "edit_rename_playlist",
            "id": "0123456789ABCDEFGHIJKL",
            "name": "Renamed",
        }))
        .unwrap();
        assert!(matches!(
            rename.command,
            Command::EditRenamePlaylist { ref id, ref name }
                if id == "0123456789ABCDEFGHIJKL" && name == "Renamed"
        ));

        let delete: Request = serde_json::from_value(json!({
            "request_id": "request-18",
            "type": "edit_delete_playlist",
            "id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(delete.command, Command::EditDeletePlaylist { .. }));

        let add: Request = serde_json::from_value(json!({
            "request_id": "request-19",
            "type": "edit_add_playlist_tracks",
            "id": "0123456789ABCDEFGHIJKL",
            "uris": ["spotify:track:2abcdefghijklmnopqrstu"],
        }))
        .unwrap();
        assert!(matches!(
            add.command,
            Command::EditAddPlaylistTracks { ref uris, .. } if uris == &["spotify:track:2abcdefghijklmnopqrstu".to_owned()]
        ));

        let remove: Request = serde_json::from_value(json!({
            "request_id": "request-20",
            "type": "edit_remove_playlist_tracks",
            "id": "0123456789ABCDEFGHIJKL",
            "uris": [],
        }))
        .unwrap();
        assert!(matches!(
            remove.command,
            Command::EditRemovePlaylistTracks { uris, .. } if uris.is_empty()
        ));

        let reorder: Request = serde_json::from_value(json!({
            "request_id": "request-21",
            "type": "edit_reorder_playlist_tracks",
            "id": "0123456789ABCDEFGHIJKL",
            "from": 3,
            "to": 1,
        }))
        .unwrap();
        assert!(matches!(
            reorder.command,
            Command::EditReorderPlaylistTracks { from: 3, to: 1, .. }
        ));
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
    fn position_heartbeat_serializes_as_a_scalar_line_without_queue() {
        let heartbeat = serde_json::to_value(PositionEvent {
            kind: "position",
            position_ms: 12_345,
            duration_ms: 240_000,
        })
        .unwrap();
        let object = heartbeat.as_object().expect("heartbeat is an object");
        // `type` plus the two playhead scalars and nothing else: a heartbeat
        // must never carry (or serialize) the queue or any other state.
        assert_eq!(object.len(), 3, "only type, position_ms and duration_ms");
        assert_eq!(heartbeat["type"], "position");
        assert_eq!(heartbeat["position_ms"], 12_345);
        assert_eq!(heartbeat["duration_ms"], 240_000);
        assert!(!object.contains_key("queue"), "no queue on a heartbeat");
        assert!(!object.contains_key("playing"), "no flags on a heartbeat");
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
    fn queue_index_jump_does_not_resend_the_queue() {
        let jump: Request = serde_json::from_value(json!({
            "request_id": "request-jump",
            "type": "play_queue_index",
            "index": 79,
        }))
        .unwrap();
        assert!(matches!(jump.command, Command::PlayQueueIndex { index: 79 }));
    }

    #[test]
    fn restore_queue_and_legacy_track_availability_deserialize_cleanly() {
        let restore: Request = serde_json::from_value(json!({
            "request_id": "request-restore",
            "type": "restore_queue",
            "queue": [{
                "uri": "spotify:track:0123456789ABCDEFGHIJKL",
                "duration_ms": 240000
            }],
            "index": 0,
            "position_ms": 42000
        }))
        .unwrap();
        let Command::RestoreQueue {
            queue,
            index,
            position_ms,
        } = restore.command
        else {
            panic!("restore_queue must parse as its dedicated command");
        };
        assert_eq!(index, 0);
        assert_eq!(position_ms, 42_000);
        assert!(!queue[0].unavailable);
        assert!(queue[0].unavailable_reason.is_none());
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

        let catalogue: Request = serde_json::from_value(json!({
            "request_id": "request-catalogue",
            "type": "browse_artist_catalogue",
            "id": "0123456789ABCDEFGHIJKL",
            "release_types": ["albums", "singles"],
        }))
        .unwrap();
        assert!(matches!(
            catalogue.command,
            Command::BrowseArtistCatalogue { offset: 0, limit: 4, .. }
        ));

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

        let liked_songs: Request = serde_json::from_value(json!({
            "request_id": "request-17",
            "type": "browse_liked_songs",
            "cursor": "hm://context-page/collection?offset=50",
        }))
        .unwrap();
        assert!(matches!(
            liked_songs.command,
            Command::BrowseLikedSongs { ref cursor }
                if cursor.as_deref() == Some("hm://context-page/collection?offset=50")
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
            playlists: playlists.clone(),
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
                        "artist_ids": [],
                        "artist_id": "",
                        "album_id": "",
                        "album_name": "",
                        "cover_url": "",
                        "duration_ms": 0
                    }],
                    "albums": [],
                    "artists": [],
                    "playlists": [{
                        "id": "0123456789ABCDEFGHIJKL",
                        "uri": "spotify:playlist:0123456789ABCDEFGHIJKL",
                        "name": "Road Trip",
                        "owner_id": "alice",
                        "owner_name": "",
                        "cover_url": "https://i.scdn.co/image/0123",
                        "track_count": 42
                    }]
                }
            })
        );

        let legacy: SearchBrowse = serde_json::from_value(json!({
            "tracks": [],
            "albums": [],
            "artists": []
        }))
        .unwrap();
        assert!(
            legacy.playlists.is_empty(),
            "old search payloads default the new playlist section"
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
    #[test]
    fn radio_commands_and_payloads_match_the_line_protocol() {
        let radio: Request = serde_json::from_value(json!({
            "request_id": "request-radio",
            "type": "browse_radio",
            "id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            radio.command,
            Command::BrowseRadio { ref id } if id == "0123456789ABCDEFGHIJKL"
        ));

        let artist_radio: Request = serde_json::from_value(json!({
            "request_id": "request-artist-radio",
            "type": "browse_radio",
            "id": "artist:0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            artist_radio.command,
            Command::BrowseRadio { ref id } if id == "artist:0123456789ABCDEFGHIJKL"
        ));

        let recommendations: Request = serde_json::from_value(json!({
            "request_id": "request-recommendations",
            "type": "browse_playlist_recommendations",
            "id": "1123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            recommendations.command,
            Command::BrowsePlaylistRecommendations { ref id }
                if id == "1123456789ABCDEFGHIJKL"
        ));

        let seed = TrackRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:track:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Seed".to_owned(),
            ..TrackRef::default()
        };
        let payload = RadioBrowse {
            seed: seed.clone(),
            tracks: vec![seed],
            seed_kind: "track".to_owned(),
            seed_artist: None,
        };
        let response = serde_json::to_value(BrowseResponse {
            kind: "browse_radio",
            request_id: "request-radio",
            ok: true,
            error: None,
            data: Some(&payload),
        })
        .unwrap();
        assert_eq!(response["type"], "browse_radio");
        assert_eq!(response["data"]["seed"]["name"], "Seed");
        assert_eq!(response["data"]["tracks"][0]["name"], "Seed");
        assert_eq!(response["data"]["seed_kind"], "track");
        assert!(response["data"].get("seed_artist").is_none());

        let artist_payload = RadioBrowse {
            seed: TrackRef {
                id: "1123456789ABCDEFGHIJKL".to_owned(),
                uri: "spotify:track:1123456789ABCDEFGHIJKL".to_owned(),
                name: "Ranked first".to_owned(),
                ..TrackRef::default()
            },
            tracks: Vec::new(),
            seed_kind: "artist".to_owned(),
            seed_artist: Some(ArtistRef {
                id: "artist-id".to_owned(),
                uri: "spotify:artist:artist-id".to_owned(),
                name: "Artist".to_owned(),
                portrait_url: None,
            }),
        };
        let artist_response = serde_json::to_value(BrowseResponse {
            kind: "browse_radio",
            request_id: "request-artist-radio",
            ok: true,
            error: None,
            data: Some(&artist_payload),
        })
        .unwrap();
        assert_eq!(artist_response["data"]["seed_kind"], "artist");
        assert_eq!(artist_response["data"]["seed_artist"]["name"], "Artist");
        assert!(artist_response["data"].get("followers").is_none());
        assert!(artist_response["data"].get("likes").is_none());
        assert!(artist_response["data"].get("listeners").is_none());

        let optional = PlaylistRecommendations {
            playlist_id: "1123456789ABCDEFGHIJKL".to_owned(),
            tracks: Vec::new(),
        };
        let response = serde_json::to_value(BrowseResponse {
            kind: "browse_playlist_recommendations",
            request_id: "request-recommendations",
            ok: true,
            error: None,
            data: Some(&optional),
        })
        .unwrap();
        assert_eq!(response["data"]["playlist_id"], "1123456789ABCDEFGHIJKL");
        assert_eq!(response["data"]["tracks"], json!([]));
    }
}
