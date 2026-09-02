use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ms: u32,
    pub end_ms: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoopRange {
    pub start_ms: u32,
    pub end_ms: u32,
    pub play_count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrackEdit {
    pub cuts: Vec<TimeRange>,
    pub loop_range: Option<LoopRange>,
}

impl TrackEdit {
    pub fn is_empty(&self) -> bool {
        self.cuts.is_empty() && self.loop_range.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrackEditDefinition {
    pub track_id: String,
    pub duration_ms: u32,
    #[serde(flatten)]
    pub edit: TrackEdit,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrackEditStatus {
    pub definition: Option<TrackEditDefinition>,
    pub enabled: bool,
    pub excluded_from_automatic_playback: bool,
}

/// Canonical full-track waveform envelope at a fixed 1 ms bin interval.
/// `peaks_base64` holds packed little-endian `(min_i16, max_i16)` pairs —
/// exactly `bin_count = ceil(duration_ms / interval_ms)` pairs, 4 bytes each.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrackWaveform {
    pub track_id: String,
    pub duration_ms: u32,
    pub interval_ms: u16,
    pub bin_count: u32,
    pub peaks_base64: String,
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
    /// Original source duration. Cuts do not change queue metadata; transport
    /// events expose the compiled duration separately.
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
    /// Whether this track's audio is already on disk in the app-owned
    /// librespot cache, so playing it costs no download.
    ///
    /// Answered where the track's metadata is parsed, because that is the one
    /// place the file ids are already in hand: knowing this anywhere else would
    /// mean a second metadata fetch per track. It is a snapshot taken at browse
    /// time, not a subscription — a track that finishes caching while a list is
    /// on screen is marked on the next browse of that list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cached: bool,
    /// Compact source context carried with a queue item into listening history
    /// (for example `playlist:<id>` or `liked`). Empty means the caller did
    /// not know the source; queue commands may provide a fallback.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    /// Immutable playback edit resolved when this item enters a queue. Browse
    /// results leave it absent; queue restore re-resolves it against the live
    /// edit store before mapping the saved transport position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_edit: Option<TrackEdit>,
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
            cached: false,
            context: String::new(),
            effective_edit: None,
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
        /// Position in the compiled transport timeline.
        position_ms: u32,
        #[serde(default)]
        context: String,
        /// When true, choose the first eligible row at or after `index`,
        /// wrapping once. Header Play/Shuffle uses this automatic path;
        /// direct row playback leaves it false.
        #[serde(default)]
        automatic_start: bool,
    },
    /// Installs a paused queue/playhead without asking librespot to load audio.
    /// Snapshot positions are in the compiled transport timeline and are
    /// mapped only after the live edit definitions have been resolved.
    RestoreQueue {
        queue: Vec<TrackRef>,
        index: usize,
        position_ms: u32,
        #[serde(default)]
        context: String,
        /// The editor preview lease this restore is allowed to tear down.
        ///
        /// Startup restores omit this field and retain the original
        /// unconditional restore semantics with lease ID zero.
        #[serde(default)]
        preview_lease_id: u64,
        /// When true, discard the restore if a real queue has already
        /// replaced the editor preview.
        #[serde(default)]
        only_if_preview: bool,
        /// When true, restore the queue's prior playing intent through the
        /// normal play path instead of leaving it paused.
        #[serde(default)]
        resume_playing: bool,
    },
    /// Replaces the queue with one track carrying the editor's exact draft.
    /// The position remains in the original source timeline.
    PreviewTrackEdit {
        track: TrackRef,
        cuts: Vec<TimeRange>,
        #[serde(default)]
        loop_range: Option<LoopRange>,
        position_ms: u32,
        /// Process-local owner of this editor preview attempt.
        preview_lease_id: u64,
    },
    PlayQueueIndex {
        index: usize,
    },
    Play,
    Pause,
    Next,
    Previous,
    Seek {
        /// Position in the compiled transport timeline.
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
    SetPlaybackSpeed {
        speed: f32,
    },
    AddQueue {
        track: TrackRef,
        #[serde(default)]
        context: String,
    },
    AddQueueBatch {
        tracks: Vec<TrackRef>,
        #[serde(default)]
        context: String,
    },
    RemoveQueue {
        index: usize,
    },
    MoveQueue {
        from: usize,
        to: usize,
    },
    GetTrackWaveform {
        track_id: String,
    },
    CancelTrackWaveform {
        track_id: String,
    },
    GetTrackEdit {
        track_id: String,
        #[serde(default)]
        playlist_id: Option<String>,
    },
    SaveTrackEdit {
        track_id: String,
        duration_ms: u32,
        cuts: Vec<TimeRange>,
        #[serde(default)]
        loop_range: Option<LoopRange>,
    },
    DeleteTrackEdit {
        track_id: String,
    },
    /// Skips or restores one track during automatic playback of a playlist.
    /// This preference is independent of any persisted track edit.
    SetPlaylistTrackExcluded {
        playlist_id: String,
        track_id: String,
        excluded: bool,
    },
    SetPlaylistTrackEditEnabled {
        playlist_id: String,
        track_id: String,
        enabled: bool,
    },
    /// One newest-first, bounded snapshot of local listening history. This is
    /// deliberately available before Spotify authentication is ready.
    GetHistory,
    /// Removes all finalized and in-progress local listening history rows.
    ClearHistory,
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
    /// Verified official `Written by <artist>` playlist discovery. The
    /// canonical artist id and display name come from the preceding artist
    /// browse, keeping this optional enhancement out of that critical path.
    /// Responded to with a `browse_artist_songwriter` message.
    BrowseArtistSongwriter {
        id: String,
        name: String,
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
    /// The user's Saved Tracks as bare URIs, one page per response. Responded
    /// to with a `browse_liked_uris` message.
    BrowseLikedUris {
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
    /// One authenticated, on-demand Canvas lookup for a track URI/id.
    /// Responded to with a `browse_canvas` message. No browse path invokes
    /// this implicitly: the panel asks only while animated Canvas is enabled.
    BrowseCanvas {
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
    /// Moves one track so that it lands at index `to` of the resulting list
    /// (the engine converts the final position into the playlist4 MOV wire's
    /// insert-before form). Responded to with an `edit_reorder_playlist_tracks`
    /// message.
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
    /// Enables or disables track-gain volume normalisation (attenuation-only,
    /// ReplayGain-style constant gain from Spotify's embedded loudness tags).
    /// The engine rebuilds its player so a live session picks the change up
    /// immediately; without a session it applies at the next successful login.
    SetNormalisation {
        enabled: bool,
    },
}

/// One logical local listening-history row.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HistoryRow {
    pub track_id: String,
    /// Unix epoch milliseconds at the first actual Playing event.
    pub started_at: i64,
    pub ms_played: u64,
    pub completed: bool,
    pub context: String,
}

/// A history row plus sanitized track metadata for rendering and replay.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct HistoryItem {
    #[serde(flatten)]
    pub row: HistoryRow,
    pub track: TrackRef,
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

fn decode_html_entity(value: &str) -> Option<String> {
    let named = match value.to_ascii_lowercase().as_str() {
        "amp" => Some("&"),
        "apos" => Some("'"),
        "gt" => Some(">"),
        "lt" => Some("<"),
        "nbsp" => Some(" "),
        "quot" => Some("\""),
        "copy" => Some("©"),
        "reg" => Some("®"),
        "trade" => Some("™"),
        "hellip" => Some("…"),
        "ndash" => Some("–"),
        "mdash" => Some("—"),
        "lsquo" => Some("‘"),
        "rsquo" => Some("’"),
        "ldquo" => Some("“"),
        "rdquo" => Some("”"),
        "bull" => Some("•"),
        "middot" => Some("·"),
        "laquo" => Some("«"),
        "raquo" => Some("»"),
        "cent" => Some("¢"),
        "pound" => Some("£"),
        "yen" => Some("¥"),
        "euro" => Some("€"),
        "sect" => Some("§"),
        "para" => Some("¶"),
        "plusmn" => Some("±"),
        "times" => Some("×"),
        "divide" => Some("÷"),
        "micro" => Some("µ"),
        "deg" => Some("°"),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named.to_owned());
    }

    let code = value
        .strip_prefix("#x")
        .or_else(|| value.strip_prefix("#X"))
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .or_else(|| {
            value
                .strip_prefix('#')
                .and_then(|decimal| decimal.parse().ok())
        });
    code.and_then(char::from_u32)
        .filter(|character| *character != '\0')
        .map(|character| character.to_string())
}

fn decode_html_entities(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0usize;
    while cursor < value.len() {
        let Some(relative_ampersand) = value[cursor..].find('&') else {
            decoded.push_str(&value[cursor..]);
            break;
        };
        let ampersand = cursor + relative_ampersand;
        decoded.push_str(&value[cursor..ampersand]);
        let Some(relative_semicolon) = value[ampersand + 1..].find(';') else {
            decoded.push_str(&value[ampersand..]);
            break;
        };
        let semicolon = ampersand + 1 + relative_semicolon;
        let entity = &value[ampersand + 1..semicolon];
        let valid = !entity.is_empty()
            && entity.len() <= 32
            && entity.chars().all(|character| {
                !character.is_whitespace() && !matches!(character, '&' | '<' | '>')
            });
        if valid {
            if let Some(replacement) = decode_html_entity(entity) {
                decoded.push_str(&replacement);
            } else {
                decoded.push_str(&value[ampersand..=semicolon]);
            }
        } else {
            decoded.push_str(&value[ampersand..=semicolon]);
        }
        cursor = semicolon + 1;
    }
    decoded
}

fn normalize_plain_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for (index, word) in value.split_whitespace().enumerate() {
        let starts_with_punctuation = word
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b',' | b'.' | b';' | b':' | b'!' | b'?'));
        if index > 0 && !starts_with_punctuation {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn strip_playlist_markup(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    let mut cursor = 0usize;
    while cursor < value.len() {
        let Some(relative_open) = value[cursor..].find('<') else {
            text.push_str(&value[cursor..]);
            break;
        };
        let open = cursor + relative_open;
        text.push_str(&value[cursor..open]);
        if value[open..].starts_with("<!--") {
            let Some(relative_end) = value[open + 4..].find("-->") else {
                text.push_str(&value[open..]);
                break;
            };
            text.push(' ');
            cursor = open + 4 + relative_end + 3;
            continue;
        }
        let Some(relative_close) = value[open + 1..].find('>') else {
            // A malformed/incomplete tag is still user-visible text. Dropping
            // the rest of a description here would corrupt otherwise useful
            // copy, and Svelte escapes it safely when rendered.
            text.push_str(&value[open..]);
            break;
        };
        text.push(' ');
        cursor = open + 1 + relative_close + 1;
    }
    text
}

fn contains_complete_playlist_markup(value: &str) -> bool {
    let mut cursor = 0usize;
    while let Some(relative_open) = value[cursor..].find('<') {
        let open = cursor + relative_open;
        let after_open = &value[open + 1..];
        if after_open
            .strip_prefix("!--")
            .is_some_and(|comment| comment.contains("-->"))
        {
            return true;
        }

        let candidate = after_open.strip_prefix('/').unwrap_or(after_open);
        let Some(first) = candidate.chars().next() else {
            return false;
        };
        if first.is_ascii_alphabetic() && candidate.find('>').is_some() {
            return true;
        }
        cursor = open + 1;
    }
    false
}

/// Removes markup from playlist descriptions before they cross the engine
/// boundary. Spotify stores descriptions as rich text (usually a short HTML
/// fragment); cards render them as plain text, never as `{@html}`. Keeping the
/// normalisation here also means cached/Tauri payloads have one stable shape.
pub fn sanitize_playlist_description(value: &str) -> String {
    let text = decode_html_entities(&strip_playlist_markup(value));
    normalize_plain_text(&text)
}

/// Normalises a description that has already crossed the engine boundary.
///
/// Engine payloads are canonical plain text, so decoding entities again would
/// turn a literal `&lt;tag&gt;` into markup and can erase it on a later card
/// render. Tauri/cache callers still need to accept older raw fragments. Only
/// a syntactically plausible complete tag or comment selects the full
/// sanitizer: ordinary comparison text such as `under < 3 min > classics`
/// remains canonical plain text.
pub fn normalize_canonical_playlist_description(value: &str) -> String {
    if contains_complete_playlist_markup(value) {
        sanitize_playlist_description(value)
    } else {
        normalize_plain_text(value)
    }
}

/// Playlist reference inside browse responses. `owner_id` is the owning user's
/// Spotify username; `owner_name` is their display name when the source carries
/// it (the rootlist only exposes usernames, so it is empty there).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistRef {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// Plain-text description from search/playlist metadata, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub owner_id: String,
    pub owner_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u32>,
}
/// The verified official playlist and the first playable rows returned by the
/// separate artist songwriter request. Track order is the playlist's source
/// order; it is not a popularity ranking.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SongwriterPlaylist {
    pub playlist: PlaylistRef,
    pub tracks: Vec<TrackRef>,
}

impl Default for SongwriterPlaylist {
    fn default() -> Self {
        Self {
            playlist: PlaylistRef {
                id: String::new(),
                uri: String::new(),
                name: String::new(),
                description: None,
                owner_id: String::new(),
                owner_name: String::new(),
                cover_url: None,
                track_count: None,
            },
            tracks: Vec::new(),
        }
    }
}

/// Payload of a successful [`Command::BrowsePlaylist`] response. `revision`
/// is the playlist4 revision hex-encoded (the value Web API edits call the
/// snapshot id).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistBrowse {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// Plain-text playlist description from playlist metadata, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub owner_id: String,
    pub owner_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    pub tracks: Vec<TrackRef>,
    /// Track ids skipped by automatic playback in this playlist. Direct row
    /// playback remains available for every row, including these ids.
    #[serde(default)]
    pub excluded_track_ids: Vec<String>,
}

/// A playlist-like radio context. `tracks[0]` is always the first playable
/// item in server rank order; recommendations follow without repeats.
///
/// `seed_kind` is `"track"` for the inspired-by song route and `"artist"` for
/// the Apollo artist station route. Artist radio has no playable artist seed,
/// so `seed` is the station's first ranked track and `seed_artist` carries the
/// label shown by the client. Track radio may also carry the official cover of
/// the inspired-by playlist in `cover_url`.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
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
    /// Largest artist-page avatar supplied by `visuals.avatarImage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_image_url: Option<String>,
    /// Every editorial gallery image supplied for the artist biography, in the
    /// order the service ranks them. An artist can publish more than one — the
    /// live overview for Taylor Swift carries eighteen — so this is a list
    /// rather than the first of them.
    pub biography_image_urls: Vec<String>,
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
    /// Server-ranked playlists from `relatedContent.discoveredOnV2`.
    pub discovered_on: Vec<PlaylistRef>,
    /// Artist-owned playlists from `profile.playlistsV2`.
    pub artist_playlists: Vec<PlaylistRef>,
    /// Whatever the artist pinned to the top of their page — see [`ArtistPick`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_pick: Option<ArtistPick>,
}

/// The item an artist pinned to the top of their page, and the note they
/// wrote alongside it.
///
/// This used to be an `Option<PlaylistRef>` and the doc comment said so
/// outright: "other item kinds stay omitted because this browse contract only
/// carries playlist cards". `profile.pinnedItem.itemV2` is a union, and a
/// pinned SINGLE is at least as common as a pinned playlist, so the contract
/// was dropping the ordinary case and keeping the rarer one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtistPick {
    /// The artist's own words about the pick. Frequently absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub item: ArtistPickItem,
}

/// The three kinds `pinnedItem.itemV2` can resolve to, tagged rather than
/// modelled as three optional fields: exactly one is present, always, and a
/// shape that cannot express "two at once" is the one that matches the source.
/// The tag survives to the UI because the card's affordance depends on it — a
/// pinned track plays, a pinned album or playlist opens.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ArtistPickItem {
    Playlist(PlaylistRef),
    Album(AlbumRef),
    Track(TrackRef),
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

/// The user's Saved Tracks as bare URIs, for callers that need membership
/// rather than playable rows. One context round trip per page; unlike
/// [`LikedSongsPage`] it never pays for per-track metadata batches.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LikedUrisPage {
    pub uris: Vec<String>,
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

/// The official Spotify Canvas video attached to one track.
///
/// The endpoint can also return image/GIF records; the engine deliberately
/// drops those and exposes only the HTTPS `canvaz.scdn.co` video URL plus the
/// source enum, so callers cannot accidentally render an invented fallback.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Canvas {
    pub url: String,
    #[serde(rename = "type")]
    pub canvas_type: String,
}

/// The single best answer to a query, whatever kind of thing it turns out
/// to be.
///
/// The four result groups are each ranked only *within* their own kind, so
/// "the first artist" answers a question nobody asked: a search for `top 50`
/// has no artist worth naming, and `Bohemian Rhapsody` is a song. The
/// `searchDesktop` response already carries a cross-kind ranking in
/// `topResultsV2` — the same one the official client puts in this slot — and
/// it costs nothing, because it travels in the response we were already
/// asking for.
///
/// Kinds this app has no destination for (podcasts, episodes, users) are
/// dropped rather than shown, so the field is an `Option`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SearchTopRef {
    Track(TrackRef),
    Album(AlbumRef),
    Artist(ArtistRef),
    Playlist(PlaylistRef),
}

/// Payload of a successful [`Command::BrowseSearch`] response.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SearchBrowse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<SearchTopRef>,
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
    /// Draft editor previews are live transport state but never durable queue
    /// state. The Tauri supervisor uses this bit to keep crash/quit restore
    /// snapshots on the last real queue.
    pub preview: bool,
    /// Position and duration in the compiled transport timeline. Queue row
    /// durations and edit ranges remain in original-source coordinates.
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub playback_speed: f32,
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
    /// Compiled transport position and one-pass compiled duration.
    pub position_ms: u32,
    pub duration_ms: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ArtistRef, AuthState, BrowseResponse, Command, LoopRange, PlaylistRecommendations,
        PlaylistRef, PositionEvent, RadioBrowse, RepeatMode, Request, Response, SearchBrowse,
        SongwriterPlaylist, StateEvent, TrackRef, normalize_canonical_playlist_description,
        sanitize_playlist_description,
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
    fn play_queue_automatic_start_defaults_false_and_accepts_true() {
        let without_flag: Request = serde_json::from_value(json!({
            "request_id": "request-play-automatic-default",
            "type": "play_queue",
            "queue": [{
                "id": "0123456789ABCDEFGHIJKL",
                "uri": "spotify:track:0123456789ABCDEFGHIJKL",
                "duration_ms": 180000
            }],
            "index": 0,
            "position_ms": 0,
            "context": "playlist:playlist"
        }))
        .unwrap();
        let Command::PlayQueue {
            queue,
            index,
            position_ms,
            context,
            automatic_start,
        } = without_flag.command
        else {
            panic!("play_queue must parse as its dedicated command");
        };
        assert_eq!(queue.len(), 1);
        assert_eq!(index, 0);
        assert_eq!(position_ms, 0);
        assert_eq!(context, "playlist:playlist");
        assert!(!automatic_start, "omitted flag defaults to direct playback");

        let with_flag: Request = serde_json::from_value(json!({
            "request_id": "request-play-automatic",
            "type": "play_queue",
            "queue": [],
            "index": 0,
            "position_ms": 0,
            "automatic_start": true
        }))
        .unwrap();
        assert!(matches!(
            with_flag.command,
            Command::PlayQueue {
                automatic_start: true,
                ..
            }
        ));
    }

    #[test]
    fn playlist_track_exclusion_command_deserializes_exact_fields() {
        let request: Request = serde_json::from_value(json!({
            "request_id": "request-exclusion",
            "type": "set_playlist_track_excluded",
            "playlist_id": "playlist-id",
            "track_id": "track-id",
            "excluded": true
        }))
        .unwrap();

        assert!(matches!(
            request.command,
            Command::SetPlaylistTrackExcluded {
                playlist_id,
                track_id,
                excluded: true,
            } if playlist_id == "playlist-id" && track_id == "track-id"
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
            preview: false,
            position_ms: 7_500,
            duration_ms: 123_456,
            volume: 42,
            shuffle: false,
            repeat: RepeatMode::Context,
            playback_speed: 1.0,
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
            preview: false,
            position_ms: 0,
            duration_ms: 0,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            playback_speed: 1.0,
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
    fn get_history_is_a_zero_argument_command() {
        let request: Request = serde_json::from_value(json!({
            "request_id": "request-history",
            "type": "get_history",
        }))
        .unwrap();
        assert!(matches!(request.command, Command::GetHistory));
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
    fn set_normalisation_command_deserializes_from_the_line_protocol() {
        let request: Request = serde_json::from_value(json!({
            "request_id": "request-norm",
            "type": "set_normalisation",
            "enabled": true,
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            Command::SetNormalisation { enabled: true }
        ));

        let request: Request = serde_json::from_value(json!({
            "request_id": "request-norm-off",
            "type": "set_normalisation",
            "enabled": false,
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            Command::SetNormalisation { enabled: false }
        ));
    }

    #[test]
    fn queue_index_jump_does_not_resend_the_queue() {
        let jump: Request = serde_json::from_value(json!({
            "request_id": "request-jump",
            "type": "play_queue_index",
            "index": 79,
        }))
        .unwrap();
        assert!(matches!(
            jump.command,
            Command::PlayQueueIndex { index: 79 }
        ));
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
            preview_lease_id,
            only_if_preview,
            resume_playing,
            ..
        } = restore.command
        else {
            panic!("restore_queue must parse as its dedicated command");
        };
        assert!(!only_if_preview);
        assert!(!resume_playing);
        assert_eq!(preview_lease_id, 0);
        assert_eq!(index, 0);
        assert_eq!(position_ms, 42_000);
        assert!(!queue[0].unavailable);
        assert!(queue[0].unavailable_reason.is_none());
    }

    #[test]
    fn restore_queue_accepts_preview_guard_and_resume_flags() {
        let request: Request = serde_json::from_value(json!({
            "request_id": "request-restore-preview",
            "type": "restore_queue",
            "queue": [],
            "index": 0,
            "position_ms": 0,
            "preview_lease_id": 9,
            "only_if_preview": true,
            "resume_playing": true
        }))
        .unwrap();

        assert!(matches!(
            request.command,
            Command::RestoreQueue {
                only_if_preview: true,
                resume_playing: true,
                ..
            }
        ));
    }

    #[test]
    fn preview_track_edit_deserializes_the_exact_draft_shape() {
        let preview: Request = serde_json::from_value(json!({
            "request_id": "request-preview",
            "type": "preview_track_edit",
            "track": {
                "id": "0123456789ABCDEFGHIJKL",
                "uri": "spotify:track:0123456789ABCDEFGHIJKL",
                "duration_ms": 240000
            },
            "cuts": [
                {"start_ms": 1000, "end_ms": 2500},
                {"start_ms": 10000, "end_ms": 12000}
            ],
            "loop_range": {"start_ms": 5000, "end_ms": 9000, "play_count": 2},
            "preview_lease_id": 7,
            "position_ms": 42000
        }))
        .unwrap();
        let Command::PreviewTrackEdit {
            track,
            cuts,
            loop_range,
            position_ms,
            preview_lease_id,
        } = preview.command
        else {
            panic!("preview_track_edit must parse as its dedicated command");
        };
        assert_eq!(track.id, "0123456789ABCDEFGHIJKL");
        assert_eq!(cuts.len(), 2);
        assert_eq!(
            loop_range,
            Some(LoopRange {
                start_ms: 5_000,
                end_ms: 9_000,
                play_count: 2,
            })
        );
        assert_eq!(position_ms, 42_000);
        assert_eq!(preview_lease_id, 7);
    }

    #[test]
    fn old_infinite_loop_objects_without_play_count_are_rejected() {
        let error = serde_json::from_value::<Request>(json!({
            "request_id": "request-old-loop",
            "type": "preview_track_edit",
            "track": {
                "id": "0123456789ABCDEFGHIJKL",
                "uri": "spotify:track:0123456789ABCDEFGHIJKL",
                "duration_ms": 240000
            },
            "cuts": [],
            "loop_range": {"start_ms": 5000, "end_ms": 9000},
            "preview_lease_id": 1,
        }))
        .expect_err("the old infinite loop shape must not deserialize");
        assert!(error.to_string().contains("play_count"));
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
        let songwriter: Request = serde_json::from_value(json!({
            "request_id": "request-songwriter",
            "type": "browse_artist_songwriter",
            "id": "0123456789ABCDEFGHIJKL",
            "name": "Artist",
        }))
        .unwrap();
        assert!(matches!(
            songwriter.command,
            Command::BrowseArtistSongwriter { ref id, ref name }
                if id == "0123456789ABCDEFGHIJKL" && name == "Artist"
        ));

        let catalogue: Request = serde_json::from_value(json!({
            "request_id": "request-catalogue",
            "type": "browse_artist_catalogue",
            "id": "0123456789ABCDEFGHIJKL",
            "release_types": ["albums", "singles"],
        }))
        .unwrap();
        assert!(matches!(
            catalogue.command,
            Command::BrowseArtistCatalogue {
                offset: 0,
                limit: 4,
                ..
            }
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
    fn playlist_descriptions_are_reduced_to_plain_text() {
        assert_eq!(
            sanitize_playlist_description("<p>Focus&nbsp;&amp; flow <a href='#'>today</a></p>"),
            "Focus & flow today"
        );
        assert_eq!(
            sanitize_playlist_description(
                "<p>The essential <b>tracks</b>, all in <a href='#'>one playlist</a>.</p>",
            ),
            "The essential tracks, all in one playlist."
        );
        assert_eq!(sanitize_playlist_description("&#x1F3B5; &#169;"), "🎵 ©");
        assert_eq!(sanitize_playlist_description("  \n\t "), "");
    }

    #[test]
    fn playlist_description_sanitizers_preserve_malformed_and_nested_text() {
        assert_eq!(
            sanitize_playlist_description("<p>&amp;lt;b&amp;gt;</p>"),
            "&lt;b&gt;"
        );
        assert_eq!(
            sanitize_playlist_description("<p>before<!-- hidden > text -->after</p>"),
            "before after"
        );
        assert_eq!(
            sanitize_playlist_description("Keep this <unfinished description"),
            "Keep this <unfinished description"
        );
        assert_eq!(
            normalize_canonical_playlist_description("&lt;b&gt;"),
            "&lt;b&gt;"
        );
        assert_eq!(
            normalize_canonical_playlist_description("<p>Road&nbsp;music</p>"),
            "Road music"
        );
        assert_eq!(
            sanitize_playlist_description("&constructor; &000000000000000000000000000000000000;"),
            "&constructor; &000000000000000000000000000000000000;"
        );
    }

    #[test]
    fn canonical_playlist_description_preserves_literal_comparison_brackets() {
        assert_eq!(
            normalize_canonical_playlist_description("Songs under < 3 min > classics"),
            "Songs under < 3 min > classics"
        );
        assert_eq!(
            normalize_canonical_playlist_description("Keep <unfinished > text"),
            "Keep text"
        );
    }

    #[test]
    fn browse_response_serializes_the_data_envelope() {
        let playlists = vec![PlaylistRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:playlist:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Road Trip".to_owned(),
            description: Some("A road trip playlist.".to_owned()),
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
            ..SearchBrowse::default()
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
                        "description": "A road trip playlist.",
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
                "description": "A road trip playlist.",
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
            preview: false,
            position_ms: 0,
            duration_ms: 0,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            playback_speed: 1.0,
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
            preview: false,
            position_ms: 0,
            duration_ms: 0,
            volume: 50,
            shuffle: false,
            repeat: RepeatMode::Off,
            playback_speed: 1.0,
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
            cover_url: Some("https://i.scdn.co/image/radio-cover".to_owned()),
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
        assert_eq!(
            response["data"]["cover_url"],
            "https://i.scdn.co/image/radio-cover"
        );
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
            cover_url: None,
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
        assert!(artist_response["data"].get("cover_url").is_none());
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

    #[test]
    fn waveform_commands_and_payload_match_the_line_protocol() {
        let get: Request = serde_json::from_value(json!({
            "request_id": "waveform-1",
            "type": "get_track_waveform",
            "track_id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            get.command,
            Command::GetTrackWaveform { track_id }
                if track_id == "0123456789ABCDEFGHIJKL"
        ));

        let cancel: Request = serde_json::from_value(json!({
            "request_id": "waveform-2",
            "type": "cancel_track_waveform",
            "track_id": "0123456789ABCDEFGHIJKL",
        }))
        .unwrap();
        assert!(matches!(
            cancel.command,
            Command::CancelTrackWaveform { track_id }
                if track_id == "0123456789ABCDEFGHIJKL"
        ));

        let payload = super::TrackWaveform {
            track_id: "0123456789ABCDEFGHIJKL".to_owned(),
            duration_ms: 25,
            interval_ms: 1,
            bin_count: 3,
            peaks_base64: "AAD//w==".to_owned(),
        };
        let response = serde_json::to_value(BrowseResponse {
            kind: "get_track_waveform",
            request_id: "waveform-1",
            ok: true,
            error: None,
            data: Some(&payload),
        })
        .unwrap();
        assert_eq!(response["data"]["interval_ms"], 1);
        assert_eq!(response["data"]["bin_count"], 3);
        assert_eq!(response["data"]["peaks_base64"], "AAD//w==");
    }

    #[test]
    fn songwriter_playlist_is_a_standalone_payload() {
        let empty: SongwriterPlaylist = serde_json::from_value(json!({})).unwrap();
        assert!(empty.playlist.id.is_empty());
        assert!(empty.tracks.is_empty());

        let payload = SongwriterPlaylist {
            playlist: PlaylistRef {
                id: "writers".to_owned(),
                uri: "spotify:playlist:writers".to_owned(),
                name: "Written by Artist".to_owned(),
                description: Some("Official".to_owned()),
                owner_id: "spotify".to_owned(),
                owner_name: "Spotify".to_owned(),
                cover_url: None,
                track_count: Some(37),
            },
            tracks: vec![TrackRef {
                id: "track".to_owned(),
                uri: "spotify:track:track".to_owned(),
                name: "Song".to_owned(),
                ..TrackRef::default()
            }],
        };
        let encoded = serde_json::to_value(payload).unwrap();
        assert_eq!(encoded["playlist"]["owner_id"], "spotify");
        assert_eq!(encoded["tracks"][0]["name"], "Song");
    }
}
