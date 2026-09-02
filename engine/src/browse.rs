//! Session-backed Spotify browsing. Private metadata, playlist, Liked Songs,
//! and search requests use librespot's authenticated spclient/login5 clients.
//!
//! Response-mapping notes:
//! - Rootlist: `/playlist/v2/user/{user}/rootlist` answered as
//!   protobuf-JSON with `contents.items`/`contents.metaItems` parallel
//!   arrays (shape cross-checked against other spclient clients, including
//!   mirrorfm's `spotify-private-api` which treats `attributes.picture` as a
//!   raw base64 string). The rootlist carries owner usernames but no owner
//!   display names, so `owner_name` is empty there. Playlist covers come
//!   from the raw `attributes.picture` file id (base64 bytes ->
//!   `https://i.scdn.co/image/{hex}`), with ready-made `pictureSize` URLs as
//!   fallback.
//! - Track/album resolution: the extended-metadata endpoint
//!   (`POST /extended-metadata/v0/extended-metadata`, librespot's
//!   `SpClient::get_extended_metadata(BatchedEntityRequest)`) resolves many
//!   entity URIs per request. Playlist/album/artist contents arrive as bare
//!   URIs and are fetched in ~40-URI batches — never one request per track
//!   (per-URI `Track::get` bursts trip the endpoint's per-request rate
//!   limiter and log hundreds of 'Resource has been exhausted' errors).
//! - Search: the searchview service (Mercury `hm://searchview/km/v4/search`
//!   or HTTP spclient `/searchview/km/v4/search`) is retired — it answers
//!   404 over Mercury and 400 INVALID_ARGUMENT over HTTP (verified live
//!   2026-08-18, both with the session's real country/locale). The official
//!   web/desktop client searches through the pathfinder GraphQL API
//!   (`POST https://api-partner.spotify.com/pathfinder/v2/query`,
//!   `operationName=searchDesktop`, persisted-query `sha256Hash`), which
//!   accepts the same first-party session bearer + client token the engine
//!   already holds (no developer app). The hash rotates; the newest known
//!   hash is tried first and the previous one is the fallback (verified
//!   live: both return results). Parsing stays tolerant: unknown/missing
//!   fields degrade to empty strings, zeros, and empty lists instead of
//!   failing the whole browse.
//! - Artist artwork: the search response carries artist avatars in
//!   `artists.items[].data.visuals.avatarImage.sources` (three sizes,
//!   unsorted) and the artist metadata carries them in `portrait_group`
//!   rather than `portrait`. Both were previously left unread, which is why
//!   every artist rendered as a monogram tile. See [`hit_image`] and
//!   [`artist_portrait`].
//! - Play counts: the dead extended-metadata `STREAM_COUNT` enum entry is not
//!   used by Spotify's client. Album rows come from the persisted pathfinder
//!   `queryAlbumTracks` document. Artist Popular counts travel in the same
//!   `queryArtistOverview` response as the page facts, with `getTrack` retained
//!   only as the graceful fallback when every overview hash has rotated.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use http::{Method, Request};
use librespot_core::cache::Cache;
use librespot_core::error::ErrorKind;
use librespot_core::{FileId, Session, SpotifyId, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track, image::Images};
use librespot_protocol::canvaz;
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use protobuf::{EnumOrUnknown, Message};
use serde::Deserialize;

use renderer_engine::protocol::{
    AlbumRef, ArtistOverview, ArtistPick, ArtistPickItem, ArtistRef, ArtistTopCity, Canvas,
    CreditArtist, CreditRole, PlaylistRecommendations, PlaylistRef, RadioBrowse,
    SongwriterPlaylist, TrackCredits, TrackRef, sanitize_playlist_description,
};

/// Public artwork base; every cover URL is `{COVER_BASE}{40-hex-file-id}`.
const COVER_BASE: &str = "https://i.scdn.co/image/";

/// Entity URIs per extended-metadata POST. The endpoint answers batches of
/// this size comfortably; larger playlists are split into several POSTs so a
/// 1000-track playlist is ~25 requests instead of 1000.
const METADATA_BATCH_SIZE: usize = 40;

/// Retries after the first attempt when a metadata batch POST fails. Items
/// are skipped only once the retries are exhausted.
const METADATA_RETRY_ATTEMPTS: usize = 3;

/// Capped exponential backoff between batch attempts, starting at this many
/// milliseconds and doubling up to [`METADATA_BACKOFF_MAX_MS`].
const METADATA_BACKOFF_BASE_MS: u64 = 250;
const METADATA_BACKOFF_MAX_MS: u64 = 2_000;

/// Retries after the first attempt when a browse request fails: the spclient
/// front door answers transient 5xx (502/503) and the metadata/search
/// services sit behind the same proxy/CDN layer, so a bounded retry absorbs
/// them before an error reaches the UI. Longer than the per-batch metadata
/// schedule because a rootlist/playlist fetch is one round-trip: 5 retries
/// with capped exponential backoff (500 ms doubling to a 4 s cap, at most
/// 11.5 s of sleep) ride out multi-second proxy hiccups while staying inside
/// the UI's 20-second browse round-trip timeout. Transient 502s fail fast
/// (connection error, no body), so the typical retry costs well under a
/// second; only a sustained outage reaches the sleep bound.
const BROWSE_RETRY_ATTEMPTS: usize = 5;
const BROWSE_BACKOFF_BASE_MS: u64 = 500;
const BROWSE_BACKOFF_MAX_MS: u64 = 4_000;

/// Upper bounds mirroring the endpoints' practical limits; larger requests
/// are clamped instead of refused.
const MAX_PLAYLISTS: usize = 1000;
const MAX_SEARCH_LIMIT: usize = 50;
const SONGWRITER_TRACK_LIMIT: usize = 10;
const SONGWRITER_QUERY_PREFIX: &str = "Written by ";

// ---------------------------------------------------------------------------
// metadata conversions
// ---------------------------------------------------------------------------

/// Picks the artwork URL for a metadata image group: the DEFAULT-size image
/// when present, otherwise the first image.
pub fn cover_url(images: &Images) -> Option<String> {
    images
        .iter()
        .find(|image| image.size == librespot_metadata::image::ImageSize::DEFAULT)
        .or_else(|| images.first())
        .and_then(|image| image.id.to_base16().ok())
        .map(|hex| format!("{COVER_BASE}{hex}"))
}

fn largest_cover_url<'a>(
    images: impl IntoIterator<Item = &'a librespot_metadata::image::Image>,
) -> Option<String> {
    images
        .into_iter()
        .max_by_key(|image| {
            i64::from(image.width.max(0)).saturating_mul(i64::from(image.height.max(0)))
        })
        .and_then(|image| image.id.to_base16().ok())
        .map(|hex| format!("{COVER_BASE}{hex}"))
}

fn id_of(uri: &SpotifyUri) -> String {
    uri.to_id().unwrap_or_default()
}

fn uri_of(uri: &SpotifyUri) -> String {
    uri.to_uri().unwrap_or_default()
}

fn playlist_description(value: &str) -> Option<String> {
    let description = sanitize_playlist_description(value);
    (!description.is_empty()).then_some(description)
}

/// Logs, once per run, which audio formats Spotify actually offers this
/// account for a real track.
///
/// This is the only way to answer "can we play lossless" with evidence rather
/// than inference. librespot 0.8.0 cannot *select* FLAC — `Bitrate` is only
/// 96/160/320 and none of `player.rs`'s three preference lists mentions
/// `FLAC_FLAC`, so the selector can never match it — but whether the server
/// offers a FLAC file at all is a separate, account- and client-gated
/// question, and it decides whether patching that selection would achieve
/// anything.
///
/// Answered for *this* client, and the answer is no: every track logged so far
/// comes back with `AAC_24, OGG_VORBIS_96, OGG_VORBIS_160, OGG_VORBIS_320` and
/// no FLAC entry. The file plainly exists — the official app plays lossless —
/// but it is not withheld from the selector so much as absent from the metadata
/// we are served, so patching librespot's preference lists would achieve
/// nothing. Reaching it means presenting as a client Spotify serves it to,
/// which is a far deeper impersonation than this does anywhere else, and may
/// not be reachable at all.
///
/// One line, first resolved track, no per-track cost.
fn log_available_formats_once(track: &Track) {
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        let mut formats: Vec<String> = track
            .files
            .0
            .keys()
            .map(|format| format!("{format:?}"))
            .collect();
        formats.sort();
        eprintln!(
            "audio formats offered for <{}>: {}",
            track.name,
            if formats.is_empty() {
                "none".to_owned()
            } else {
                formats.join(", ")
            }
        );
    });
}

#[derive(Clone, Debug)]
struct AvailabilityPolicy {
    country: String,
    catalogue: String,
    filter_explicit: bool,
}

impl AvailabilityPolicy {
    fn for_session(session: &Session) -> Self {
        Self {
            country: session.country(),
            catalogue: session
                .get_user_attribute("catalogue")
                .map(|value| value.to_string())
                .unwrap_or_else(|| "premium".to_owned()),
            filter_explicit: session.filter_explicit_content(),
        }
    }
}

/// Computes only stable metadata/account restrictions. Loader, key, format,
/// and network errors deliberately do not enter this path.
fn permanent_unavailability(track: &Track, policy: &AvailabilityPolicy) -> Option<String> {
    if track.duration <= 0 {
        return Some("invalid track duration".to_owned());
    }
    if policy.filter_explicit && track.is_explicit {
        return Some("explicit content is filtered".to_owned());
    }
    let now = librespot_core::date::Date::now_utc();
    if now < track.earliest_live_timestamp
        || (!track.availability.is_empty()
            && track
                .availability
                .iter()
                .all(|availability| now < availability.start))
    {
        return Some("not available until its release date".to_owned());
    }
    for restriction in track.restrictions.iter().filter(|restriction| {
        restriction
            .catalogue_strs
            .iter()
            .any(|catalogue| catalogue == &policy.catalogue)
    }) {
        if restriction
            .countries_allowed
            .as_ref()
            .is_some_and(|countries| !countries.iter().any(|country| country == &policy.country))
        {
            return Some("not available in your country".to_owned());
        }
        if restriction
            .countries_forbidden
            .as_ref()
            .is_some_and(|countries| countries.iter().any(|country| country == &policy.country))
        {
            return Some("not available in your country".to_owned());
        }
    }
    // Librespot resolves an alternative lazily and chooses the first one that
    // is playable. No base file is therefore permanent only when there is no
    // alternative to try.
    if track.files.is_empty() && track.alternatives.is_empty() {
        return Some("no playable audio file".to_owned());
    }
    None
}

/// Converts resolved track metadata into the protocol's `TrackRef` shape
/// (identical to the one `play_queue` receives from the UI).
pub fn track_ref(track: &Track) -> TrackRef {
    log_available_formats_once(track);
    TrackRef {
        id: id_of(&track.id),
        uri: uri_of(&track.id),
        name: track.name.clone(),
        artist_names: track
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect(),
        // Same list, same order, same pass: the ids were already parsed here
        // and simply discarded, so every credited artist becomes linkable for
        // no extra request.
        artist_ids: track
            .artists
            .iter()
            .map(|artist| id_of(&artist.id))
            .collect(),
        artist_id: track
            .artists
            .first()
            .map(|artist| id_of(&artist.id))
            .unwrap_or_default(),
        album_id: id_of(&track.album.id),
        album_name: track.album.name.clone(),
        cover_url: cover_url(&track.album.covers).unwrap_or_default(),
        duration_ms: u32::try_from(track.duration).unwrap_or(0),
        play_count: None,
        added_at: None,
        unavailable: false,
        unavailable_reason: None,
        cached: false,
        context: String::new(),
        effective_edit: None,
    }
}

/// Whether this track's audio is already sitting in the app-owned cache.
///
/// THE COST, because this runs per track on every browse: `file_path` is pure
/// string work (librespot hashes the file id into `audio/<xx>/<rest>`), and
/// `fs::exists` is one `GetFileAttributesW` — not an open, which is what
/// `Path::exists` would have been. A track exposes a handful of formats and the
/// scan short-circuits on the first hit, so a fully cached 200-track playlist
/// costs 200 attribute lookups and an uncached one about four times that. On
/// the order of a millisecond, once, on a code path that has just finished a
/// network round trip — against the alternative the brief warned about, which
/// was 200 IPC calls from the UI.
///
/// It is deliberately NOT a directory index with a TTL. An index is faster in
/// the abstract and wrong in practice: it goes stale the moment playback
/// caches something, and the staleness window is exactly when someone is
/// looking at the mark.
///
/// Region-locked tracks carry no files of their own. Their alternative ids are
/// retained in the session index, but ordinary browse never fetches substitute
/// metadata merely to improve a cache badge.
fn track_is_cached(track: &Track, cache: Option<&Cache>) -> bool {
    let Some(cache) = cache else {
        return false;
    };
    any_file_on_disk(track.files.values().copied(), cache)
}

/// `std::fs::exists`, not `Path::exists`: the former is one `GetFileAttributesW`,
/// the latter opens a handle. At a few hundred rows the difference is the whole
/// budget.
fn any_file_on_disk(files: impl IntoIterator<Item = FileId>, cache: &Cache) -> bool {
    files.into_iter().any(|file_id| {
        cache
            .file_path(file_id)
            .is_some_and(|path| std::fs::exists(path).unwrap_or(false))
    })
}

/// What a track's audio is made of, for every track parsed this session.
///
/// A download mark is derived from a track's FILE ids, and those exist only on
/// the metadata payload. They are not on `TrackRef` and not in the on-disk
/// track cache — both of those persist what the UI needs, not what the audio
/// layer needs — so once a payload has been parsed the ids are gone, and
/// answering "is this cached?" anywhere else costs a metadata fetch. That is
/// the reason the mark could only ever be a snapshot taken while browsing, and
/// why playing a song did not light it up until the next browse.
///
/// Keeping the ids makes every later answer pure filesystem. The map is cheap:
/// an id string and a handful of 20-byte file ids per track, so ten thousand
/// tracks sit comfortably under a megabyte. It is memory-only on purpose — a
/// file id outliving the metadata it came from would send us looking for the
/// wrong file, and a browse refills it for free. Growth is bounded anyway
/// ([`FILE_IDS_CAPACITY`]): overwriting a known track is refresh-only, so a
/// long session ages out its stalest parse instead of growing without limit.
static FILE_IDS: LazyLock<RwLock<HashMap<String, TrackFiles>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
/// The id index is process-lifetime state with no explicit retirement, so its
/// worst case is declared here rather than trusted: one entry per distinct
/// track id parsed this session, refreshed whenever that payload reparses.
const FILE_IDS_CAPACITY: usize = 10_000;

/// Canvas answers are intentionally process-memory only. A panel opening can
/// revisit one track several times, but Canvas is not part of ordinary browse
/// payloads and must never become a background scan or disk cache.
const CANVAS_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const CANVAS_CACHE_MAX: usize = 256;

#[derive(Clone)]
struct CanvasCacheEntry {
    fetched_at: Instant,
    value: Option<Canvas>,
}

static CANVAS_CACHE: LazyLock<Mutex<HashMap<String, CanvasCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached_canvas(uri: &str) -> Option<Option<Canvas>> {
    let mut cache = CANVAS_CACHE.lock().ok()?;
    let entry = cache.get(uri)?;
    if entry.fetched_at.elapsed() > CANVAS_CACHE_TTL {
        cache.remove(uri);
        return None;
    }
    Some(entry.value.clone())
}

fn remember_canvas(uri: &str, value: Option<Canvas>) {
    let Ok(mut cache) = CANVAS_CACHE.lock() else {
        return;
    };
    let key_is_absent = !cache.contains_key(uri);
    evict_oldest_for_fresh_key(&mut cache, CANVAS_CACHE_MAX, key_is_absent, |entry| {
        entry.fetched_at
    });
    cache.insert(
        uri.to_owned(),
        CanvasCacheEntry {
            fetched_at: Instant::now(),
            value,
        },
    );
}

/// Explicit logout/session reset hook. Canvas is not persisted across users.
pub fn clear_canvas_cache() {
    if let Ok(mut cache) = CANVAS_CACHE.lock() {
        cache.clear();
    }
}

/// The one bounded-growth rule every browse cache shares: when a fresh key
/// joins a map already at capacity, the stalest resident makes room. Only a
/// genuinely new key can evict — refreshing an existing entry never touches
/// anyone else — so the caller reports whether the key was absent.
fn evict_oldest_for_fresh_key<K, V>(
    entries: &mut HashMap<K, V>,
    capacity: usize,
    key_is_absent: bool,
    fetched_at_of: impl Fn(&V) -> Instant,
) where
    K: Clone + Eq + std::hash::Hash,
{
    if !key_is_absent || entries.len() < capacity {
        return;
    }
    let oldest = entries
        .iter()
        .min_by_key(|(_, entry)| fetched_at_of(entry))
        .map(|(key, _)| key.clone());
    if let Some(oldest) = oldest {
        entries.remove(&oldest);
    }
}

struct TrackFiles {
    /// When the payload behind these ids was parsed. This is the stamp
    /// [`FILE_IDS`] ages entries by, not a freshness signal.
    fetched_at: Instant,
    files: Vec<FileId>,
    /// The recordings librespot may substitute for this one. A region-replaced
    /// track carries no files of its own and plays from one of these, so a
    /// lookup that stops at `files` calls it uncached forever.
    alternatives: Vec<String>,
}

fn remember_track_files(id: &str, track: &Track) {
    if id.is_empty() {
        return;
    }
    let entry = TrackFiles {
        fetched_at: Instant::now(),
        files: track.files.values().copied().collect(),
        alternatives: track.alternatives.0.iter().map(id_of).collect(),
    };
    if let Ok(mut index) = FILE_IDS.write() {
        let key_is_absent = !index.contains_key(id);
        evict_oldest_for_fresh_key(&mut index, FILE_IDS_CAPACITY, key_is_absent, |entry| {
            entry.fetched_at
        });
        index.insert(id.to_owned(), entry);
    }
}

/// One level of alternative-chasing, and one only: substitutes do not chain in
/// practice, and an unbounded walk over attacker-shaped data is not worth the
/// two lines it would save.
fn resolve_cached(
    id: &str,
    index: &HashMap<String, TrackFiles>,
    cache: &Cache,
    follow: bool,
) -> Option<bool> {
    let entry = index.get(id)?;
    if any_file_on_disk(entry.files.iter().copied(), cache) {
        return Some(true);
    }
    if follow
        && entry
            .alternatives
            .iter()
            .any(|alt| resolve_cached(alt, index, cache, false) == Some(true))
    {
        return Some(true);
    }
    Some(false)
}

/// The subset of `ids` whose audio is already on disk.
///
/// Pure filesystem — one path join and one attribute lookup per file, short
/// circuiting on the first format that hits — so a screenful of rows costs
/// microseconds and a whole playlist costs about a millisecond. Ids the engine
/// has never parsed are simply absent from the result rather than reported as
/// uncached, because "I do not know" and "no" are different answers.
pub fn cached_track_ids(ids: &[String], cache: Option<&Cache>) -> Vec<String> {
    let Some(cache) = cache else {
        return Vec::new();
    };
    let Ok(index) = FILE_IDS.read() else {
        return Vec::new();
    };
    ids.iter()
        .filter(|id| resolve_cached(id, &index, cache, true) == Some(true))
        .cloned()
        .collect()
}

/// Release year of an album, or `None` when the metadata carries no date.
///
/// The date protobuf is optional and librespot maps a missing one to year 0
/// via `get_or_default`, so the zero has to be filtered out here: rendering
/// "0" under a cover is worse than rendering nothing. Only the year is
/// surfaced because librespot substitutes 1 January for an absent month and
/// day, which would make a year-only release indistinguishable from a real
/// New Year's Day one.
fn album_year(album: &Album) -> Option<u32> {
    u32::try_from(album.date.as_utc().year())
        .ok()
        .filter(|year| *year > 0)
}

/// Converts resolved album metadata into the protocol's `AlbumRef` shape.
pub fn album_ref(album: &Album) -> AlbumRef {
    AlbumRef {
        id: id_of(&album.id),
        uri: uri_of(&album.id),
        name: album.name.clone(),
        artist_names: album
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect(),
        artist_ids: album
            .artists
            .iter()
            .map(|artist| id_of(&artist.id))
            .collect(),
        cover_url: cover_url(&album.covers),
        year: album_year(album),
    }
}

// ---------------------------------------------------------------------------
// batched metadata resolution (extended-metadata)
// ---------------------------------------------------------------------------

/// The retry/backoff schedule between extended-metadata batch attempts:
/// `METADATA_RETRY_ATTEMPTS` retries after the first attempt, backing off
/// from `base_ms`, doubling, and capping at `max_ms`. Pure so the schedule is
/// unit-testable.
fn backoff_sequence(attempts: usize, base_ms: u64, max_ms: u64) -> Vec<u64> {
    (0..attempts)
        .map(|attempt| (base_ms << attempt).min(max_ms))
        .collect()
}

/// Extracts the HTTP status code when a librespot error wraps an
/// `HttpClientError::StatusCode` — the shape unmapped server statuses such
/// as 502 Bad Gateway take (reported under [`ErrorKind::Unknown`]).
fn http_status_of(error: &librespot_core::Error) -> Option<u16> {
    use librespot_core::http_client::HttpClientError;
    match error.error.downcast_ref::<HttpClientError>() {
        Some(HttpClientError::StatusCode(code)) => Some(code.as_u16()),
        None => None,
    }
}

/// Whether a failed browse round-trip is transient and worth a bounded
/// retry: network/timeout failures (`Unavailable`, `DeadlineExceeded`) and
/// HTTP server errors (5xx — including 502, which librespot's status
/// mapping leaves as `Unknown` wrapping the status). Client errors (4xx)
/// and other kinds are permanent and fail fast. `status` is the HTTP status
/// when the error wraps one. Pure so the retry decision is unit-testable.
fn browse_error_is_transient(kind: ErrorKind, status: Option<u16>) -> bool {
    match kind {
        ErrorKind::Unavailable | ErrorKind::DeadlineExceeded => true,
        ErrorKind::Unknown => status.is_some_and(|code| (500..=599).contains(&code)),
        _ => false,
    }
}

/// Deduplicates URIs by entity id, keeping first-appearance order
/// (playlists repeat tracks). Pure so the ordering is unit-testable.
fn dedupe_uris<'a>(uris: impl Iterator<Item = &'a SpotifyUri>) -> Vec<SpotifyUri> {
    let mut seen = HashSet::new();
    uris.into_iter()
        .filter(|uri| seen.insert(id_of(uri)))
        .cloned()
        .collect()
}

/// Splits deduplicated URIs into extended-metadata batches of at most
/// `batch_size`. Pure so the chunking is unit-testable.
fn metadata_chunks(uris: &[SpotifyUri], batch_size: usize) -> Vec<&[SpotifyUri]> {
    uris.chunks(batch_size.max(1)).collect()
}

/// Builds the protobuf request for one extended-metadata batch: one
/// `EntityRequest` per URI asking for `kind` extensions, plus the session
/// country in the header (as the official clients send). Pure so the request
/// shape is unit-testable.
fn build_batched_request(
    uris: &[SpotifyUri],
    kind: ExtensionKind,
    country: &str,
) -> BatchedEntityRequest {
    let mut request = BatchedEntityRequest::new();
    request.header.mut_or_insert_default().country = country.to_owned();
    request.entity_request = uris
        .iter()
        .map(|uri| {
            let mut entity = EntityRequest::new();
            entity.entity_uri = uri.to_uri().unwrap_or_default();
            entity.query.push(ExtensionQuery {
                extension_kind: EnumOrUnknown::new(kind),
                ..Default::default()
            });
            entity
        })
        .collect();
    request
}

/// Pulls `(entity_uri, payload bytes)` pairs out of a batch response: only
/// entries whose kind matches the request and whose per-entity status is 200
/// OK count; per-entity failures (unresolvable/removed items) are skipped
/// individually, never as a whole batch. Pure so the mapping is
/// unit-testable.
fn collect_extension_payloads(
    response: &BatchedExtensionResponse,
    kind: ExtensionKind,
) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for array in &response.extended_metadata {
        if array
            .extension_kind
            .enum_value_or(ExtensionKind::UNKNOWN_EXTENSION)
            != kind
        {
            continue;
        }
        for entry in &array.extension_data {
            if entry.header.status_code != 200 {
                continue;
            }
            let Some(payload) = entry.extension_data.as_ref().map(|any| any.value.clone()) else {
                continue;
            };
            out.push((entry.entity_uri.clone(), payload));
        }
    }
    out
}

/// Posts one batch with retries: a failed POST is retried up to
/// [`METADATA_RETRY_ATTEMPTS`] times with capped exponential backoff, and
/// only then reported as an error (the caller skips that batch's items).
/// Batches are posted sequentially so the endpoint never sees a burst.
async fn fetch_extended_batch(
    session: &Session,
    uris: &[SpotifyUri],
    kind: ExtensionKind,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let request = build_batched_request(uris, kind, &session.country());
    let backoffs = backoff_sequence(
        METADATA_RETRY_ATTEMPTS,
        METADATA_BACKOFF_BASE_MS,
        METADATA_BACKOFF_MAX_MS,
    );
    let mut attempt = 0usize;
    loop {
        match session
            .spclient()
            .get_extended_metadata(request.clone())
            .await
        {
            Ok(response) => return Ok(collect_extension_payloads(&response, kind)),
            Err(error) => {
                if attempt >= backoffs.len() {
                    return Err(format!(
                        "metadata batch of {} {kind:?} URIs failed after {} attempts: {error}",
                        uris.len(),
                        attempt + 1,
                    ));
                }
                let backoff = backoffs[attempt];
                eprintln!(
                    "metadata batch of {} {kind:?} URIs failed ({error}); retrying in {backoff} ms (attempt {}/{})",
                    uris.len(),
                    attempt + 1,
                    backoffs.len() + 1,
                );
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                attempt += 1;
            }
        }
    }
}

/// Maps each deduplicated URI's canonical string form to its first-appearance
/// slot in the request list, built once per fetch. The endpoint echoes the
/// exact `entity_uri` strings the request sent, so a batch response is placed
/// with one hash lookup per entry instead of a linear scan per entry. Pure so
/// the mapping is unit-testable.
fn build_uri_index(uris: &[SpotifyUri]) -> HashMap<String, usize> {
    let mut index_by_uri = HashMap::with_capacity(uris.len());
    for (index, uri) in uris.iter().enumerate() {
        // `or_insert` keeps the first slot when two URIs stringify alike,
        // mirroring the first-match semantics of the scan it replaces.
        index_by_uri.entry(uri_of(uri)).or_insert(index);
    }
    index_by_uri
}

/// Resolves extended-metadata entities (tracks, albums, ...) in batches of
/// [`METADATA_BATCH_SIZE`]. URIs are deduplicated by id in first-appearance
/// order; each batch is one POST; items missing from the response or failing
/// per-entity resolution are skipped; a batch is skipped only after its
/// retries with backoff are exhausted.
///
/// A *total* batch failure is an error, never an empty result: returning a
/// success with zero items when the endpoint is failing (e.g. rate-limited)
/// would make the app cache "empty playlist" and show a bare list for a
/// playlist that actually has tracks. Partial failures keep the resolved
/// items and skip the failed batch, like before.
async fn fetch_extended<'a, T>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
    kind: ExtensionKind,
    parse: impl Fn(&str, &[u8]) -> Option<T>,
) -> Result<Vec<T>, String> {
    let unique = dedupe_uris(uris.into_iter());
    if unique.is_empty() {
        return Ok(Vec::new());
    }
    let index_by_uri = build_uri_index(&unique);
    let mut results: Vec<Option<T>> = (0..unique.len()).map(|_| None).collect();
    let mut batches_total = 0usize;
    let mut batches_failed = 0usize;
    for chunk in metadata_chunks(&unique, METADATA_BATCH_SIZE) {
        batches_total += 1;
        match fetch_extended_batch(session, chunk, kind).await {
            Ok(entries) => {
                for (entity_uri, payload) in entries {
                    let Some(&index) = index_by_uri.get(&entity_uri) else {
                        continue;
                    };
                    results[index] = parse(&entity_uri, &payload);
                }
            }
            Err(error) => {
                // Only a fully retried batch failure skips its items.
                batches_failed += 1;
                eprintln!(
                    "skipping {count} unresolvable item(s): {error}",
                    count = chunk.len()
                );
            }
        }
    }
    if batches_failed == batches_total {
        return Err(format!(
            "all {batches_total} {kind:?} metadata batch(es) failed; no items could be resolved"
        ));
    }
    Ok(results.into_iter().flatten().collect())
}

/// Parses one extended-metadata track payload into the protocol's `TrackRef`
/// shape. Returns `None` for unparseable or non-track payloads.
fn parse_track_payload(
    entity_uri: &str,
    payload: &[u8],
    policy: &AvailabilityPolicy,
    cache: Option<&Cache>,
) -> Option<TrackRef> {
    let message = librespot_protocol::metadata::Track::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let track = Track::parse(&message, &uri).ok()?;
    let unavailable_reason = permanent_unavailability(&track, policy);
    let mut reference = track_ref(&track);
    reference.unavailable = unavailable_reason.is_some();
    reference.unavailable_reason = unavailable_reason;
    // Here and nowhere else: this is the one point where a track's file ids
    // exist without a request having been made for them.
    remember_track_files(&reference.id, &track);
    reference.cached = track_is_cached(&track, cache);
    Some(reference)
}

/// Parses one extended-metadata album payload into the protocol's `AlbumRef`
/// shape. Returns `None` for unparseable or non-album payloads.
fn parse_album_payload(entity_uri: &str, payload: &[u8]) -> Option<AlbumRef> {
    let message = librespot_protocol::metadata::Album::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let album = Album::parse(&message, &uri).ok()?;
    Some(album_ref(&album))
}

/// Resolves a batch of track URIs into `TrackRef`s via the extended-metadata
/// endpoint: URIs are deduplicated by id (playlists repeat tracks), fetched
/// in ~40-URI batches in first-appearance order, and items that fail to
/// resolve (episodes, local files, removed tracks) are skipped. No per-item or
/// substitute follow-up requests are made from this response-critical path.
pub async fn fetch_tracks<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Result<Vec<TrackRef>, String> {
    let policy = AvailabilityPolicy::for_session(session);
    let cache = session.cache().cloned();
    fetch_extended(
        session,
        uris,
        ExtensionKind::TRACK_V4,
        |entity_uri, payload| parse_track_payload(entity_uri, payload, &policy, cache.as_deref()),
    )
    .await
}

/// Playlist item attributes carry the only trustworthy "added" timestamp.
/// A protobuf default of zero means the field was absent, not January 1970,
/// so it must remain missing in the browse payload.
fn playlist_added_at(timestamp_ms: i64) -> Option<i64> {
    (timestamp_ms > 0).then_some(timestamp_ms)
}

/// Maps resolved track metadata back onto playlist items while preserving the
/// source order and duplicates. Extended metadata itself deduplicates requests,
/// so the playlist layer has to restore those source positions afterwards.
fn playlist_tracks_from_resolved(
    items: &[(SpotifyUri, Option<i64>)],
    resolved: Vec<TrackRef>,
) -> Vec<TrackRef> {
    let by_uri: HashMap<String, TrackRef> = resolved
        .into_iter()
        .map(|track| (track.uri.clone(), track))
        .collect();
    items
        .iter()
        .filter_map(|(uri, added_at)| {
            let mut track = by_uri.get(&uri_of(uri))?.clone();
            track.added_at = *added_at;
            Some(track)
        })
        .collect()
}

fn limited_playlist_items(
    items: &[(SpotifyUri, Option<i64>)],
    limit: usize,
) -> Vec<(SpotifyUri, Option<i64>)> {
    items.iter().take(limit).cloned().collect()
}

/// Resolves playlist items while preserving Spotify's item order and
/// duplicates. Extended metadata intentionally deduplicates requests, so the
/// playlist layer maps each item back onto its resolved metadata and attaches
/// that item's own optional added timestamp.
async fn fetch_playlist_tracks(
    session: &Session,
    items: &[(SpotifyUri, Option<i64>)],
) -> Result<Vec<TrackRef>, String> {
    let resolved = fetch_tracks(session, items.iter().map(|(uri, _)| uri)).await?;
    Ok(playlist_tracks_from_resolved(items, resolved))
}

/// Resolves only the first `limit` source items. This is used for compact
/// artist-page shelves so a large playlist never causes a full-list metadata
/// request merely to render ten rows.
async fn fetch_playlist_tracks_limited(
    session: &Session,
    items: &[(SpotifyUri, Option<i64>)],
    limit: usize,
) -> Result<Vec<TrackRef>, String> {
    let limited = limited_playlist_items(items, limit);
    let resolved = fetch_tracks(session, limited.iter().map(|(uri, _)| uri)).await?;
    Ok(playlist_tracks_from_resolved(&limited, resolved))
}

// ---------------------------------------------------------------------------
// radio and playlist recommendations
// ---------------------------------------------------------------------------

const MAX_RADIO_RECOMMENDATIONS: usize = 50;
const MAX_PLAYLIST_RECOMMENDATIONS: usize = 10;

/// Apollo returns a broad ranked candidate set, but playlist recommendations
/// only need enough metadata to fill the visible ten-item shelf. Keeping this
/// bound below one metadata batch preserves rank while avoiding needless
/// resolutions for candidates that can never be shown.
const MAX_PLAYLIST_METADATA_CANDIDATES: usize = 20;

const APOLLO_REQUEST_COUNT: usize = 50;

enum InspiredBySource {
    Tracks(Vec<SpotifyUri>),
    Playlist(SpotifyUri),
}
enum RadioSeed {
    Track(SpotifyUri),
    Artist(SpotifyUri),
}

/// The frontend keeps one `radio` route and prefixes artist seeds so the
/// command/model stay shared without making a track id ambiguous.
fn parse_radio_seed(raw: &str) -> Result<RadioSeed, String> {
    if let Some(id) = raw.strip_prefix("artist:") {
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
            .map_err(|error| format!("invalid radio seed artist id: {error}"))?;
        if !matches!(uri, SpotifyUri::Artist { .. }) {
            return Err("radio seed artist id was not an artist URI".to_owned());
        }
        return Ok(RadioSeed::Artist(uri));
    }
    track_uri(&format!("spotify:track:{raw}"))
        .map(RadioSeed::Track)
        .ok_or_else(|| "invalid radio seed track id".to_owned())
}

fn track_uri(raw: &str) -> Option<SpotifyUri> {
    let uri = SpotifyUri::from_uri(raw).ok()?;
    matches!(uri, SpotifyUri::Track { .. }).then_some(uri)
}

fn playlist_uri_in_json(value: &serde_json::Value) -> Option<SpotifyUri> {
    match value {
        serde_json::Value::String(raw) => {
            let uri = SpotifyUri::from_uri(raw).ok()?;
            matches!(uri, SpotifyUri::Playlist { .. }).then_some(uri)
        }
        serde_json::Value::Array(items) => items.iter().find_map(playlist_uri_in_json),
        serde_json::Value::Object(fields) => fields.values().find_map(playlist_uri_in_json),
        _ => None,
    }
}

/// The inspired-by service has shipped two JSON shapes: ranked track URIs in
/// `mediaItems`, and an envelope containing a playlist URI. Unknown fields and
/// malformed media items are ignored; a usable source is still required.
fn parse_inspired_by(bytes: &[u8]) -> Result<InspiredBySource, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid inspired-by JSON: {error}"))?;
    let tracks = value
        .get("mediaItems")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("uri").and_then(serde_json::Value::as_str))
        .filter_map(track_uri)
        .collect::<Vec<_>>();
    if !tracks.is_empty() {
        return Ok(InspiredBySource::Tracks(tracks));
    }
    playlist_uri_in_json(&value)
        .map(InspiredBySource::Playlist)
        .ok_or_else(|| "inspired-by response carried neither tracks nor a playlist".to_owned())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ApolloResponse {
    tracks: Vec<ApolloTrack>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ApolloTrack {
    original_gid: Option<String>,
    uri: Option<String>,
}

fn apollo_gid_uri(raw: &str) -> Option<SpotifyUri> {
    if let Some(uri) = track_uri(raw) {
        return Some(uri);
    }
    let id = match raw.len() {
        22 => SpotifyId::from_base62(raw).ok()?,
        32 => SpotifyId::from_base16(raw)
            .or_else(|_| SpotifyId::from_base16(&raw.to_ascii_lowercase()))
            .ok()?,
        _ => return None,
    };
    Some(SpotifyUri::Track { id })
}

/// Accepts both the current `original_gid` spelling and the older full `uri`.
/// Invalid/non-track rows are skipped individually.
fn parse_apollo_tracks(bytes: &[u8]) -> Result<Vec<SpotifyUri>, String> {
    let response: ApolloResponse = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid radio-Apollo JSON: {error}"))?;
    Ok(response
        .tracks
        .into_iter()
        .filter_map(|track| {
            track
                .original_gid
                .as_deref()
                .and_then(apollo_gid_uri)
                .or_else(|| track.uri.as_deref().and_then(track_uri))
        })
        .collect())
}

fn ranked_radio_tracks(seed: TrackRef, recommendations: Vec<TrackRef>) -> Vec<TrackRef> {
    let mut seen = HashSet::with_capacity(recommendations.len().min(MAX_RADIO_RECOMMENDATIONS) + 1);
    seen.insert(seed.id.clone());
    let mut tracks = Vec::with_capacity(recommendations.len().min(MAX_RADIO_RECOMMENDATIONS) + 1);
    tracks.push(seed);
    tracks.extend(
        recommendations
            .into_iter()
            .filter(|track| seen.insert(track.id.clone()))
            .take(MAX_RADIO_RECOMMENDATIONS),
    );
    tracks
}

async fn track_radio_browse(
    session: &Session,
    seed_uri: SpotifyUri,
) -> Result<RadioBrowse, String> {
    let bytes = session
        .spclient()
        .get_radio_for_track(&seed_uri)
        .await
        .map_err(|error| format!("inspired-by fetch for {seed_uri} failed: {error}"))?;
    let source = parse_inspired_by(&bytes)?;

    let (recommendation_uris, cover_url) = match source {
        InspiredBySource::Tracks(uris) => (uris, None),
        InspiredBySource::Playlist(uri) => {
            let playlist: Playlist = metadata_get(session, &uri, "radio playlist").await?;
            let cover_url = playlist_attributes_cover(&playlist.attributes);
            let recommendation_uris = playlist
                .contents
                .items
                .iter()
                .map(|item| item.id.clone())
                .collect();
            (recommendation_uris, cover_url)
        }
    };
    let seed_id = id_of(&seed_uri);
    let candidates = dedupe_uris(recommendation_uris.iter())
        .into_iter()
        .filter(|candidate| id_of(candidate) != seed_id)
        .take(MAX_RADIO_RECOMMENDATIONS);
    let mut uris = Vec::with_capacity(MAX_RADIO_RECOMMENDATIONS + 1);
    uris.push(seed_uri);
    uris.extend(candidates);

    // Resolve the complete bounded context together: 51 URIs means exactly
    // two existing 40-URI metadata batches, rather than a one-track seed batch
    // followed by two recommendation batches.
    let mut resolved = fetch_tracks(session, uris.iter()).await?;
    if resolved.first().map(|track| track.id.as_str()) != Some(seed_id.as_str()) {
        return Err("radio seed metadata was unavailable".to_owned());
    }
    let seed = resolved.remove(0);
    let tracks = ranked_radio_tracks(seed.clone(), resolved);
    Ok(RadioBrowse {
        seed,
        tracks,
        seed_kind: "track".to_owned(),
        seed_artist: None,
        cover_url,
    })
}

/// Artist radio uses the Apollo station context directly. Its first resolved
/// station item is the playable seed, preserving server rank while the artist
/// reference supplies the route label.
async fn artist_radio_browse(
    session: &Session,
    artist_uri: SpotifyUri,
) -> Result<RadioBrowse, String> {
    let artist: Artist = metadata_get(session, &artist_uri, "artist radio artist").await?;
    let artist_id = id_of(&artist_uri);
    let context = format!("spotify:station:artist:{artist_id}");
    let bytes = session
        .spclient()
        .get_apollo_station(
            "stations",
            &context,
            Some(APOLLO_REQUEST_COUNT),
            Vec::new(),
            false,
        )
        .await
        .map_err(|error| format!("artist radio for {context} failed: {error}"))?;
    let candidates = parse_apollo_tracks(&bytes)?;
    let candidates = dedupe_uris(candidates.iter())
        .into_iter()
        .take(MAX_RADIO_RECOMMENDATIONS);
    let uris = candidates.collect::<Vec<_>>();
    let mut resolved = fetch_tracks(session, uris.iter()).await?;
    if resolved.is_empty() {
        return Err("artist radio returned no playable tracks".to_owned());
    }
    let seed = resolved.remove(0);
    let tracks = ranked_radio_tracks(seed.clone(), resolved);
    let portrait_url = artist_portrait(&artist);
    let seed_artist = ArtistRef {
        id: artist_id,
        uri: uri_of(&artist.id),
        name: artist.name,
        portrait_url,
    };
    Ok(RadioBrowse {
        seed,
        tracks,
        seed_kind: "artist".to_owned(),
        seed_artist: Some(seed_artist),
        cover_url: None,
    })
}

/// Song and artist radio share one route/model but use distinct server
/// contexts. Neither browse path starts playback; the client owns that action.
pub async fn radio_browse(session: &Session, id: &str) -> Result<RadioBrowse, String> {
    match parse_radio_seed(id)? {
        RadioSeed::Track(uri) => track_radio_browse(session, uri).await,
        RadioSeed::Artist(uri) => artist_radio_browse(session, uri).await,
    }
}

/// Manual/lazy recommendations for a playlist. This is deliberately not
/// called by `playlist_browse`: merely opening a playlist must not touch
/// radio-Apollo.
pub async fn playlist_recommendations_browse(
    session: &Session,
    id: &str,
) -> Result<PlaylistRecommendations, String> {
    let uri = playlist_uri(id)?;
    let playlist: Playlist = metadata_get(session, &uri, "playlist").await?;
    let existing: HashSet<String> = playlist
        .contents
        .items
        .iter()
        .map(|item| id_of(&item.id))
        .collect();
    let bytes = session
        .spclient()
        .get_apollo_station(
            "stations",
            &format!("spotify:station:playlist:{id}"),
            Some(APOLLO_REQUEST_COUNT),
            Vec::new(),
            false,
        )
        .await
        .map_err(|error| format!("playlist recommendations for {id} failed: {error}"))?;
    let candidates = parse_apollo_tracks(&bytes)?;
    let candidates = dedupe_uris(candidates.iter())
        .into_iter()
        .filter(|candidate| !existing.contains(&id_of(candidate)))
        .take(MAX_PLAYLIST_METADATA_CANDIDATES)
        .collect::<Vec<_>>();
    let mut tracks = fetch_tracks(session, candidates.iter()).await?;
    tracks.truncate(MAX_PLAYLIST_RECOMMENDATIONS);
    Ok(PlaylistRecommendations {
        playlist_id: id.to_owned(),
        tracks,
    })
}

// ---------------------------------------------------------------------------
// playlist library (rootlist)
// ---------------------------------------------------------------------------

/// Raw protobuf-JSON of the rootlist response. Every field is optional so an
/// unexpected server shape degrades to an empty library instead of an error
/// storm; field names follow the protobuf-JSON camelCase mapping.
#[derive(Default, Deserialize)]
struct RootlistJson {
    #[serde(default)]
    contents: Option<RootlistContentsJson>,
}

#[derive(Default, Deserialize)]
struct RootlistContentsJson {
    #[serde(default)]
    items: Vec<RootlistItemJson>,
    #[serde(default, rename = "metaItems")]
    meta_items: Vec<RootlistMetaItemJson>,
}

#[derive(Default, Deserialize)]
struct RootlistItemJson {
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Default, Deserialize)]
struct RootlistMetaItemJson {
    #[serde(default)]
    length: Option<i64>,
    #[serde(default, rename = "ownerUsername")]
    owner_username: Option<String>,
    #[serde(default)]
    attributes: Option<RootlistAttributesJson>,
}

#[derive(Default, Deserialize)]
struct RootlistAttributesJson {
    #[serde(default)]
    name: Option<String>,
    /// Playlist picture file id, base64 (protobuf-JSON bytes mapping).
    #[serde(default)]
    picture: Option<String>,
    #[serde(default, rename = "pictureSize")]
    picture_size: Vec<PictureSizeJson>,
}

#[derive(Default, Deserialize)]
struct PictureSizeJson {
    #[serde(default)]
    url: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Cover for a rootlist playlist: the raw `picture` file id (base64 bytes)
/// maps 1:1 to the canonical `https://i.scdn.co/image/{hex}` URL and wins
/// over ready-made `pictureSize` URLs (which may be mosaic-style crops).
/// The protobuf-JSON bytes field is standard padded base64; URL-safe (and
/// unpadded) spellings are accepted too so a server variant can never drop
/// every cover.
fn rootlist_cover(attributes: &RootlistAttributesJson) -> Option<String> {
    if let Some(picture) = attributes
        .picture
        .as_deref()
        .filter(|picture| !picture.is_empty())
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(picture)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(picture))
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(picture))
            .ok();
        if let Some(bytes) = decoded.filter(|bytes| !bytes.is_empty()) {
            return Some(format!("{COVER_BASE}{}", hex(&bytes)));
        }
    }
    attributes
        .picture_size
        .iter()
        .find_map(|size| size.url.as_deref())
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

/// Converts one aligned `items[i]`/`metaItems[i]` pair into a `PlaylistRef`.
/// Non-playlist rows (folders, local rows) and rows without a meta item are
/// skipped.
fn playlist_ref_from_rootlist(
    item: &RootlistItemJson,
    meta: &RootlistMetaItemJson,
) -> Option<PlaylistRef> {
    let raw_uri = item.uri.as_deref()?;
    let parsed = SpotifyUri::from_uri(raw_uri).ok()?;
    let SpotifyUri::Playlist { id, user } = parsed else {
        return None;
    };
    let attributes = meta.attributes.as_ref();
    Some(PlaylistRef {
        id: id.to_base62().unwrap_or_default(),
        uri: raw_uri.to_owned(),
        name: attributes.and_then(|a| a.name.clone()).unwrap_or_default(),
        owner_id: meta
            .owner_username
            .clone()
            .filter(|owner| !owner.is_empty())
            .or(user)
            .unwrap_or_default(),
        // The rootlist carries owner usernames but no display names or
        // descriptions; descriptions arrive from playlist metadata/search.
        description: None,
        owner_name: String::new(),
        cover_url: attributes.and_then(rootlist_cover),
        track_count: meta.length.and_then(|length| u32::try_from(length).ok()),
    })
}

/// The user's playlist library from the spclient rootlist endpoint. The GET
/// is retried with the bounded capped-exponential schedule: the spclient
/// front door answers transient 502/503s that a retry absorbs before the
/// error reaches the UI (librespot's own HTTP retry only covers
/// network-level failures, never 5xx responses). The final failure carries
/// the method/path (the session's own account username, not a credential)
/// and the last error, which embeds the HTTP status.
pub async fn playlists_browse(
    session: &Session,
    length: usize,
) -> Result<Vec<PlaylistRef>, String> {
    let length = length.clamp(1, MAX_PLAYLISTS);
    let endpoint = format!(
        "/playlist/v2/user/{user}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length={length}",
        user = session.username(),
    );
    let backoffs = backoff_sequence(
        BROWSE_RETRY_ATTEMPTS,
        BROWSE_BACKOFF_BASE_MS,
        BROWSE_BACKOFF_MAX_MS,
    );
    let mut attempt = 0usize;
    let body = loop {
        match session
            .spclient()
            .request_as_json(&Method::GET, &endpoint, None, None)
            .await
        {
            Ok(body) => break body,
            Err(error) => {
                if !browse_error_is_transient(error.kind, http_status_of(&error))
                    || attempt >= backoffs.len()
                {
                    return Err(format!(
                        "rootlist request failed: GET {endpoint} after {} attempts (last error: {error})",
                        attempt + 1,
                    ));
                }
                let backoff = backoffs[attempt];
                eprintln!(
                    "rootlist request GET {endpoint} failed ({error}); retrying in {backoff} ms (attempt {}/{})",
                    attempt + 1,
                    backoffs.len() + 1,
                );
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                attempt += 1;
            }
        }
    };
    let parsed: RootlistJson = serde_json::from_slice(&body)
        .map_err(|error| format!("unparseable rootlist response: {error}"))?;
    let contents = parsed.contents.unwrap_or_default();
    Ok(contents
        .items
        .iter()
        .zip(contents.meta_items.iter())
        .filter_map(|(item, meta)| playlist_ref_from_rootlist(item, meta))
        .collect())
}

// ---------------------------------------------------------------------------
// playlist / album / artist metadata
// ---------------------------------------------------------------------------

fn playlist_uri(id: &str) -> Result<SpotifyUri, String> {
    SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))
        .map_err(|error| format!("invalid playlist id: {error}"))
}

/// Fetches one metadata entity with the bounded retry/backoff schedule:
/// librespot's `Metadata::get` is a single Mercury round-trip with no retry
/// of its own, so a transient server-side failure (reported as
/// `Unavailable`) would otherwise fail the whole browse.
async fn metadata_get<T: Metadata>(
    session: &Session,
    uri: &SpotifyUri,
    kind: &str,
) -> Result<T, String> {
    let backoffs = backoff_sequence(
        BROWSE_RETRY_ATTEMPTS,
        BROWSE_BACKOFF_BASE_MS,
        BROWSE_BACKOFF_MAX_MS,
    );
    let mut attempt = 0usize;
    loop {
        match T::get(session, uri).await {
            Ok(item) => return Ok(item),
            Err(error) => {
                if !browse_error_is_transient(error.kind, http_status_of(&error))
                    || attempt >= backoffs.len()
                {
                    return Err(format!(
                        "{kind} fetch of {uri} failed after {} attempts (last error: {error})",
                        attempt + 1,
                    ));
                }
                let backoff = backoffs[attempt];
                eprintln!(
                    "{kind} fetch of {uri} failed ({error}); retrying in {backoff} ms (attempt {}/{})",
                    attempt + 1,
                    backoffs.len() + 1,
                );
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                attempt += 1;
            }
        }
    }
}

/// Playlist header and tracks via the spclient playlist4 endpoint.
pub async fn playlist_browse(
    session: &Session,
    id: &str,
) -> Result<renderer_engine::protocol::PlaylistBrowse, String> {
    let uri = playlist_uri(id)?;
    let playlist: Playlist = metadata_get(session, &uri, "playlist").await?;
    let (owner_id, owner_name) = match &playlist.id {
        SpotifyUri::Playlist { user, .. } => (
            user.clone().unwrap_or_default(),
            user.clone().unwrap_or_default(),
        ),
        _ => (String::new(), String::new()),
    };
    let items: Vec<(SpotifyUri, Option<i64>)> = playlist
        .contents
        .items
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                playlist_added_at(item.attributes.timestamp.as_timestamp_ms()),
            )
        })
        .collect();
    let tracks = fetch_playlist_tracks(session, &items).await?;
    Ok(renderer_engine::protocol::PlaylistBrowse {
        id: id_of(&playlist.id),
        uri: uri_of(&playlist.id),
        name: playlist.name().to_owned(),
        revision: Some(hex(&playlist.revision)),
        owner_id,
        description: playlist_description(&playlist.attributes.description),
        owner_name,
        cover_url: playlist_attributes_cover(&playlist.attributes),
        tracks,
        excluded_track_ids: Vec::new(),
    })
}

/// Playlist artwork: the attributes' raw picture file id (i.scdn.co) wins;
/// ready-made `picture_sizes` URLs are the fallback.
fn playlist_attributes_cover(
    attributes: &librespot_metadata::playlist::attribute::PlaylistAttributes,
) -> Option<String> {
    if !attributes.picture.is_empty() {
        return Some(format!("{COVER_BASE}{}", hex(&attributes.picture)));
    }
    attributes
        .picture_sizes
        .first()
        .map(|size| size.url.clone())
        .filter(|url| !url.is_empty())
}

/// Album header and tracks via the extended-metadata album endpoint.
pub async fn album_browse(
    session: &Session,
    id: &str,
) -> Result<renderer_engine::protocol::AlbumBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}"))
        .map_err(|error| format!("invalid album id: {error}"))?;
    let album: Album = metadata_get(session, &uri, "album").await?;
    let album_track_count = album.tracks().count();
    let mut tracks = fetch_tracks(session, album.tracks()).await?;
    match album_playcounts(session, &uri_of(&album.id), album_track_count).await {
        Ok(counts) => apply_playcounts(&mut tracks, &counts),
        Err(error) => eprintln!("album play counts unavailable: {error}"),
    }
    Ok(renderer_engine::protocol::AlbumBrowse {
        id: id_of(&album.id),
        uri: uri_of(&album.id),
        name: album.name.clone(),
        artist_names: album
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect(),
        artist_ids: album
            .artists
            .iter()
            .map(|artist| id_of(&artist.id))
            .collect(),
        cover_url: cover_url(&album.covers),
        year: album_year(&album),
        tracks,
    })
}

/// Resolves a batch of album URIs into `AlbumRef`s via the extended-metadata
/// endpoint, preserving order and skipping albums that fail to resolve.
async fn fetch_albums<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Result<Vec<AlbumRef>, String> {
    fetch_extended(session, uris, ExtensionKind::ALBUM_V4, parse_album_payload).await
}

/// Artist artwork: the bare `portrait` list first, then the `portrait_group`
/// image group.
///
/// The fallback is not defensive, it is the normal path: ARTIST_V4 leaves
/// `portrait` empty and puts the three sizes (160/320/640) in `portrait_group`
/// instead — verified live against Taylor Swift, The Weeknd and Drake
/// (2026-08-19), all three of which report `portraits=0, portrait_group=3`.
/// Reading only `portraits`, as this did, meant every artist page rendered a
/// monogram tile.
fn artist_portrait(artist: &Artist) -> Option<String> {
    cover_url(&artist.portraits).or_else(|| cover_url(&artist.portrait_group))
}

#[derive(Clone, Debug, Default)]
struct ArtistVisualIdentity {
    square_cover: Option<String>,
    sixteen_by_nine: Option<String>,
    square_full_bleed: Option<String>,
    wide_full_bleed: Option<String>,
}

impl ArtistVisualIdentity {
    fn header(&self) -> Option<&str> {
        self.wide_full_bleed
            .as_deref()
            .or(self.sixteen_by_nine.as_deref())
    }

    fn biography(&self) -> Option<&str> {
        self.sixteen_by_nine
            .as_deref()
            .or(self.square_full_bleed.as_deref())
            .or(self.square_cover.as_deref())
    }
}

struct WireReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WireReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *self.bytes.get(self.position)?;
            self.position += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn field(&mut self) -> Option<(u32, u8)> {
        let tag = self.varint()?;
        Some((u32::try_from(tag >> 3).ok()?, u8::try_from(tag & 7).ok()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = usize::try_from(self.varint()?).ok()?;
        let end = self.position.checked_add(length)?;
        let value = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(value)
    }

    fn skip(&mut self, wire_type: u8) -> bool {
        match wire_type {
            0 => self.varint().is_some(),
            1 => {
                self.position = self.position.saturating_add(8);
                self.position <= self.bytes.len()
            }
            2 => self.bytes().is_some(),
            5 => {
                self.position = self.position.saturating_add(4);
                self.position <= self.bytes.len()
            }
            _ => false,
        }
    }

    fn done(&self) -> bool {
        self.position >= self.bytes.len()
    }
}

fn visual_flat_file_url(payload: &[u8]) -> Option<String> {
    let mut reader = WireReader::new(payload);
    while !reader.done() {
        let (field, wire_type) = reader.field()?;
        if field == 1 && wire_type == 2 {
            return std::str::from_utf8(reader.bytes()?)
                .ok()
                .filter(|url| !url.is_empty())
                .map(str::to_owned);
        }
        if !reader.skip(wire_type) {
            return None;
        }
    }
    None
}

fn visual_instance(payload: &[u8]) -> Option<(u64, String)> {
    let mut reader = WireReader::new(payload);
    let mut url = None;
    let mut size = 0;
    while !reader.done() {
        let (field, wire_type) = reader.field()?;
        match (field, wire_type) {
            (1, 2) => url = visual_flat_file_url(reader.bytes()?),
            (2, 0) => size = reader.varint()?,
            _ if !reader.skip(wire_type) => return None,
            _ => {}
        }
    }
    Some((size, url?))
}

fn visual_group_url(payload: &[u8]) -> Option<String> {
    let mut reader = WireReader::new(payload);
    let mut best = None;
    while !reader.done() {
        let (field, wire_type) = reader.field()?;
        if field == 1 && wire_type == 2 {
            if let Some(candidate) = visual_instance(reader.bytes()?) {
                if best
                    .as_ref()
                    .is_none_or(|current: &(u64, String)| candidate.0 >= current.0)
                {
                    best = Some(candidate);
                }
            }
        } else if !reader.skip(wire_type) {
            return None;
        }
    }
    best.map(|(_, url)| url)
}

fn parse_artist_visual_identity(payload: &[u8]) -> Option<ArtistVisualIdentity> {
    let mut reader = WireReader::new(payload);
    let mut visuals = ArtistVisualIdentity::default();
    while !reader.done() {
        let (field, wire_type) = reader.field()?;
        if wire_type == 2 && matches!(field, 1 | 2 | 5 | 6) {
            let url = visual_group_url(reader.bytes()?);
            match field {
                1 => visuals.square_cover = url,
                2 => visuals.sixteen_by_nine = url,
                5 => visuals.square_full_bleed = url,
                6 => visuals.wide_full_bleed = url,
                _ => unreachable!(),
            }
        } else if !reader.skip(wire_type) {
            return None;
        }
    }
    (visuals.header().is_some() || visuals.biography().is_some()).then_some(visuals)
}

async fn artist_visual_identity(
    session: &Session,
    artist_uri: &SpotifyUri,
) -> Result<Option<ArtistVisualIdentity>, String> {
    // The entity service only returns visual identity when identity is part of
    // the same request. Asking for VISUAL_IDENTITY_TRAIT alone succeeds with
    // an empty response, which previously hid the dedicated wide header.
    let mut request = build_batched_request(
        std::slice::from_ref(artist_uri),
        ExtensionKind::VISUAL_IDENTITY_TRAIT,
        &session.country(),
    );

    request.entity_request[0].query.insert(
        0,
        ExtensionQuery {
            extension_kind: EnumOrUnknown::new(ExtensionKind::IDENTITY_TRAIT),
            ..Default::default()
        },
    );
    let response = session
        .spclient()
        .get_extended_metadata(request)
        .await
        .map_err(|error| format!("artist visual identity request failed: {error}"))?;
    let payload = collect_extension_payloads(&response, ExtensionKind::VISUAL_IDENTITY_TRAIT)
        .into_iter()
        .next()
        .map(|(_, payload)| payload);
    Ok(payload.as_deref().and_then(parse_artist_visual_identity))
}
/// The narrow Canvas request schema from Spotify's official protobuf:
///
/// ```text
/// message EntityCanvazRequest {
///   message Entity { string entityUri = 1; string etag = 2; }
///   repeated Entity entities = 1;
/// }
/// ```
///
/// librespot-protocol 0.8 includes only the nested response record, so the
/// request is encoded here with the same bounded wire reader/writer style as
/// the artist visual-identity parser below.
fn canvas_request(uri: &str) -> Vec<u8> {
    fn varint_len(mut value: usize) -> usize {
        let mut length = 1;
        while value >= 0x80 {
            value >>= 7;
            length += 1;
        }
        length
    }

    fn push_varint(output: &mut Vec<u8>, mut value: usize) {
        while value >= 0x80 {
            output.push((value as u8) | 0x80);
            value >>= 7;
        }
        output.push(value as u8);
    }

    let entity_len = 1 + varint_len(uri.len()) + uri.len();
    let mut entity = Vec::with_capacity(1 + varint_len(uri.len()) + uri.len());
    entity.push(0x0a);
    push_varint(&mut entity, uri.len());
    entity.extend_from_slice(uri.as_bytes());

    let mut request = Vec::with_capacity(1 + varint_len(entity_len) + entity_len);
    request.push(0x0a);
    push_varint(&mut request, entity_len);
    request.extend_from_slice(&entity);
    request
}

fn normalize_canvas_track_uri(id: &str) -> Result<String, String> {
    let id = id.trim();
    let suffix = id
        .strip_prefix("spotify:track:")
        .or_else(|| (!id.is_empty() && !id.contains(':')).then_some(id))
        .filter(|suffix| !suffix.is_empty() && !suffix.contains(':'))
        .ok_or_else(|| "Canvas lookup requires a Spotify track id".to_owned())?;
    Ok(format!("spotify:track:{suffix}"))
}

fn canvas_type_name(kind: canvaz::Type) -> Option<&'static str> {
    match kind {
        canvaz::Type::VIDEO => Some("video"),
        canvaz::Type::VIDEO_LOOPING => Some("video_looping"),
        canvaz::Type::VIDEO_LOOPING_RANDOM => Some("video_looping_random"),
        canvaz::Type::IMAGE | canvaz::Type::GIF => None,
    }
}

/// Parses the stale outer response with the generated 0.8 nested message.
///
/// The generated `EntityCanvazResponse` is empty because its checked-in
/// extraction predates the repeated `canvases = 1` field. Its nested `Canvaz`
/// record is still authoritative for the URL/type field numbers, so only the
/// one missing outer repeated-message layer is hand-decoded.
fn parse_canvas_response(payload: &[u8], track_uri: &str) -> Result<Option<Canvas>, String> {
    let mut reader = WireReader::new(payload);
    while !reader.done() {
        let (field, wire_type) = reader
            .field()
            .ok_or_else(|| "invalid Canvas response field".to_owned())?;
        if field == 1 && wire_type == 2 {
            let record = canvaz::entity_canvaz_response::Canvaz::parse_from_bytes(
                reader
                    .bytes()
                    .ok_or_else(|| "invalid Canvas response record".to_owned())?,
            )
            .map_err(|error| format!("invalid Canvas response record: {error}"))?;
            if !record.entity_uri.is_empty() && record.entity_uri != track_uri {
                continue;
            }
            let Some(canvas_type) =
                canvas_type_name(record.type_.enum_value_or(canvaz::Type::IMAGE))
            else {
                continue;
            };
            let url = record.url.trim();
            if url.starts_with("https://canvaz.scdn.co/")
                && url.ends_with(".mp4")
                && !url.contains(char::is_whitespace)
            {
                return Ok(Some(Canvas {
                    url: url.to_owned(),
                    canvas_type: canvas_type.to_owned(),
                }));
            }
        } else if !reader.skip(wire_type) {
            return Err("invalid Canvas response wire type".to_owned());
        }
    }
    Ok(None)
}

/// Authenticated, on-demand Canvas lookup for one current track.
///
/// Spotify's official client posts the protobuf request to the spclient
/// `/canvaz-cache/v0/canvases` endpoint. Only positive/negative answers are
/// cached; transport/parse failures remain errors so a later panel open may
/// retry them.
pub async fn canvas_browse(session: &Session, id: &str) -> Result<Option<Canvas>, String> {
    let track_uri = normalize_canvas_track_uri(id)?;
    if let Some(value) = cached_canvas(&track_uri) {
        return Ok(value);
    }

    let request_body = canvas_request(&track_uri);
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-protobuf"),
    );
    let payload = session
        .spclient()
        .request(
            &Method::POST,
            "/canvaz-cache/v0/canvases",
            Some(headers),
            Some(&request_body),
        )
        .await
        .map_err(|error| format!("Canvas request failed: {error}"))?;
    let result = parse_canvas_response(&payload, &track_uri)?;
    remember_canvas(&track_uri, result.clone());
    Ok(result)
}

const INITIAL_ARTIST_RELEASES: usize = 12;
const MAX_ARTIST_RELEASE_PAGE: usize = 40;

fn artist_release_groups(artist: &Artist) -> [Vec<SpotifyUri>; 4] {
    [
        artist.albums_current().cloned().collect(),
        artist.singles_current().cloned().collect(),
        artist.compilations_current().cloned().collect(),
        artist.appears_on_albums_current().cloned().collect(),
    ]
}

fn selected_release_group_indices(release_types: &[String]) -> Result<Vec<usize>, String> {
    if release_types.is_empty() {
        return Ok(vec![0, 1, 2, 3]);
    }
    let mut selected = Vec::new();
    for kind in release_types {
        let index = match kind.as_str() {
            "albums" => 0,
            "singles" => 1,
            "compilations" => 2,
            "appears_on" => 3,
            _ => return Err(format!("unsupported artist release type: {kind}")),
        };
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    Ok(selected)
}

async fn resolve_artist_release_entries(
    session: &Session,
    page: Vec<(usize, &SpotifyUri)>,
    total: usize,
    offset: usize,
    limit: usize,
) -> Result<renderer_engine::protocol::ArtistReleasePage, String> {
    let resolved = fetch_albums(session, page.iter().map(|(_, uri)| *uri)).await?;
    let by_id: HashMap<&str, &AlbumRef> = resolved
        .iter()
        .map(|album| (album.id.as_str(), album))
        .collect();
    let mut releases = renderer_engine::protocol::ArtistReleases::default();
    for (group, uri) in page {
        let Some(album) = by_id.get(id_of(uri).as_str()).map(|album| (*album).clone()) else {
            continue;
        };
        match group {
            0 => releases.albums.push(album),
            1 => releases.singles.push(album),
            2 => releases.compilations.push(album),
            _ => releases.appears_on.push(album),
        }
    }
    let end = offset.saturating_add(limit).min(total);
    Ok(renderer_engine::protocol::ArtistReleasePage {
        releases,
        total,
        next_offset: (end < total).then_some(end),
    })
}

/// Picks a bounded, round-robin page from the four catalogue groups. Empty
/// groups are skipped on every pass, so their unused share is immediately
/// available to the remaining groups.
fn balanced_artist_release_page<'a>(
    groups: &'a [Vec<SpotifyUri>; 4],
    limit: usize,
) -> Vec<(usize, &'a SpotifyUri)> {
    let mut offsets = [0usize; 4];
    let mut page = Vec::with_capacity(limit);
    while page.len() < limit {
        let mut added = false;
        for (group, releases) in groups.iter().enumerate() {
            let Some(uri) = releases.get(offsets[group]) else {
                continue;
            };
            offsets[group] += 1;
            page.push((group, uri));
            added = true;
            if page.len() == limit {
                break;
            }
        }
        if !added {
            break;
        }
    }
    page
}

async fn resolve_initial_artist_release_page(
    session: &Session,
    groups: &[Vec<SpotifyUri>; 4],
) -> Result<renderer_engine::protocol::ArtistReleasePage, String> {
    let total = groups.iter().map(|group| group.len()).sum::<usize>();
    let limit = INITIAL_ARTIST_RELEASES.min(MAX_ARTIST_RELEASE_PAGE);
    let page = balanced_artist_release_page(groups, limit);
    resolve_artist_release_entries(session, page, total, 0, limit).await
}

fn metadata_artist_overview(artist: &Artist) -> ArtistOverview {
    let biography = artist
        .biographies
        .iter()
        .find(|biography| !biography.text.trim().is_empty());
    let biography_images = biography.into_iter().flat_map(|biography| {
        biography.portraits.iter().chain(
            biography
                .portrait_group
                .iter()
                .flat_map(|group| group.iter()),
        )
    });
    ArtistOverview {
        biography: biography.map(|biography| biography.text.trim().to_owned()),
        header_image_url: largest_cover_url(
            artist.portraits.iter().chain(artist.portrait_group.iter()),
        ),
        biography_image_url: largest_cover_url(biography_images),
        popularity: u32::try_from(artist.popularity)
            .ok()
            .filter(|popularity| *popularity > 0),
        related_artists: artist
            .related
            .iter()
            .map(|related| ArtistRef {
                id: id_of(&related.id),
                uri: uri_of(&related.id),
                name: related.name.clone(),
                portrait_url: artist_portrait(related),
            })
            .filter(|related| !related.id.is_empty() && !related.name.is_empty())
            .collect(),
        ..ArtistOverview::default()
    }
}

fn merge_artist_overview(
    artist: &Artist,
    query: Option<&ArtistOverviewQuery>,
    visuals: Option<&ArtistVisualIdentity>,
) -> Option<ArtistOverview> {
    let mut overview = metadata_artist_overview(artist);
    if let Some(query) = query {
        let supplied = &query.overview;
        if supplied.biography.is_some() {
            overview.biography.clone_from(&supplied.biography);
        }
        if supplied.header_image_url.is_some() {
            overview
                .header_image_url
                .clone_from(&supplied.header_image_url);
        }
        if supplied.biography_image_url.is_some() {
            overview
                .biography_image_url
                .clone_from(&supplied.biography_image_url);
        }
        overview.followers = supplied.followers;
        overview.monthly_listeners = supplied.monthly_listeners;
        overview.world_rank = supplied.world_rank;
        overview.top_cities.clone_from(&supplied.top_cities);
        overview
            .popular_releases
            .clone_from(&supplied.popular_releases);
        if !supplied.related_artists.is_empty() {
            overview
                .related_artists
                .clone_from(&supplied.related_artists);
        }
        overview.discovered_on.clone_from(&supplied.discovered_on);
        overview
            .artist_playlists
            .clone_from(&supplied.artist_playlists);
        overview.artist_pick.clone_from(&supplied.artist_pick);
    }
    if let Some(visuals) = visuals {
        if let Some(header) = visuals.header() {
            overview.header_image_url = Some(header.to_owned());
        }
        if overview.biography_image_url.is_none() {
            overview.biography_image_url = visuals.biography().map(str::to_owned);
        }
    }

    (overview.biography.is_some()
        || overview.header_image_url.is_some()
        || overview.biography_image_url.is_some()
        || overview.popularity.is_some()
        || overview.followers.is_some()
        || overview.monthly_listeners.is_some()
        || overview.world_rank.is_some()
        || !overview.top_cities.is_empty()
        || !overview.popular_releases.is_empty()
        || !overview.related_artists.is_empty()
        || !overview.discovered_on.is_empty()
        || !overview.artist_playlists.is_empty()
        || overview.artist_pick.is_some())
    .then_some(overview)
}

/// Normalizes a playlist title for the official-title comparison. Spotify's
/// search response has used several kinds of Unicode whitespace in the same
/// title, so ASCII-only trimming or lowercasing would admit false negatives
/// (or force a looser, unsafe comparison).
fn normalize_playlist_title(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn songwriter_playlist_title(canonical_artist_name: &str) -> Option<String> {
    let artist_name = canonical_artist_name.trim();
    (!artist_name.is_empty()).then(|| format!("{SONGWRITER_QUERY_PREFIX}{artist_name}"))
}

/// Applies the search-side prefilter. Search owner data is never accepted as
/// proof by itself: it only narrows candidates before the playlist metadata
/// request below re-verifies the owner and title.
fn official_songwriter_candidate(
    playlists: &[PlaylistRef],
    canonical_artist_name: &str,
) -> Result<Option<PlaylistRef>, String> {
    let Some(expected_title) = songwriter_playlist_title(canonical_artist_name) else {
        return Ok(None);
    };
    let expected_title = normalize_playlist_title(&expected_title);
    let mut seen_ids = HashSet::new();
    let mut candidates = Vec::new();
    for reference in playlists {
        if reference.owner_id != "spotify"
            || normalize_playlist_title(&reference.name) != expected_title
        {
            continue;
        }
        let Ok(uri) = SpotifyUri::from_uri(&reference.uri) else {
            continue;
        };
        if !matches!(uri, SpotifyUri::Playlist { .. }) {
            continue;
        }
        let id = id_of(&uri);
        if id.is_empty() || !seen_ids.insert(id.clone()) {
            continue;
        }
        let mut canonical = reference.clone();
        canonical.id = id;
        canonical.uri = uri_of(&uri);
        candidates.push(canonical);
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        count => Err(format!(
            "found {count} exact official songwriter playlists; rejecting ambiguous result"
        )),
    }
}

fn official_playlist_owner(uri: &SpotifyUri) -> Option<&str> {
    match uri {
        SpotifyUri::Playlist {
            user: Some(user), ..
        } if user == "spotify" => Some(user),
        _ => None,
    }
}

/// Rebuilds a playlist reference from the authoritative metadata response.
/// The search reference contributes only display metadata unavailable from
/// playlist4 (currently the owner's display name); owner id, URI, title,
/// description, cover, and count all come from the metadata payload.
fn official_songwriter_playlist_ref(
    playlist: &Playlist,
    search_reference: &PlaylistRef,
) -> Option<PlaylistRef> {
    let owner_id = official_playlist_owner(&playlist.id)?.to_owned();
    let id = id_of(&playlist.id);
    let uri = uri_of(&playlist.id);
    if id.is_empty() || uri.is_empty() {
        return None;
    }
    Some(PlaylistRef {
        id,
        uri,
        name: playlist.name().to_owned(),
        description: playlist_description(&playlist.attributes.description),
        owner_id: owner_id.clone(),
        owner_name: if search_reference.owner_name.is_empty() {
            owner_id.clone()
        } else {
            search_reference.owner_name.clone()
        },
        cover_url: playlist_attributes_cover(&playlist.attributes),
        track_count: u32::try_from(playlist.length).ok(),
    })
}

async fn discover_songwriter_playlist(
    session: &Session,
    canonical_artist_name: &str,
) -> Result<Option<SongwriterPlaylist>, String> {
    let Some(query) = songwriter_playlist_title(canonical_artist_name) else {
        return Ok(None);
    };
    let search = search_browse(session, &query, MAX_SEARCH_LIMIT).await?;
    let Some(candidate) = official_songwriter_candidate(&search.playlists, canonical_artist_name)?
    else {
        return Ok(None);
    };
    let candidate_uri = SpotifyUri::from_uri(&candidate.uri)
        .map_err(|error| format!("invalid songwriter playlist URI: {error}"))?;
    let playlist: Playlist = metadata_get(session, &candidate_uri, "songwriter playlist").await?;
    let Some(reference) = official_songwriter_playlist_ref(&playlist, &candidate) else {
        return Ok(None);
    };
    if normalize_playlist_title(&reference.name) != normalize_playlist_title(&query) {
        return Ok(None);
    }
    let items: Vec<(SpotifyUri, Option<i64>)> = playlist
        .contents
        .items
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                playlist_added_at(item.attributes.timestamp.as_timestamp_ms()),
            )
        })
        .collect();
    let mut tracks = fetch_playlist_tracks_limited(session, &items, SONGWRITER_TRACK_LIMIT).await?;
    tracks.truncate(SONGWRITER_TRACK_LIMIT);
    if tracks.is_empty() {
        return Ok(None);
    }
    Ok(Some(SongwriterPlaylist {
        playlist: reference,
        tracks,
    }))
}

async fn cached_songwriter_playlist(
    session: &Session,
    artist_id: &str,
    canonical_artist_name: &str,
) -> Option<SongwriterPlaylist> {
    if let Ok(mut cache) = SONGWRITER_PLAYLIST_CACHE.lock() {
        if let Some(value) = cache.get(artist_id, Instant::now()) {
            return value;
        }
    }
    let value = match discover_songwriter_playlist(session, canonical_artist_name).await {
        Ok(value) => value,
        Err(error) => {
            eprintln!("songwriter playlist unavailable for artist {artist_id}: {error}");
            None
        }
    };
    if let Ok(mut cache) = SONGWRITER_PLAYLIST_CACHE.lock() {
        cache.insert(artist_id.to_owned(), value.clone(), Instant::now());
    }
    value
}
/// Resolves the optional verified `Written by <artist>` playlist independently
/// from the artist overview. The canonical identity comes from `artist_browse`,
/// so this request does not repeat artist metadata work.
pub async fn artist_songwriter_browse(
    session: &Session,
    artist_id: &str,
    canonical_artist_name: &str,
) -> Result<Option<SongwriterPlaylist>, String> {
    if artist_id.trim().is_empty() {
        return Ok(None);
    }
    Ok(cached_songwriter_playlist(session, artist_id, canonical_artist_name).await)
}

/// Artist portrait, top tracks, and the grouped release catalogue via the
/// extended-metadata artist endpoint.
///
/// The four groups are resolved in one combined set of batches rather than
/// four separate ones: batching is per 40 URIs, so four independent fetches
/// would round up to a partial request each (and re-resolve any album that
/// appears in two groups), while one combined fetch deduplicates by id and
/// only ever pays for the last partial batch once.
pub async fn artist_browse(
    session: &Session,
    id: &str,
) -> Result<renderer_engine::protocol::ArtistBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
        .map_err(|error| format!("invalid artist id: {error}"))?;
    let artist: Artist = metadata_get(session, &uri, "artist").await?;
    let artist_uri = uri_of(&artist.id);
    let top_track_uris = artist.top_tracks.for_country(&session.country());
    let groups = artist_release_groups(&artist);
    let (query_overview, visual_identity, top_tracks, page) = tokio::join!(
        cached_artist_overview(session, id, &artist_uri),
        artist_visual_identity(session, &uri),
        fetch_tracks(session, top_track_uris.iter()),
        resolve_initial_artist_release_page(session, &groups),
    );
    let visual_identity = visual_identity
        .inspect_err(|error| eprintln!("{error}"))
        .ok()
        .flatten();
    let mut top_tracks = top_tracks?;
    if let Some(query) = query_overview.as_ref() {
        // queryArtistOverview carries the same URI/playcount map as getTrack.
        // Even a sparse successful overview must not trigger the old extra
        // request: missing counts stay missing.
        apply_playcounts(&mut top_tracks, &query.top_playcounts);
    } else if let Some(seed) = top_tracks
        .iter()
        .find(|track| track.artist_id == id)
        .or_else(|| top_tracks.first())
    {
        match artist_top_playcounts(session, &seed.uri).await {
            Ok(counts) => apply_playcounts(&mut top_tracks, &counts),
            Err(error) => eprintln!("artist play counts unavailable: {error}"),
        }
    }

    let overview =
        merge_artist_overview(&artist, query_overview.as_ref(), visual_identity.as_ref());
    let page = page?;
    Ok(renderer_engine::protocol::ArtistBrowse {
        id: id_of(&artist.id),
        uri: artist_uri,
        name: artist.name.clone(),
        portrait_url: artist_portrait(&artist),
        top_tracks,
        releases: page.releases,
        release_counts: renderer_engine::protocol::ArtistReleaseCounts {
            albums: groups[0].len(),
            singles: groups[1].len(),
            compilations: groups[2].len(),
            appears_on: groups[3].len(),
        },
        releases_next_offset: page.next_offset,
        overview,
    })
}

#[derive(Clone)]
struct CatalogueReleaseSeed {
    header: renderer_engine::protocol::AlbumBrowse,
    track_uris: Vec<SpotifyUri>,
}

fn parse_catalogue_release_payload(
    entity_uri: &str,
    payload: &[u8],
) -> Option<CatalogueReleaseSeed> {
    let message = librespot_protocol::metadata::Album::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let album = Album::parse(&message, &uri).ok()?;
    Some(CatalogueReleaseSeed {
        header: renderer_engine::protocol::AlbumBrowse {
            id: id_of(&album.id),
            uri: uri_of(&album.id),
            name: album.name.clone(),
            artist_names: album
                .artists
                .iter()
                .map(|artist| artist.name.clone())
                .collect(),
            artist_ids: album
                .artists
                .iter()
                .map(|artist| id_of(&artist.id))
                .collect(),
            cover_url: cover_url(&album.covers),
            year: album_year(&album),
            tracks: Vec::new(),
        },
        track_uris: album.tracks().cloned().collect(),
    })
}

const CATALOGUE_MANIFEST_TTL: Duration = Duration::from_secs(30 * 60);
const CATALOGUE_MANIFEST_CACHE_CAPACITY: usize = 8;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CatalogueManifestKey {
    artist_id: String,
    country: String,
    selected_groups: Vec<usize>,
}
#[derive(Clone)]
struct CatalogueManifestEntry {
    fetched_at: Instant,
    releases: Arc<[SpotifyUri]>,
}

#[derive(Default)]
struct CatalogueManifestCache {
    entries: HashMap<CatalogueManifestKey, CatalogueManifestEntry>,
}
impl CatalogueManifestCache {
    fn get(&mut self, key: &CatalogueManifestKey, now: Instant) -> Option<Arc<[SpotifyUri]>> {
        let entry = self.entries.get(key)?;
        if now.duration_since(entry.fetched_at) >= CATALOGUE_MANIFEST_TTL {
            self.entries.remove(key);
            return None;
        }
        Some(Arc::clone(&entry.releases))
    }
    fn insert(&mut self, key: CatalogueManifestKey, releases: Arc<[SpotifyUri]>, now: Instant) {
        self.entries
            .retain(|_, entry| now.duration_since(entry.fetched_at) < CATALOGUE_MANIFEST_TTL);
        let key_is_absent = !self.entries.contains_key(&key);
        evict_oldest_for_fresh_key(
            &mut self.entries,
            CATALOGUE_MANIFEST_CACHE_CAPACITY,
            key_is_absent,
            |entry| entry.fetched_at,
        );
        self.entries.insert(
            key,
            CatalogueManifestEntry {
                fetched_at: now,
                releases,
            },
        );
    }
}

static CATALOGUE_MANIFEST_CACHE: LazyLock<Mutex<CatalogueManifestCache>> =
    LazyLock::new(|| Mutex::new(CatalogueManifestCache::default()));

/// Deduplicates the selected release groups in a deterministic order before
/// their metadata is resolved. Multi-group manifests are sorted globally by
/// year afterwards; this round-robin order is only the stable tie-breaker.
/// Single-group manifests retain the artist endpoint's source order.
fn interleaved_catalogue_uris(
    groups: &[Vec<SpotifyUri>; 4],
    selected: &[usize],
) -> Vec<SpotifyUri> {
    if selected.len() <= 1 {
        return selected
            .first()
            .map(|&index| {
                let mut seen = HashSet::new();
                groups[index]
                    .iter()
                    .filter(|uri| seen.insert(uri_of(uri)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
    }

    let mut positions = vec![0usize; selected.len()];
    let mut seen = HashSet::new();
    let mut releases = Vec::new();
    loop {
        let mut added = false;
        for (slot, &group) in selected.iter().enumerate() {
            while let Some(uri) = groups[group].get(positions[slot]) {
                positions[slot] += 1;
                if seen.insert(uri_of(uri)) {
                    releases.push(uri.clone());
                    added = true;
                    break;
                }
            }
        }
        if !added {
            break;
        }
    }
    releases
}

fn order_catalogue_manifest(releases: &mut [CatalogueReleaseSeed], selected_group_count: usize) {
    if selected_group_count <= 1 {
        return;
    }
    // The artist metadata keeps each release type in a separate sequence.
    // Round-robin pagination therefore places an old album ahead of a recent
    // single that has not been loaded yet. Sort the complete mixed manifest
    // before choosing any page boundary. Stable sort preserves source order
    // for same-year and undated releases.
    releases.sort_by(|left, right| {
        right
            .header
            .year
            .unwrap_or(0)
            .cmp(&left.header.year.unwrap_or(0))
    });
}

const ARTIST_DISCOGRAPHY_HASH: &str =
    "5e07d323febb57b4a56a42abbf781490e58764aa45feb6e3dc0591564fc56599";

#[derive(Default, Deserialize)]
struct ArtistDiscographyResponseJson {
    #[serde(default)]
    data: Option<ArtistDiscographyDataJson>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistDiscographyDataJson {
    #[serde(default)]
    artistUnion: Option<ArtistDiscographyUnionJson>,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyUnionJson {
    #[serde(default)]
    discography: Option<ArtistDiscographyJson>,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyJson {
    #[serde(default)]
    albums: Option<ArtistDiscographySectionJson>,
    #[serde(default)]
    singles: Option<ArtistDiscographySectionJson>,
    #[serde(default)]
    compilations: Option<ArtistDiscographySectionJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistDiscographySectionJson {
    #[serde(default)]
    items: Vec<ArtistDiscographyReleaseGroupJson>,
    #[serde(default)]
    totalCount: usize,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyReleaseGroupJson {
    #[serde(default)]
    releases: ArtistDiscographyReleasesJson,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyReleasesJson {
    #[serde(default)]
    items: Vec<ArtistDiscographyReleaseJson>,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyReleaseJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    date: Option<ArtistDiscographyDateJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistDiscographyDateJson {
    #[serde(default)]
    isoString: Option<String>,
    #[serde(default)]
    year: Option<u32>,
}

#[derive(Clone)]
struct OrderedReleaseUri {
    uri: SpotifyUri,
    date: String,
}

fn artist_discography_body(
    artist_uri: &str,
    group: usize,
    offset: usize,
    limit: usize,
) -> Result<String, String> {
    let operation = match group {
        0 => "queryArtistDiscographyAlbums",
        1 => "queryArtistDiscographySingles",
        2 => "queryArtistDiscographyCompilations",
        _ => return Err("the official discography query has no Appears On group".to_owned()),
    };
    Ok(serde_json::json!({
        "operationName": operation,
        "variables": {
            "uri": artist_uri,
            "offset": offset,
            "limit": limit.clamp(1, 300),
            "order": "DATE_DESC",
        },
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": ARTIST_DISCOGRAPHY_HASH,
            },
        },
    })
    .to_string())
}

fn parse_artist_discography_page(
    payload: &[u8],
    group: usize,
) -> Result<(Vec<OrderedReleaseUri>, usize, usize), String> {
    let ArtistDiscographyResponseJson { data, errors } = serde_json::from_slice(payload)
        .map_err(|error| format!("unparseable artist discography response: {error}"))?;
    let discography = data
        .and_then(|data| data.artistUnion)
        .and_then(|artist| artist.discography)
        .ok_or_else(|| {
            if errors.as_deref().unwrap_or_default().is_empty() {
                "artist discography response contained no catalogue".to_owned()
            } else {
                "artist discography document was rejected".to_owned()
            }
        })?;
    let section = match group {
        0 => discography.albums,
        1 => discography.singles,
        2 => discography.compilations,
        _ => None,
    }
    .ok_or_else(|| "artist discography response omitted the requested group".to_owned())?;
    let consumed = section.items.len();
    let releases = section
        .items
        .into_iter()
        .filter_map(|group| {
            // Each row is one release family. Spotify orders variants by
            // market preference; the first valid URI is the one its client
            // presents for that row.
            let release = group
                .releases
                .items
                .into_iter()
                .find(|release| release.uri.as_deref().is_some_and(|uri| !uri.is_empty()))?;
            let uri = SpotifyUri::from_uri(release.uri.as_deref()?).ok()?;
            let date = release
                .date
                .and_then(|date| {
                    date.isoString
                        .filter(|date| !date.is_empty())
                        .or_else(|| date.year.map(|year| format!("{year:04}")))
                })
                .unwrap_or_default();
            Some(OrderedReleaseUri { uri, date })
        })
        .collect();
    Ok((releases, consumed, section.totalCount))
}

async fn official_discography_group(
    session: &Session,
    artist_uri: &str,
    group: usize,
    expected: usize,
) -> Result<Vec<OrderedReleaseUri>, String> {
    if expected == 0 {
        return Ok(Vec::new());
    }
    let mut offset = 0;
    let mut total = expected;
    let mut releases = Vec::with_capacity(expected);
    loop {
        let limit = total.saturating_sub(offset).clamp(1, 300);
        let body = artist_discography_body(artist_uri, group, offset, limit)?;
        let payload = pathfinder_post(session, &body, "artist discography").await?;
        let (mut page, consumed, reported_total) = parse_artist_discography_page(&payload, group)?;
        total = reported_total;
        releases.append(&mut page);
        if consumed == 0 || offset.saturating_add(consumed) >= total {
            break;
        }
        offset = offset.saturating_add(consumed);
    }
    if expected > 0 && releases.is_empty() {
        return Err("artist discography returned no releases".to_owned());
    }
    Ok(releases)
}

fn merge_official_discography(
    groups: impl IntoIterator<Item = Vec<OrderedReleaseUri>>,
) -> Vec<SpotifyUri> {
    let mut seen = HashSet::new();
    let mut releases = groups
        .into_iter()
        .flatten()
        .filter(|release| seen.insert(uri_of(&release.uri)))
        .collect::<Vec<_>>();
    // Stable sort: exact server dates decide the global mixed order; equal-date
    // rows retain each section's server order and the selected section order.
    releases.sort_by(|left, right| right.date.cmp(&left.date));
    releases.into_iter().map(|release| release.uri).collect()
}

async fn official_catalogue_manifest(
    session: &Session,
    artist_uri: &str,
    groups: &[Vec<SpotifyUri>; 4],
    selected: &[usize],
) -> Result<Vec<SpotifyUri>, String> {
    match selected {
        [group @ 0..=2] => Ok(merge_official_discography([official_discography_group(
            session,
            artist_uri,
            *group,
            groups[*group].len(),
        )
        .await?])),
        [0, 1, 2] => {
            let (albums, singles, compilations) = tokio::join!(
                official_discography_group(session, artist_uri, 0, groups[0].len()),
                official_discography_group(session, artist_uri, 1, groups[1].len()),
                official_discography_group(session, artist_uri, 2, groups[2].len()),
            );
            Ok(merge_official_discography([
                albums?,
                singles?,
                compilations?,
            ]))
        }
        _ => Err("unsupported official discography group combination".to_owned()),
    }
}

fn cached_catalogue_manifest(
    artist_id: &str,
    country: &str,
    selected: &[usize],
) -> Option<Arc<[SpotifyUri]>> {
    let key = CatalogueManifestKey {
        artist_id: artist_id.to_owned(),
        country: country.to_owned(),
        selected_groups: selected.to_vec(),
    };
    CATALOGUE_MANIFEST_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key, Instant::now())
}

async fn catalogue_manifest(
    session: &Session,
    artist_id: &str,
    groups: &[Vec<SpotifyUri>; 4],
    selected: &[usize],
) -> Result<Arc<[SpotifyUri]>, String> {
    let key = CatalogueManifestKey {
        artist_id: artist_id.to_owned(),
        country: session.country(),
        selected_groups: selected.to_vec(),
    };
    if let Some(cached) = CATALOGUE_MANIFEST_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key, Instant::now())
    {
        return Ok(cached);
    }

    let artist_uri = format!("spotify:artist:{artist_id}");
    let releases = match official_catalogue_manifest(session, &artist_uri, groups, selected).await {
        Ok(releases) if !releases.is_empty() => releases,
        Ok(_) | Err(_) => {
            // The persisted document can rotate. Retain the metadata4 path as
            // a correct, slower fallback rather than making the catalogue
            // unavailable when that optional server ordering disappears.
            let uris = interleaved_catalogue_uris(groups, selected);
            let mut seeds = fetch_extended(
                session,
                uris.iter(),
                ExtensionKind::ALBUM_V4,
                parse_catalogue_release_payload,
            )
            .await?;
            order_catalogue_manifest(&mut seeds, selected.len());
            seeds
                .into_iter()
                .filter_map(|release| SpotifyUri::from_uri(&release.header.uri).ok())
                .collect()
        }
    };

    let releases: Arc<[SpotifyUri]> = releases.into();
    CATALOGUE_MANIFEST_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, Arc::clone(&releases), Instant::now());
    Ok(releases)
}

pub async fn artist_catalogue_browse(
    session: &Session,
    id: &str,
    release_types: &[String],
    offset: usize,
    limit: usize,
) -> Result<renderer_engine::protocol::ArtistCataloguePage, String> {
    let selected = if release_types.is_empty() {
        vec![0, 1, 2]
    } else {
        selected_release_group_indices(release_types)?
    };
    let country = session.country();
    let manifest = if let Some(cached) = cached_catalogue_manifest(id, &country, &selected) {
        cached
    } else {
        let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
            .map_err(|error| format!("invalid artist id: {error}"))?;
        let artist: Artist = metadata_get(session, &uri, "artist").await?;
        let groups = artist_release_groups(&artist);
        catalogue_manifest(session, id, &groups, &selected).await?
    };
    let total = manifest.len();
    let limit = limit.clamp(1, 6);
    let end = offset.saturating_add(limit).min(total);
    let page = manifest.get(offset..end).unwrap_or_default();
    let mut seeds = fetch_extended(
        session,
        page.iter(),
        ExtensionKind::ALBUM_V4,
        parse_catalogue_release_payload,
    )
    .await?;
    let wanted_tracks: Vec<SpotifyUri> = seeds
        .iter()
        .flat_map(|release| release.track_uris.iter().cloned())
        .collect();
    let resolved_tracks = fetch_tracks(session, wanted_tracks.iter()).await?;
    let by_uri: HashMap<String, TrackRef> = resolved_tracks
        .into_iter()
        .map(|track| (track.uri.clone(), track))
        .collect();
    let mut complete = Vec::with_capacity(seeds.len());
    for mut release in seeds.drain(..) {
        release.header.tracks = release
            .track_uris
            .iter()
            .filter_map(|uri| by_uri.get(&uri_of(uri)).cloned())
            .collect();
        match album_playcounts(session, &release.header.uri, release.track_uris.len()).await {
            Ok(counts) => apply_playcounts(&mut release.header.tracks, &counts),
            Err(error) => eprintln!("album play counts unavailable: {error}"),
        }
        complete.push(release.header);
    }
    Ok(renderer_engine::protocol::ArtistCataloguePage {
        releases: complete,
        total,
        next_offset: (end < total).then_some(end),
    })
}

fn context_page_track_uris(
    page: &librespot_protocol::context_page::ContextPage,
) -> Vec<SpotifyUri> {
    page.tracks
        .iter()
        .filter_map(|track| SpotifyUri::from_uri(track.uri()).ok())
        .collect()
}

fn context_page_next_cursor(
    page: &librespot_protocol::context_page::ContextPage,
) -> Option<String> {
    page.next_page_url
        .clone()
        .filter(|url| !url.is_empty())
        .or_else(|| {
            (page.tracks.is_empty())
                .then(|| page.page_url.clone())
                .flatten()
                .filter(|url| !url.is_empty())
        })
}

/// Read-only Saved Tracks browse through librespot's authenticated context
/// resolver. Pagination remains explicit: opening Liked Songs fetches one
/// page; following `next_cursor` is the only thing that fetches another.
pub async fn liked_songs_browse(
    session: &Session,
    cursor: Option<&str>,
) -> Result<renderer_engine::protocol::LikedSongsPage, String> {
    let (uris, next_cursor) = if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
        let payload = session
            .spclient()
            .get_next_page(cursor)
            .await
            .map_err(|error| format!("liked songs page request failed: {error}"))?;
        let json = std::str::from_utf8(&payload)
            .map_err(|error| format!("liked songs page was not utf-8: {error}"))?;
        let page = protobuf_json_mapping::parse_from_str::<
            librespot_protocol::context_page::ContextPage,
        >(json)
        .map_err(|error| format!("unparseable liked songs page: {error}"))?;
        (
            context_page_track_uris(&page),
            context_page_next_cursor(&page),
        )
    } else {
        let context_uri = format!("spotify:user:{}:collection", session.username());
        let context = session
            .spclient()
            .get_context(&context_uri)
            .await
            .map_err(|error| format!("liked songs request failed: {error}"))?;
        let uris = context
            .pages
            .iter()
            .flat_map(context_page_track_uris)
            .collect();
        let next_cursor = context.pages.iter().find_map(context_page_next_cursor);
        (uris, next_cursor)
    };
    let tracks = fetch_tracks(session, uris.iter()).await?;
    Ok(renderer_engine::protocol::LikedSongsPage {
        tracks,
        next_cursor,
    })
}

/// The Saved Tracks collection as bare URIs — the same spclient context
/// resolver [`liked_songs_browse`] walks, minus the per-track metadata
/// batches. Membership checks need identity, not playable rows, so a whole
/// collection costs one round trip per page instead of one metadata batch
/// per hundred tracks. Pagination mirrors the parent: following
/// `next_cursor` is the only thing that fetches another page.
pub async fn liked_song_uris_browse(
    session: &Session,
    cursor: Option<&str>,
) -> Result<renderer_engine::protocol::LikedUrisPage, String> {
    let (uris, next_cursor) = if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
        let payload = session
            .spclient()
            .get_next_page(cursor)
            .await
            .map_err(|error| format!("liked song uris page request failed: {error}"))?;
        let json = std::str::from_utf8(&payload)
            .map_err(|error| format!("liked song uris page was not utf-8: {error}"))?;
        let page = protobuf_json_mapping::parse_from_str::<
            librespot_protocol::context_page::ContextPage,
        >(json)
        .map_err(|error| format!("unparseable liked song uris page: {error}"))?;
        (
            context_page_track_uris(&page),
            context_page_next_cursor(&page),
        )
    } else {
        let context_uri = format!("spotify:user:{}:collection", session.username());
        let context = session
            .spclient()
            .get_context(&context_uri)
            .await
            .map_err(|error| format!("liked song uris request failed: {error}"))?;
        let uris = context
            .pages
            .iter()
            .flat_map(context_page_track_uris)
            .collect();
        let next_cursor = context.pages.iter().find_map(context_page_next_cursor);
        (uris, next_cursor)
    };
    Ok(renderer_engine::protocol::LikedUrisPage {
        uris: uris.iter().map(uri_of).collect(),
        next_cursor,
    })
}

// ---------------------------------------------------------------------------
// track credits (queryTrackCreditsGroupedModal)
// ---------------------------------------------------------------------------

/// Spotify ids are opaque 22-character base62 values. Restricting the id to
/// that alphabet and length keeps a malformed source URI from becoming URL
/// path data. The original URI is still retained for diagnostics.
fn credit_artist_id(uri: &str) -> String {
    let Some(id) = uri.strip_prefix("spotify:artist:") else {
        return String::new();
    };
    if id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        id.to_owned()
    } else {
        String::new()
    }
}

/// Songwriter/producer/performer credits for one track.
///
/// One GET of roughly a kilobyte, authenticated with the session's own
/// login5 bearer and client token like every other browse here. Called only
/// when the user actually asks for credits: nothing else in the app needs
/// them, and this is the one endpoint whose cost is per track rather than
/// per page.
pub async fn track_credits_browse(session: &Session, id: &str) -> Result<TrackCredits, String> {
    // Validated rather than interpolated blindly: the id lands in a URL path.
    let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}"))
        .map_err(|error| format!("invalid track id: {error}"))?;
    let track_id = id_of(&uri);
    if track_id.is_empty() {
        return Err("invalid track id".to_owned());
    }
    let track_uri = format!("spotify:track:{track_id}");

    // The GraphQL operation the official client uses. There is deliberately no
    // fallback to the older `track-credits-view` REST endpoint: it returns
    // three fixed role groups instead of the full contributor list, and no
    // external links at all, so falling back to it would quietly restore both
    // of the defects this replaced. A rotated persisted-query hash should
    // surface as "credits unavailable" and get the hash updated, not hide
    // behind a thinner answer that looks like it worked.
    let payload = pathfinder_post(session, &track_credits_body(&track_uri), "credits").await?;
    let parsed: CreditsQueryJson = serde_json::from_slice(&payload)
        .map_err(|error| format!("unparseable credits response: {error}"))?;
    let credits = track_credits_from_query(&parsed, &track_uri);
    if credits.roles.is_empty() {
        return Err("Spotify returned no credits for this track".to_owned());
    }
    Ok(credits)
}

/// Persisted-query hash for `queryTrackCreditsGroupedModal`, the operation
/// behind the official desktop client's Credits dialog (read out of its own
/// bundle, `xpui-root-dialogs.js`, client 1.2.96.518).
const TRACK_CREDITS_QUERY_HASH: &str =
    "f135fb9be58a72d041ab5d214d817021a272405d883860468e2627afb01a3ca9";

/// The official client's contributor limit. It pages with an offset, but one
/// request of 100 covers every track worth showing a dialog for.
const TRACK_CREDITS_CONTRIBUTOR_LIMIT: u32 = 100;

fn track_credits_body(track_uri: &str) -> String {
    serde_json::json!({
        "extensions": {
            "persistedQuery": { "sha256Hash": TRACK_CREDITS_QUERY_HASH, "version": 1 },
        },
        "operationName": "queryTrackCreditsGroupedModal",
        "variables": {
            "trackUri": track_uri,
            "contributorsLimit": TRACK_CREDITS_CONTRIBUTOR_LIMIT,
            "contributorsOffset": 0,
        },
    })
    .to_string()
}

/// Raw JSON of the credits GraphQL response. Optional throughout so a renamed
/// or missing field degrades to a thinner dialog rather than an error.
#[derive(Default, Deserialize)]
struct CreditsQueryJson {
    #[serde(default)]
    data: Option<CreditsDataJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // service field names
struct CreditsDataJson {
    #[serde(default)]
    trackUnion: Option<TrackUnionJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct TrackUnionJson {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    creditsTrait: Option<CreditsTraitJson>,
}

#[derive(Default, Deserialize)]
struct CreditsTraitJson {
    #[serde(default)]
    contributors: Option<ContributorItemsJson>,
    #[serde(default)]
    sources: Option<SourceItemsJson>,
}

#[derive(Default, Deserialize)]
struct ContributorItemsJson {
    #[serde(default)]
    items: Vec<ContributorJson>,
}

#[derive(Default, Deserialize)]
struct SourceItemsJson {
    #[serde(default)]
    items: Vec<NamedJson>,
}

#[derive(Default, Deserialize)]
struct NamedJson {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ContributorJson {
    #[serde(default)]
    name: Option<String>,
    /// Singular here, unlike the REST view's `subroles` array: the flat list
    /// carries one entry per person *per role*, and grouping merges them.
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    /// The external link the official client opens — for writers this is the
    /// `artists.spotify.com/songwriter/<id>` page. The client never builds
    /// this URL; the server supplies it finished, which is the only reason
    /// songwriter links are possible at all (that id space is not the artist
    /// one, and nothing in the payload maps between them).
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    reference: Option<ReferenceJson>,
    #[serde(default)]
    roleGroup: Option<NamedJson>,
}

#[derive(Default, Deserialize)]
struct ReferenceJson {
    #[serde(default)]
    url: Option<String>,
}

/// Groups the flat contributor list the way the official dialog does: bucket
/// by `roleGroup` name in first-seen order, then merge repeats of the same
/// person within a bucket, accumulating their roles.
fn track_credits_from_query(parsed: &CreditsQueryJson, track_uri: &str) -> TrackCredits {
    let track = parsed
        .data
        .as_ref()
        .and_then(|data| data.trackUnion.as_ref());
    let credits = track.and_then(|track| track.creditsTrait.as_ref());

    let mut roles: Vec<CreditRole> = Vec::new();
    for item in credits.iter().flat_map(|credits| {
        credits
            .contributors
            .as_ref()
            .map(|contributors| contributors.items.as_slice())
            .unwrap_or_default()
    }) {
        let Some(name) = item.name.clone().filter(|name| !name.is_empty()) else {
            continue;
        };
        let group_title = item
            .roleGroup
            .as_ref()
            .and_then(|group| group.name.clone())
            .unwrap_or_else(|| "Other".to_owned());
        let group = match roles.iter_mut().find(|role| role.title == group_title) {
            Some(existing) => existing,
            None => {
                roles.push(CreditRole {
                    title: group_title,
                    artists: Vec::new(),
                });
                roles.last_mut().expect("just pushed")
            }
        };

        let role = item.role.clone().unwrap_or_default();
        let uri = item.uri.clone().unwrap_or_default();
        let url = item
            .url
            .clone()
            .or_else(|| {
                item.reference
                    .as_ref()
                    .and_then(|reference| reference.url.clone())
            })
            .unwrap_or_default();
        if let Some(existing) = group.artists.iter_mut().find(|artist| {
            (!uri.is_empty() && artist.uri == uri)
                || (uri.is_empty() && artist.uri.is_empty() && artist.name == name)
        }) {
            if !role.is_empty() && !existing.subroles.contains(&role) {
                existing.subroles.push(role);
            }
            if existing.url.is_empty() && !url.is_empty() {
                existing.url = url;
            }
            continue;
        }
        group.artists.push(CreditArtist {
            id: credit_artist_id(&uri),
            uri,
            url,
            name,
            subroles: if role.is_empty() {
                Vec::new()
            } else {
                vec![role]
            },
        });
    }

    TrackCredits {
        track_uri: track_uri.to_owned(),
        track_name: track
            .and_then(|track| track.name.clone())
            .unwrap_or_default(),
        roles,
        source: credits
            .and_then(|credits| credits.sources.as_ref())
            .map(|sources| {
                sources
                    .items
                    .iter()
                    .filter_map(|source| source.name.clone())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// search (pathfinder searchDesktop)
// ---------------------------------------------------------------------------

/// Raw JSON of the pathfinder `searchDesktop` GraphQL response. Field names
/// follow the persisted-query's schema and stay optional everywhere so a
/// renamed/missing field degrades to an empty section, never an error.
#[derive(Default, Deserialize)]
struct SearchDesktopResponse {
    #[serde(default)]
    data: SearchDesktopData,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchDesktopData {
    #[serde(default)]
    searchV2: SearchV2Json,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchV2Json {
    #[serde(default)]
    topResultsV2: SearchTopSectionJson,
    #[serde(default)]
    tracksV2: SearchTrackSectionJson,
    #[serde(default)]
    albumsV2: SearchAlbumSectionJson,
    #[serde(default)]
    artists: SearchArtistSectionJson,
    #[serde(default)]
    playlists: Option<SearchPlaylistSectionJson>,
}

/// The cross-kind ranking. `numberOfTopResults` in the request variables is
/// what asks for it, so it has been arriving in every response all along.
#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchTopSectionJson {
    #[serde(default)]
    itemsV2: Vec<SearchTopItemJson>,
}

#[derive(Default, Deserialize)]
struct SearchTopItemJson {
    #[serde(default)]
    item: Option<SearchTopWrapperJson>,
}

#[derive(Default, Deserialize)]
struct SearchTopWrapperJson {
    #[serde(default)]
    data: Option<SearchTopDataJson>,
}

/// One ranked hit, discriminated by its own `__typename`. The payloads are
/// the same shapes the per-kind sections carry, so they reuse the same
/// structs and the same `*_ref_from_hit` mappers.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum SearchTopDataJson {
    Track(SearchTrackHitJson),
    Album(SearchAlbumHitJson),
    Artist(SearchArtistHitJson),
    Playlist(SearchPlaylistHitJson),
    /// Podcasts, episodes, audiobooks and users rank here too. The app has
    /// nowhere to send them, so they are skipped and the next hit is tried.
    #[serde(other)]
    Unsupported,
}

#[derive(Default, Deserialize)]
struct SearchPlaylistSectionJson {
    #[serde(default)]
    items: Option<Vec<SearchPlaylistWrapperJson>>,
}

#[derive(Default, Deserialize)]
struct SearchPlaylistWrapperJson {
    #[serde(default)]
    data: Option<SearchPlaylistHitJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchPlaylistHitJson {
    #[serde(default, rename = "__typename")]
    typename: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    ownerV2: Option<SearchPlaylistOwnerV2Json>,
    #[serde(default)]
    images: Option<SearchPlaylistImagesJson>,
    #[serde(default)]
    content: Option<SearchPlaylistContentJson>,
}

#[derive(Default, Deserialize)]
struct SearchPlaylistOwnerV2Json {
    #[serde(default)]
    data: Option<SearchPlaylistOwnerJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchPlaylistOwnerJson {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Default, Deserialize)]
struct SearchPlaylistImagesJson {
    #[serde(default)]
    items: Option<Vec<SearchCoverArtJson>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchPlaylistContentJson {
    #[serde(default)]
    totalCount: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct SearchTrackSectionJson {
    #[serde(default)]
    items: Vec<SearchTrackWrapperJson>,
}

#[derive(Default, Deserialize)]
struct SearchAlbumSectionJson {
    #[serde(default)]
    items: Vec<SearchAlbumWrapperJson>,
}

#[derive(Default, Deserialize)]
struct SearchArtistSectionJson {
    #[serde(default)]
    items: Vec<SearchArtistWrapperJson>,
}

/// Track hits are wrapped twice: `items[].item.data`.
#[derive(Default, Deserialize)]
struct SearchTrackWrapperJson {
    #[serde(default)]
    item: SearchTrackItemJson,
}

#[derive(Default, Deserialize)]
struct SearchTrackItemJson {
    #[serde(default)]
    data: SearchTrackHitJson,
}

/// Album/artist hits are wrapped once: `items[].data`.
#[derive(Default, Deserialize)]
struct SearchAlbumWrapperJson {
    #[serde(default)]
    data: SearchAlbumHitJson,
}

#[derive(Default, Deserialize)]
struct SearchArtistWrapperJson {
    #[serde(default)]
    data: SearchArtistHitJson,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchTrackHitJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artists: SearchArtistListJson,
    #[serde(default)]
    albumOfTrack: Option<SearchAlbumOfTrackJson>,
    #[serde(default)]
    duration: Option<SearchDurationJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchAlbumHitJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artists: SearchArtistListJson,
    #[serde(default)]
    coverArt: Option<SearchCoverArtJson>,
    #[serde(default)]
    date: Option<SearchDateJson>,
}

/// Release date of an album hit. The searchDesktop response carries only the
/// year here, which is exactly the precision [`AlbumRef::year`] wants.
#[derive(Default, Deserialize)]
struct SearchDateJson {
    #[serde(default)]
    year: Option<u32>,
}

#[derive(Default, Deserialize)]
struct SearchArtistHitJson {
    #[serde(default)]
    uri: Option<String>,
    /// Some responses carry a top-level `name`, others only `profile.name`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    profile: Option<SearchProfileJson>,
    #[serde(default)]
    visuals: Option<SearchArtistVisualsJson>,
}

/// Artist artwork in a search hit. The avatar is the same image group the
/// artist page shows, in the same `sources` shape as album `coverArt`; an
/// artist with no picture arrives as an explicit `"avatarImage": null`
/// (verified live 2026-08-19), which is why it is an `Option` rather than a
/// defaulted struct.
#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchArtistVisualsJson {
    #[serde(default)]
    avatarImage: Option<SearchCoverArtJson>,
}

#[derive(Default, Deserialize)]
struct SearchArtistListJson {
    #[serde(default)]
    items: Vec<SearchArtistJson>,
}

#[derive(Default, Deserialize)]
struct SearchArtistJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    profile: Option<SearchProfileJson>,
}

#[derive(Default, Deserialize)]
struct SearchProfileJson {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchAlbumOfTrackJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    coverArt: Option<SearchCoverArtJson>,
}

#[derive(Default, Deserialize)]
struct SearchCoverArtJson {
    #[serde(default)]
    sources: Vec<SearchImageSourceJson>,
}

#[derive(Default, Deserialize)]
struct SearchImageSourceJson {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    width: Option<u32>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)] // GraphQL schema field names
struct SearchDurationJson {
    /// The schema types this as an Int; accept strings too for tolerance.
    #[serde(default)]
    totalMilliseconds: Option<serde_json::Value>,
}

/// Extracts the 22-char id from a `spotify:{type}:{id}` uri.
fn hit_id(raw_uri: Option<&str>) -> String {
    raw_uri
        .and_then(|uri| uri.rsplit(':').next())
        .unwrap_or_default()
        .to_owned()
}

/// Smallest image a search hit may be painted from: covers and avatars are
/// rendered at up to ~200 CSS px, so anything at least this wide is sharp on
/// a 1.5x display.
const HIT_IMAGE_MIN_WIDTH: u32 = 300;

/// Picks an artwork URL from a hit's image sources: the *smallest* source at
/// least [`HIT_IMAGE_MIN_WIDTH`] wide, falling back to the widest one when
/// every source is smaller than that.
///
/// Smallest-that-is-big-enough rather than first-that-is-big-enough because
/// the `sources` array is not sorted, and not even consistently ordered
/// between entity types: album covers arrive 300 px first, artist avatars
/// arrive 640/160/320 (verified live 2026-08-19). Taking the first match
/// would download and cache a 640 px portrait to paint a tile that is never
/// larger than 200, doubling the bytes on disk for no visible difference.
fn hit_image(cover: Option<&SearchCoverArtJson>) -> Option<String> {
    let sources = &cover?.sources;
    let width = |source: &&SearchImageSourceJson| source.width.unwrap_or(0);
    sources
        .iter()
        .filter(|source| width(source) >= HIT_IMAGE_MIN_WIDTH)
        .min_by_key(width)
        .or_else(|| sources.iter().max_by_key(width))
        .and_then(|source| source.url.clone())
        .filter(|url| !url.is_empty())
}

fn parse_millis(value: Option<&serde_json::Value>) -> u32 {
    let millis = value.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    });
    millis.and_then(|ms| u32::try_from(ms).ok()).unwrap_or(0)
}

fn artist_name(artist: &SearchArtistJson) -> String {
    artist
        .profile
        .as_ref()
        .and_then(|profile| profile.name.clone())
        .unwrap_or_default()
}

fn track_ref_from_hit(hit: &SearchTrackHitJson) -> TrackRef {
    TrackRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        artist_names: hit.artists.items.iter().map(artist_name).collect(),
        // The search hit already carries a uri per artist alongside the name.
        artist_ids: hit
            .artists
            .items
            .iter()
            .map(|artist| hit_id(artist.uri.as_deref()))
            .collect(),
        artist_id: hit
            .artists
            .items
            .first()
            .map(|artist| hit_id(artist.uri.as_deref()))
            .unwrap_or_default(),
        album_id: hit
            .albumOfTrack
            .as_ref()
            .map(|album| hit_id(album.uri.as_deref()))
            .unwrap_or_default(),
        album_name: hit
            .albumOfTrack
            .as_ref()
            .and_then(|album| album.name.clone())
            .unwrap_or_default(),
        cover_url: hit
            .albumOfTrack
            .as_ref()
            .and_then(|album| hit_image(album.coverArt.as_ref()))
            .unwrap_or_default(),
        duration_ms: parse_millis(
            hit.duration
                .as_ref()
                .and_then(|duration| duration.totalMilliseconds.as_ref()),
        ),
        play_count: None,
        added_at: None,
        unavailable: hit
            .duration
            .as_ref()
            .and_then(|duration| duration.totalMilliseconds.as_ref())
            .is_some_and(|value| parse_millis(Some(value)) == 0),
        unavailable_reason: hit
            .duration
            .as_ref()
            .and_then(|duration| duration.totalMilliseconds.as_ref())
            .filter(|value| parse_millis(Some(value)) == 0)
            .map(|_| "invalid track duration".to_owned()),
        // A pathfinder search hit carries no file ids, so cache state is not
        // knowable here. It is answered only where real track metadata is
        // parsed; see `track_is_cached`.
        cached: false,
        context: String::new(),
        effective_edit: None,
    }
}

fn album_ref_from_hit(hit: &SearchAlbumHitJson) -> AlbumRef {
    AlbumRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        artist_names: hit.artists.items.iter().map(artist_name).collect(),
        artist_ids: hit
            .artists
            .items
            .iter()
            .map(|artist| hit_id(artist.uri.as_deref()))
            .collect(),
        cover_url: hit_image(hit.coverArt.as_ref()),
        year: hit
            .date
            .as_ref()
            .and_then(|date| date.year)
            .filter(|year| *year > 0),
    }
}

fn artist_ref_from_hit(hit: &SearchArtistHitJson) -> ArtistRef {
    ArtistRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit
            .name
            .clone()
            .or_else(|| {
                hit.profile
                    .as_ref()
                    .and_then(|profile| profile.name.clone())
            })
            .unwrap_or_default(),
        // Free: the avatar travels in the same searchDesktop response as the
        // name, so a portrait costs no extra round-trip.
        portrait_url: hit_image(
            hit.visuals
                .as_ref()
                .and_then(|visuals| visuals.avatarImage.as_ref()),
        ),
    }
}

fn parse_count(value: Option<&serde_json::Value>) -> Option<u32> {
    let count = value.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    });
    count.and_then(|count| u32::try_from(count).ok())
}

fn playlist_ref_from_hit(hit: &SearchPlaylistHitJson) -> PlaylistRef {
    let owner = hit.ownerV2.as_ref().and_then(|owner| owner.data.as_ref());
    let owner_id = owner
        .and_then(|owner| owner.username.clone())
        .filter(|username| !username.is_empty())
        .or_else(|| owner.map(|owner| hit_id(owner.uri.as_deref())))
        .unwrap_or_default();
    PlaylistRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        description: hit.description.as_deref().and_then(playlist_description),
        owner_id,
        owner_name: owner
            .and_then(|owner| owner.name.clone())
            .unwrap_or_default(),
        cover_url: hit
            .images
            .as_ref()
            .and_then(|images| images.items.as_deref())
            .and_then(|images| hit_image(images.first())),
        track_count: hit
            .content
            .as_ref()
            .and_then(|content| parse_count(content.totalCount.as_ref())),
    }
}
/// Drops repeats of an id, keeping the first and the service's ordering.
///
/// The per-kind sections can carry one entity twice: a live `OK Computer`
/// search returns the same album id in two album slots. A grid that draws the
/// same record twice is wrong on its own terms, and it also throws in the view
/// — the card grids are keyed by id, and Svelte refuses a duplicate key — so
/// the whole results page fails to render on nothing worse than a repeated
/// row. Fixed here, where the id is the identity, rather than per view.
fn dedupe_by_id<T>(items: Vec<T>, id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(id(item).to_owned()))
        .collect()
}

/// The highest-ranked hit this app can actually open.
///
/// Walks the ranking in order rather than taking `[0]`, so a query whose best
/// answer is a podcast still yields the best *playable* answer underneath it
/// instead of nothing. A hit missing an id or a name is skipped for the same
/// reason: a top result that cannot be opened or named is not a result.
/// How well a candidate's name answers what was typed.
///
/// `topResultsV2` is a ranked list, not a chosen answer: nothing in the
/// response says "this is the top result" — its `featured` sibling is an
/// editorial playlist, not a pick — and the ranking itself is weighted by
/// popularity. Searching "taylor" therefore ranks the track "Style" above the
/// artist Taylor Swift, which is not what someone typing an artist's name is
/// asking for, while "kygo" ranks the artist first because the name matches
/// exactly. So the query has to break the tie.
fn top_match_rank(query: &str, name: &str) -> u8 {
    let query = query.trim().to_lowercase();
    let name = name.trim().to_lowercase();
    if query.is_empty() || name.is_empty() {
        return 0;
    }
    if name == query {
        return 3;
    }
    if name.starts_with(&query) {
        return 2;
    }
    // "rhapsody" should still find "Bohemian Rhapsody"; a bare substring would
    // also match the middle of a word, which is noise rather than an answer.
    if name.split_whitespace().any(|word| word.starts_with(&query)) {
        return 1;
    }
    0
}

/// Picks the top result: best name match wins, and Spotify's own order breaks
/// ties within a match quality. Their ranking is good, it just answers a
/// different question than the one the search box asked.
fn search_top_ref(
    query: &str,
    section: &SearchTopSectionJson,
) -> Option<renderer_engine::protocol::SearchTopRef> {
    use renderer_engine::protocol::SearchTopRef;
    section
        .itemsV2
        .iter()
        .filter_map(|entry| entry.item.as_ref()?.data.as_ref())
        .filter_map(|data| {
            let (top, id, name) = match data {
                SearchTopDataJson::Track(hit) => {
                    let reference = track_ref_from_hit(hit);
                    let (id, name) = (reference.id.clone(), reference.name.clone());
                    (SearchTopRef::Track(reference), id, name)
                }
                SearchTopDataJson::Album(hit) => {
                    let reference = album_ref_from_hit(hit);
                    let (id, name) = (reference.id.clone(), reference.name.clone());
                    (SearchTopRef::Album(reference), id, name)
                }
                SearchTopDataJson::Artist(hit) => {
                    let reference = artist_ref_from_hit(hit);
                    let (id, name) = (reference.id.clone(), reference.name.clone());
                    (SearchTopRef::Artist(reference), id, name)
                }
                SearchTopDataJson::Playlist(hit) => {
                    let reference = playlist_ref_from_hit(hit);
                    let (id, name) = (reference.id.clone(), reference.name.clone());
                    (SearchTopRef::Playlist(reference), id, name)
                }
                SearchTopDataJson::Unsupported => return None,
            };
            if id.is_empty() || name.is_empty() {
                return None;
            }
            Some((top_match_rank(query, &name), top))
        })
        .enumerate()
        .max_by_key(|(index, (rank, _))| (*rank, std::cmp::Reverse(*index)))
        .map(|(_, (_, top))| top)
}

fn overview_playlist_ref(item: &SearchPlaylistWrapperJson) -> Option<PlaylistRef> {
    let hit = item.data.as_ref()?;
    if hit.typename.as_deref() != Some("Playlist") {
        return None;
    }
    let reference = playlist_ref_from_hit(hit);
    (!reference.id.is_empty() && !reference.name.is_empty()).then_some(reference)
}

/// Maps `profile.pinnedItem` onto the three kinds an Artist Pick can be.
///
/// The union is discriminated by the payload's own `__typename`, and each arm
/// is handed to the same mapper the search results use, so a pinned album and a
/// searched album become the same `AlbumRef` by the same code. An unknown
/// `__typename` (Spotify has added kinds before, and will again) yields `None`
/// rather than a card with no name in it.
///
/// A pick with no resolvable id or name is dropped: the card's whole job is to
/// be clicked, and there is nothing to open without an id.
fn artist_pick_from_pinned_item(pinned: &ArtistOverviewPinnedItemJson) -> Option<ArtistPick> {
    let data = pinned.itemV2.as_ref()?.data.as_ref()?;
    let typename = data.get("__typename").and_then(serde_json::Value::as_str)?;
    let item = match typename {
        "Playlist" => {
            let hit: SearchPlaylistHitJson = serde_json::from_value(data.clone()).ok()?;
            ArtistPickItem::Playlist(playlist_ref_from_hit(&hit))
        }
        "Album" => {
            let hit: SearchAlbumHitJson = serde_json::from_value(data.clone()).ok()?;
            ArtistPickItem::Album(album_ref_from_hit(&hit))
        }
        "Track" => {
            let hit: SearchTrackHitJson = serde_json::from_value(data.clone()).ok()?;
            ArtistPickItem::Track(track_ref_from_hit(&hit))
        }
        _ => return None,
    };
    let (id, name) = match &item {
        ArtistPickItem::Playlist(playlist) => (&playlist.id, &playlist.name),
        ArtistPickItem::Album(album) => (&album.id, &album.name),
        ArtistPickItem::Track(track) => (&track.id, &track.name),
    };
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some(ArtistPick {
        comment: pinned
            .comment
            .as_deref()
            .map(str::trim)
            .filter(|comment| !comment.is_empty())
            .map(str::to_owned),
        item,
    })
}

/// Keeps only server-tagged playlist entries from an artist overview section,
/// preserving the service's order and dropping duplicate URIs.
fn overview_playlist_refs(section: Option<&ArtistOverviewPlaylistSectionJson>) -> Vec<PlaylistRef> {
    let mut seen = HashSet::new();
    section
        .and_then(|section| section.items.as_deref())
        .into_iter()
        .flatten()
        .filter_map(overview_playlist_ref)
        .filter(|reference| seen.insert(reference.uri.clone()))
        .collect()
}

/// Pathfinder persisted-query hashes for `operationName=searchDesktop`,
/// newest first. The hashes rotate; when the primary is rejected the
/// previous one is tried. Both verified live against a real session
/// (2026-08-18).
const SEARCH_DESKTOP_HASHES: &[&str] = &[
    "3c9d3f60dac5dea3876b6db3f534192b1c1d90032c4233c1bbaba526db41eb31",
    "21969b655b795601fb2d2204a4243188e75fdc6d3520e7b9cd3f4db2aff9591e",
];

/// The pathfinder GraphQL endpoint the official web/desktop client uses for
/// search.
const PATHFINDER_ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const ARTIST_OVERVIEW_SUCCESS_TTL: Duration = Duration::from_secs(5 * 60);
const ARTIST_OVERVIEW_FAILURE_TTL: Duration = Duration::from_secs(30);
const ARTIST_OVERVIEW_CACHE_CAPACITY: usize = 64;

/// Discovery is an optional page enhancement, but a revisit must not repeat
/// its search and metadata work. Keep both hits and misses in process memory;
/// the engine owns one authenticated session and this cache disappears with
/// that process. The capacity matches the existing artist-overview cache.
const SONGWRITER_PLAYLIST_CACHE_CAPACITY: usize = ARTIST_OVERVIEW_CACHE_CAPACITY;
/// Mirrors the artist-overview pairing: a real playlist survives a normal
/// revisit window, while a failed discovery must not hammer search on every
/// Back-and-forth either.
const SONGWRITER_PLAYLIST_SUCCESS_TTL: Duration = Duration::from_secs(5 * 60);
const SONGWRITER_PLAYLIST_FAILURE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct SongwriterPlaylistCacheEntry {
    fetched_at: Instant,
    value: Option<SongwriterPlaylist>,
}

#[derive(Default)]
struct SongwriterPlaylistCache {
    entries: HashMap<String, SongwriterPlaylistCacheEntry>,
}

impl SongwriterPlaylistCache {
    fn ttl(value: &Option<SongwriterPlaylist>) -> Duration {
        if value.is_some() {
            SONGWRITER_PLAYLIST_SUCCESS_TTL
        } else {
            SONGWRITER_PLAYLIST_FAILURE_TTL
        }
    }

    /// Entries expire the same way the artist-overview ones do, so a stale
    /// playlist hint cannot outlive its shelf life by resting unread.
    fn get(&mut self, artist_id: &str, now: Instant) -> Option<Option<SongwriterPlaylist>> {
        let entry = self.entries.get(artist_id)?;
        if now.duration_since(entry.fetched_at) >= Self::ttl(&entry.value) {
            self.entries.remove(artist_id);
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(&mut self, artist_id: String, value: Option<SongwriterPlaylist>, now: Instant) {
        self.entries
            .retain(|_, entry| now.duration_since(entry.fetched_at) < Self::ttl(&entry.value));
        let key_is_absent = !self.entries.contains_key(&artist_id);
        evict_oldest_for_fresh_key(
            &mut self.entries,
            SONGWRITER_PLAYLIST_CACHE_CAPACITY,
            key_is_absent,
            |entry| entry.fetched_at,
        );
        self.entries.insert(
            artist_id,
            SongwriterPlaylistCacheEntry {
                fetched_at: now,
                value,
            },
        );
    }
}

static SONGWRITER_PLAYLIST_CACHE: LazyLock<Mutex<SongwriterPlaylistCache>> =
    LazyLock::new(|| Mutex::new(SongwriterPlaylistCache::default()));

/// Persisted `queryArtistOverview` documents newest first. Each historical
/// document keeps the exact variable shape it was captured with; GraphQL
/// gateways are not required to ignore undeclared variables.
const ARTIST_OVERVIEW_SCHEMAS: &[ArtistOverviewSchema] = &[
    // Current desktop client document (Spotify 1.2.97): includes the richer
    // artist visuals. A successful historical schema can omit those fields.
    ArtistOverviewSchema {
        hash: "1ac33ddab5d39a3a9c27802774e6d78b9405cc188c6f75aed007df2a32737c72",
        variables: ArtistOverviewVariables::Locale,
    },
    ArtistOverviewSchema {
        hash: "ae0e2958a4ab645b35ca19ac04d0495ae12d9c5d7b7286217674801a9aab281a",
        variables: ArtistOverviewVariables::LocaleAndPreRelease,
    },
    ArtistOverviewSchema {
        hash: "5b9e64f43843fa3a9b6a98543600299b0a2cbbbccfdcdcef2402eb9c1017ca4c",
        variables: ArtistOverviewVariables::PreRelease,
    },
    ArtistOverviewSchema {
        hash: "d66221ea13998b2f81883c5187d174c8646e4041d67f5b1e103bc262d447e3a0",
        variables: ArtistOverviewVariables::UriOnly,
    },
];

#[derive(Clone, Copy)]
struct ArtistOverviewSchema {
    hash: &'static str,
    variables: ArtistOverviewVariables,
}

#[derive(Clone, Copy)]
enum ArtistOverviewVariables {
    LocaleAndPreRelease,
    Locale,
    PreRelease,
    UriOnly,
}

#[derive(Clone, Debug, Default)]
struct ArtistOverviewQuery {
    overview: ArtistOverview,
    top_playcounts: HashMap<String, u64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ArtistOverviewCacheKey {
    artist_id: String,
    /// The primary hash identifies the parser/request schema compiled into
    /// this process. A future hash rotation cannot reuse an older entry.
    schema_hash: &'static str,
}

#[derive(Clone)]
struct ArtistOverviewCacheEntry {
    fetched_at: Instant,
    /// Failures are cached too: an optional enhancement should not repeat four
    /// rejected documents whenever Back revisits an artist.
    value: Option<ArtistOverviewQuery>,
}

#[derive(Default)]
struct ArtistOverviewCache {
    entries: HashMap<ArtistOverviewCacheKey, ArtistOverviewCacheEntry>,
}

impl ArtistOverviewCache {
    fn ttl(value: &Option<ArtistOverviewQuery>) -> Duration {
        if value.is_some() {
            ARTIST_OVERVIEW_SUCCESS_TTL
        } else {
            ARTIST_OVERVIEW_FAILURE_TTL
        }
    }

    fn get(
        &mut self,
        key: &ArtistOverviewCacheKey,
        now: Instant,
    ) -> Option<Option<ArtistOverviewQuery>> {
        let entry = self.entries.get(key)?;
        if now.duration_since(entry.fetched_at) >= Self::ttl(&entry.value) {
            self.entries.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(
        &mut self,
        key: ArtistOverviewCacheKey,
        value: Option<ArtistOverviewQuery>,
        now: Instant,
    ) {
        self.entries
            .retain(|_, entry| now.duration_since(entry.fetched_at) < Self::ttl(&entry.value));
        let key_is_absent = !self.entries.contains_key(&key);
        evict_oldest_for_fresh_key(
            &mut self.entries,
            ARTIST_OVERVIEW_CACHE_CAPACITY,
            key_is_absent,
            |entry| entry.fetched_at,
        );
        self.entries.insert(
            key,
            ArtistOverviewCacheEntry {
                fetched_at: now,
                value,
            },
        );
    }
}

static ARTIST_OVERVIEW_CACHE: LazyLock<Mutex<ArtistOverviewCache>> =
    LazyLock::new(|| Mutex::new(ArtistOverviewCache::default()));

#[derive(Default, Deserialize)]
struct ArtistOverviewResponseJson {
    #[serde(default)]
    data: Option<ArtistOverviewDataJson>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewDataJson {
    #[serde(default)]
    artistUnion: Option<ArtistOverviewUnionJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewUnionJson {
    #[serde(default)]
    profile: Option<ArtistOverviewProfileJson>,
    #[serde(default)]
    visuals: Option<ArtistOverviewVisualsJson>,
    #[serde(default)]
    stats: Option<ArtistOverviewStatsJson>,
    #[serde(default)]
    discography: Option<ArtistOverviewDiscographyJson>,
    #[serde(default)]
    relatedContent: Option<ArtistOverviewRelatedContentJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewProfileJson {
    #[serde(default)]
    biography: Option<ArtistBiographyJson>,
    #[serde(default)]
    playlistsV2: Option<ArtistOverviewPlaylistSectionJson>,
    #[serde(default)]
    pinnedItem: Option<ArtistOverviewPinnedItemJson>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewPlaylistSectionJson {
    #[serde(default)]
    items: Option<Vec<SearchPlaylistWrapperJson>>,
}

/// `profile.pinnedItem` is a wrapper around a UNION, so its payload is kept as
/// raw JSON and re-read once the `__typename` inside has said which of the
/// three documented shapes it is. That is deliberately not a superset struct:
/// the three hit shapes already exist and are already exercised by search, and
/// one flat struct holding every field of all three would silently accept a
/// playlist that happened to carry a `date`.
#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewPinnedItemJson {
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    itemV2: Option<ArtistPinnedItemWrapperJson>,
}

#[derive(Default, Deserialize)]
struct ArtistPinnedItemWrapperJson {
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistBiographyJson {
    #[serde(default)]
    text: Option<String>,
}

/// Overview-only playlist shelves. `featuringV2` is intentionally absent:
/// Spotify loads it through the separate `queryArtistFeaturing` document.
#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewRelatedContentJson {
    #[serde(default)]
    relatedArtists: Option<ArtistRelatedArtistsJson>,
    #[serde(default)]
    discoveredOnV2: Option<ArtistOverviewPlaylistSectionJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewStatsJson {
    #[serde(default)]
    followers: Option<serde_json::Value>,
    #[serde(default)]
    monthlyListeners: Option<serde_json::Value>,
    #[serde(default)]
    worldRank: Option<serde_json::Value>,
    #[serde(default)]
    topCities: Option<ArtistTopCitiesJson>,
}

#[derive(Default, Deserialize)]
struct ArtistTopCitiesJson {
    #[serde(default)]
    items: Option<Vec<ArtistTopCityJson>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistTopCityJson {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    numberOfListeners: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewDiscographyJson {
    #[serde(default)]
    topTracks: Option<ArtistOverviewTopTracksJson>,
    #[serde(default)]
    popularReleasesAlbums: Option<ArtistPopularReleasesJson>,
    #[serde(default)]
    popularReleasesSingles: Option<ArtistPopularReleasesJson>,
    #[serde(default)]
    popularReleasesCompilations: Option<ArtistPopularReleasesJson>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewTopTracksJson {
    #[serde(default)]
    items: Option<Vec<ArtistOverviewTopTrackItemJson>>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewTopTrackItemJson {
    #[serde(default)]
    track: Option<ArtistOverviewTrackJson>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewTrackJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    playcount: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct ArtistPopularReleasesJson {
    #[serde(default)]
    items: Option<Vec<ArtistPopularReleaseJson>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistPopularReleaseJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    coverArt: Option<ArtistOverviewImageJson>,
    #[serde(default)]
    date: Option<SearchDateJson>,
    #[serde(default)]
    artists: Option<ArtistOverviewArtistListJson>,
}

#[derive(Default, Deserialize)]
struct ArtistRelatedArtistsJson {
    #[serde(default)]
    items: Option<Vec<ArtistOverviewRelatedArtistJson>>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewImageJson {
    #[serde(default)]
    sources: Option<Vec<SearchImageSourceJson>>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewVisualsJson {
    #[serde(default)]
    avatarImage: Option<ArtistOverviewImageJson>,
    #[serde(default)]
    gallery: Option<ArtistOverviewGalleryJson>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewGalleryJson {
    #[serde(default)]
    items: Option<Vec<ArtistOverviewImageJson>>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewArtistListJson {
    #[serde(default)]
    items: Option<Vec<SearchArtistJson>>,
}

#[derive(Default, Deserialize)]
struct ArtistOverviewRelatedArtistJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    profile: Option<SearchProfileJson>,
    #[serde(default)]
    visuals: Option<ArtistOverviewRelatedVisualsJson>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistOverviewRelatedVisualsJson {
    #[serde(default)]
    avatarImage: Option<ArtistOverviewImageJson>,
}
fn artist_overview_body(artist_uri: &str, schema: ArtistOverviewSchema) -> String {
    let variables = match schema.variables {
        ArtistOverviewVariables::LocaleAndPreRelease => serde_json::json!({
            "uri": artist_uri,
            "locale": "",
            "preReleaseV2": false,
        }),
        ArtistOverviewVariables::Locale => serde_json::json!({
            "uri": artist_uri,
            "locale": "",
        }),
        ArtistOverviewVariables::PreRelease => serde_json::json!({
            "uri": artist_uri,
            "preReleaseV2": false,
        }),
        ArtistOverviewVariables::UriOnly => serde_json::json!({
            "uri": artist_uri,
        }),
    };
    serde_json::json!({
        "extensions": {
            "persistedQuery": { "sha256Hash": schema.hash, "version": 1 },
        },
        "operationName": "queryArtistOverview",
        "variables": variables,
    })
    .to_string()
}

fn overview_count(value: Option<&serde_json::Value>) -> Option<u64> {
    playcount_value(value).filter(|count| *count > 0)
}

fn overview_image(image: Option<&ArtistOverviewImageJson>) -> Option<String> {
    let sources = image?.sources.as_deref().unwrap_or_default();
    let width = |source: &&SearchImageSourceJson| source.width.unwrap_or(0);
    sources
        .iter()
        .filter(|source| width(source) >= HIT_IMAGE_MIN_WIDTH)
        .min_by_key(width)
        .or_else(|| sources.iter().max_by_key(width))
        .and_then(|source| source.url.clone())
        .filter(|url| !url.is_empty())
}

fn overview_largest_image(image: Option<&ArtistOverviewImageJson>) -> Option<String> {
    image?
        .sources
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|source| source.url.as_deref().is_some_and(|url| !url.is_empty()))
        .max_by_key(|source| source.width.unwrap_or(0))
        .and_then(|source| source.url.clone())
}

fn overview_related_artist_ref(artist: &ArtistOverviewRelatedArtistJson) -> ArtistRef {
    ArtistRef {
        id: hit_id(artist.uri.as_deref()),
        uri: artist.uri.clone().unwrap_or_default(),
        name: artist
            .name
            .clone()
            .or_else(|| {
                artist
                    .profile
                    .as_ref()
                    .and_then(|profile| profile.name.clone())
            })
            .unwrap_or_default(),
        portrait_url: overview_image(
            artist
                .visuals
                .as_ref()
                .and_then(|visuals| visuals.avatarImage.as_ref()),
        ),
    }
}

fn popular_release_ref(release: &ArtistPopularReleaseJson) -> Option<AlbumRef> {
    let uri = release.uri.clone().filter(|uri| !uri.is_empty())?;
    let name = release.name.clone().filter(|name| !name.is_empty())?;
    let artists = release.artists.as_ref();
    Some(AlbumRef {
        id: hit_id(Some(&uri)),
        uri,
        name,
        artist_names: artists
            .into_iter()
            .flat_map(|artists| artists.items.as_deref().unwrap_or_default())
            .map(artist_name)
            .collect(),
        artist_ids: artists
            .into_iter()
            .flat_map(|artists| artists.items.as_deref().unwrap_or_default())
            .map(|artist| hit_id(artist.uri.as_deref()))
            .collect(),
        cover_url: overview_image(release.coverArt.as_ref()),
        year: release
            .date
            .as_ref()
            .and_then(|date| date.year)
            .filter(|year| *year > 0),
    })
}

fn parse_artist_overview_payload(payload: &[u8]) -> Result<ArtistOverviewQuery, String> {
    let ArtistOverviewResponseJson { data, errors } = serde_json::from_slice(payload)
        .map_err(|error| format!("unparseable artist overview response: {error}"))?;
    let artist = data.and_then(|data| data.artistUnion).ok_or_else(|| {
        if errors.as_deref().unwrap_or_default().is_empty() {
            "artist overview response contained no artist".to_owned()
        } else {
            "artist overview document was rejected".to_owned()
        }
    })?;

    let profile = artist.profile.as_ref();
    let stats = artist.stats.as_ref();
    let discography = artist.discography.as_ref();
    let visuals = artist.visuals.as_ref();
    let header_image_url =
        overview_largest_image(visuals.and_then(|visuals| visuals.avatarImage.as_ref()));
    let biography_image_url = visuals
        .and_then(|visuals| visuals.gallery.as_ref())
        .and_then(|gallery| gallery.items.as_deref())
        .unwrap_or_default()
        .iter()
        .find_map(|image| overview_largest_image(Some(image)));
    let biography = profile
        .and_then(|profile| profile.biography.as_ref())
        .and_then(|biography| biography.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let followers = stats.and_then(|stats| overview_count(stats.followers.as_ref()));
    let monthly_listeners = stats.and_then(|stats| overview_count(stats.monthlyListeners.as_ref()));
    let world_rank = stats
        .and_then(|stats| overview_count(stats.worldRank.as_ref()))
        .and_then(|rank| u32::try_from(rank).ok());
    let top_cities = stats
        .and_then(|stats| stats.topCities.as_ref())
        .and_then(|cities| cities.items.as_deref())
        .unwrap_or_default()
        .iter()
        .filter_map(|city| {
            let city_name = city.city.clone().filter(|name| !name.is_empty())?;
            Some(ArtistTopCity {
                city: city_name,
                country: city.country.clone().unwrap_or_default(),
                region: city.region.clone().unwrap_or_default(),
                listeners: overview_count(city.numberOfListeners.as_ref()),
            })
        })
        .collect();
    let discovered_on = overview_playlist_refs(
        artist
            .relatedContent
            .as_ref()
            .and_then(|content| content.discoveredOnV2.as_ref()),
    );
    let artist_playlists =
        overview_playlist_refs(profile.and_then(|profile| profile.playlistsV2.as_ref()));
    let artist_pick = profile
        .and_then(|profile| profile.pinnedItem.as_ref())
        .and_then(artist_pick_from_pinned_item);

    // These three arrays are independently server ranked. Preserve both their
    // category order and each array's item order; never re-sort by year/name.
    let mut popular_releases = Vec::new();
    let mut seen_releases = HashSet::new();
    if let Some(discography) = discography {
        for section in [
            discography.popularReleasesAlbums.as_ref(),
            discography.popularReleasesSingles.as_ref(),
            discography.popularReleasesCompilations.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for release in section.items.as_deref().unwrap_or_default() {
                let Some(release) = popular_release_ref(release) else {
                    continue;
                };
                if seen_releases.insert(release.uri.clone()) {
                    popular_releases.push(release);
                }
            }
        }
    }

    let related_artists = artist
        .relatedContent
        .as_ref()
        .and_then(|content| content.relatedArtists.as_ref())
        .and_then(|artists| artists.items.as_deref())
        .unwrap_or_default()
        .iter()
        .map(overview_related_artist_ref)
        .filter(|artist| !artist.id.is_empty() && !artist.name.is_empty())
        .collect();

    let mut top_playcounts = HashMap::new();
    if let Some(items) = discography
        .and_then(|discography| discography.topTracks.as_ref())
        .and_then(|tracks| tracks.items.as_deref())
    {
        for track in items.iter().filter_map(|item| item.track.as_ref()) {
            let Some(uri) = track.uri.as_ref().filter(|uri| !uri.is_empty()) else {
                continue;
            };
            let Some(playcount) = overview_count(track.playcount.as_ref()) else {
                continue;
            };
            top_playcounts.insert(uri.clone(), playcount);
        }
    }

    Ok(ArtistOverviewQuery {
        overview: ArtistOverview {
            biography,
            header_image_url,
            biography_image_url,
            popularity: None,
            followers,
            monthly_listeners,
            world_rank,
            top_cities,
            popular_releases,
            related_artists,
            discovered_on,
            artist_playlists,
            artist_pick,
        },
        top_playcounts,
    })
}

fn accept_artist_overview_attempt(
    last_error: &mut String,
    attempt: Result<Vec<u8>, String>,
) -> Option<ArtistOverviewQuery> {
    match attempt.and_then(|payload| parse_artist_overview_payload(&payload)) {
        Ok(overview) => Some(overview),
        Err(error) => {
            *last_error = error;
            None
        }
    }
}

async fn fetch_artist_overview(
    session: &Session,
    artist_uri: &str,
) -> Result<ArtistOverviewQuery, String> {
    let mut last_error = String::new();
    for schema in ARTIST_OVERVIEW_SCHEMAS {
        let body = artist_overview_body(artist_uri, *schema);
        let attempt = pathfinder_post(session, &body, "artist overview")
            .await
            .map_err(|error| format!("{}: {error}", schema.hash));
        if let Some(overview) = accept_artist_overview_attempt(&mut last_error, attempt) {
            return Ok(overview);
        }
    }
    Err(format!(
        "all {} artist overview documents failed ({last_error})",
        ARTIST_OVERVIEW_SCHEMAS.len()
    ))
}

async fn cached_artist_overview(
    session: &Session,
    artist_id: &str,
    artist_uri: &str,
) -> Option<ArtistOverviewQuery> {
    let key = ArtistOverviewCacheKey {
        artist_id: artist_id.to_owned(),
        schema_hash: ARTIST_OVERVIEW_SCHEMAS[0].hash,
    };
    let now = Instant::now();
    if let Some(cached) = ARTIST_OVERVIEW_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key, now)
    {
        return cached;
    }

    let fetched = match fetch_artist_overview(session, artist_uri).await {
        Ok(overview) => Some(overview),
        Err(error) => {
            eprintln!("artist overview unavailable: {error}");
            None
        }
    };
    ARTIST_OVERVIEW_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, fetched.clone(), Instant::now());
    fetched
}

/// Persisted documents used by Spotify 1.2.96.518 for the play-count-bearing
/// album and track pages. Like search and credits, these are client artefacts:
/// a future rotation produces no counts (and one diagnostic) rather than
/// inventing or silently retaining stale values.
const ALBUM_TRACKS_QUERY_HASH: &str =
    "b9bfabef66ed756e5e13f68a942deb60bd4125ec1f1be8cc42769dc0259b4b10";
const GET_TRACK_QUERY_HASH: &str =
    "1a2f0cce77c90a4a5b1730beecc4da7e34290d684324c16663bf09a268ebce48";
const ALBUM_TRACKS_PAGE_SIZE: usize = 50;

#[derive(Default, Deserialize)]
struct AlbumCountsResponse {
    #[serde(default)]
    data: Option<AlbumCountsData>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct AlbumCountsData {
    #[serde(default)]
    albumUnion: Option<AlbumCountsUnion>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct AlbumCountsUnion {
    #[serde(default)]
    tracksV2: Option<PlaycountItems>,
}

#[derive(Default, Deserialize)]
struct PlaycountItems {
    #[serde(default)]
    items: Option<Vec<PlaycountItem>>,
}

#[derive(Default, Deserialize)]
struct PlaycountItem {
    #[serde(default)]
    track: Option<PlaycountTrack>,
}

#[derive(Default, Deserialize)]
struct PlaycountTrack {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    playcount: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct GetTrackCountsResponse {
    #[serde(default)]
    data: Option<GetTrackCountsData>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct GetTrackCountsData {
    #[serde(default)]
    trackUnion: Option<GetTrackCountsUnion>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct GetTrackCountsUnion {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    playcount: Option<serde_json::Value>,
    #[serde(default)]
    firstArtist: Option<ArtistItems>,
}

#[derive(Default, Deserialize)]
struct ArtistItems {
    #[serde(default)]
    items: Option<Vec<ArtistDiscographyItem>>,
}

#[derive(Default, Deserialize)]
struct ArtistDiscographyItem {
    #[serde(default)]
    discography: Option<ArtistDiscography>,
}

#[derive(Default, Deserialize)]
#[allow(non_snake_case)]
struct ArtistDiscography {
    #[serde(default)]
    topTracks: Option<PlaycountItems>,
}

fn playcount_value(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    })
}

fn collect_playcount_items(items: &PlaycountItems, counts: &mut HashMap<String, u64>) {
    for track in items
        .items
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.track.as_ref())
    {
        let Some(uri) = track.uri.as_ref().filter(|uri| !uri.is_empty()) else {
            continue;
        };
        let Some(count) = playcount_value(track.playcount.as_ref()).filter(|count| *count > 0)
        else {
            continue;
        };
        counts.insert(uri.clone(), count);
    }
}

fn apply_playcounts(tracks: &mut [TrackRef], counts: &HashMap<String, u64>) {
    for track in tracks {
        track.play_count = counts.get(&track.uri).copied();
    }
}

fn album_tracks_body(album_uri: &str, offset: usize, limit: usize) -> String {
    serde_json::json!({
        "extensions": {
            "persistedQuery": { "sha256Hash": ALBUM_TRACKS_QUERY_HASH, "version": 1 },
        },
        "operationName": "queryAlbumTracks",
        "variables": { "uri": album_uri, "offset": offset, "limit": limit },
    })
    .to_string()
}

async fn album_playcounts(
    session: &Session,
    album_uri: &str,
    track_count: usize,
) -> Result<HashMap<String, u64>, String> {
    if track_count == 0 {
        return Ok(HashMap::new());
    }
    let mut counts = HashMap::new();
    let mut offset = 0;
    while offset < track_count {
        let limit = (track_count - offset).min(ALBUM_TRACKS_PAGE_SIZE);
        let body = album_tracks_body(album_uri, offset, limit);
        let payload = pathfinder_post(session, &body, "album play counts").await?;
        let parsed: AlbumCountsResponse = serde_json::from_slice(&payload)
            .map_err(|error| format!("unparseable album play-count response: {error}"))?;
        if let Some(items) = parsed
            .data
            .as_ref()
            .and_then(|data| data.albumUnion.as_ref())
            .and_then(|album| album.tracksV2.as_ref())
        {
            collect_playcount_items(items, &mut counts);
        }
        offset += limit;
    }
    if counts.is_empty() {
        return Err("Spotify returned no play counts for this album".to_owned());
    }
    Ok(counts)
}

fn get_track_body(track_uri: &str) -> String {
    serde_json::json!({
        "extensions": {
            "persistedQuery": { "sha256Hash": GET_TRACK_QUERY_HASH, "version": 1 },
        },
        "operationName": "getTrack",
        "variables": { "uri": track_uri, "includeVideoAssociationItems": false },
    })
    .to_string()
}

async fn artist_top_playcounts(
    session: &Session,
    seed_track_uri: &str,
) -> Result<HashMap<String, u64>, String> {
    let payload = pathfinder_post(
        session,
        &get_track_body(seed_track_uri),
        "artist play counts",
    )
    .await?;
    let parsed: GetTrackCountsResponse = serde_json::from_slice(&payload)
        .map_err(|error| format!("unparseable artist play-count response: {error}"))?;
    let mut counts = HashMap::new();
    let track = parsed
        .data
        .as_ref()
        .and_then(|data| data.trackUnion.as_ref())
        .ok_or_else(|| "Spotify returned no track data for this artist".to_owned())?;
    if let (Some(uri), Some(count)) = (
        track.uri.as_ref().filter(|uri| !uri.is_empty()),
        playcount_value(track.playcount.as_ref()).filter(|count| *count > 0),
    ) {
        counts.insert(uri.clone(), count);
    }
    if let Some(items) = track
        .firstArtist
        .as_ref()
        .and_then(|artist| artist.items.as_deref())
    {
        for top_tracks in items
            .iter()
            .filter_map(|item| item.discography.as_ref())
            .filter_map(|discography| discography.topTracks.as_ref())
        {
            collect_playcount_items(top_tracks, &mut counts);
        }
    }
    if counts.is_empty() {
        return Err("Spotify returned no play counts for this artist".to_owned());
    }
    Ok(counts)
}

/// Builds the `searchDesktop` request body. The variables mirror what the
/// official client sends; the persisted-query hash selects the document.
fn search_desktop_body(query: &str, limit: usize, hash: &str) -> String {
    serde_json::json!({
        "extensions": { "persistedQuery": { "sha256Hash": hash, "version": 1 } },
        "operationName": "searchDesktop",
        "variables": {
            "searchTerm": query,
            "offset": 0,
            "limit": limit.clamp(1, MAX_SEARCH_LIMIT),
            "numberOfTopResults": 5,
            "includeArtistHasConcertsField": false,
            "includeAudiobooks": false,
            "includeAuthors": false,
            "includePreReleases": false,
        },
    })
    .to_string()
}

/// One pathfinder attempt for a given persisted-query body: POST with the
/// session's login5 bearer + client token, transient failures retried with
/// the bounded backoff. A client error (e.g. a 400 persisted-query
/// rejection) fails fast so the caller can fall back to another hash.
///
/// `what` names the operation in error and retry messages only.
async fn pathfinder_post(session: &Session, body: &str, what: &str) -> Result<Vec<u8>, String> {
    let token = session
        .login5()
        .auth_token()
        .await
        .map_err(|error| format!("{what} authentication failed: {error}"))?;
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(PATHFINDER_ENDPOINT)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            format!("{} {}", token.token_type, token.access_token),
        );
    if let Ok(client_token) = session.spclient().client_token().await {
        builder = builder.header("client-token", client_token.as_str());
    }
    let request = builder
        .body(Bytes::from(body.to_owned()))
        .map_err(|error| format!("{what} request construction failed: {error}"))?;

    let backoffs = backoff_sequence(
        BROWSE_RETRY_ATTEMPTS,
        BROWSE_BACKOFF_BASE_MS,
        BROWSE_BACKOFF_MAX_MS,
    );
    let mut attempt = 0usize;
    loop {
        match session.http_client().request_body(request.clone()).await {
            Ok(bytes) => return Ok(bytes.to_vec()),
            Err(error) => {
                let transient = browse_error_is_transient(error.kind, http_status_of(&error));
                if !transient || attempt >= backoffs.len() {
                    return Err(format!(
                        "{what} request failed after {} attempt(s) (last error: {error})",
                        attempt + 1,
                    ));
                }
                let backoff = backoffs[attempt];
                eprintln!(
                    "{what} request failed ({error}); retrying in {backoff} ms (attempt {}/{})",
                    attempt + 1,
                    backoffs.len() + 1,
                );
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                attempt += 1;
            }
        }
    }
}

/// Search via the official client's pathfinder GraphQL `searchDesktop`
/// operation, authenticated with the session's own login5 bearer + client
/// token (no developer app). Hash rotation is handled by trying the newest
/// known persisted-query hash first and the previous one on rejection; a
/// renamed/missing response field degrades to an empty section, never an
/// error.
pub async fn search_browse(
    session: &Session,
    query: &str,
    limit: usize,
) -> Result<renderer_engine::protocol::SearchBrowse, String> {
    if query.trim().is_empty() {
        return Err("search query must not be empty".to_owned());
    }
    let mut last_error = String::new();
    for hash in SEARCH_DESKTOP_HASHES {
        let body = search_desktop_body(query.trim(), limit, hash);
        let payload = match pathfinder_post(session, &body, "search").await {
            Ok(payload) => payload,
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let parsed: SearchDesktopResponse = serde_json::from_slice(&payload)
            .map_err(|error| format!("unparseable search response: {error}"))?;
        return Ok(renderer_engine::protocol::SearchBrowse {
            top: search_top_ref(query.trim(), &parsed.data.searchV2.topResultsV2),
            tracks: parsed
                .data
                .searchV2
                .tracksV2
                .items
                .iter()
                .map(|wrapper| track_ref_from_hit(&wrapper.item.data))
                .collect(),
            albums: dedupe_by_id(
                parsed
                    .data
                    .searchV2
                    .albumsV2
                    .items
                    .iter()
                    .map(|wrapper| album_ref_from_hit(&wrapper.data))
                    .collect(),
                |album| album.id.as_str(),
            ),
            artists: dedupe_by_id(
                parsed
                    .data
                    .searchV2
                    .artists
                    .items
                    .iter()
                    .map(|wrapper| artist_ref_from_hit(&wrapper.data))
                    .collect(),
                |artist| artist.id.as_str(),
            ),
            playlists: dedupe_by_id(
                parsed
                    .data
                    .searchV2
                    .playlists
                    .as_ref()
                    .and_then(|section| section.items.as_deref())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|wrapper| wrapper.data.as_ref())
                    .map(playlist_ref_from_hit)
                    .collect(),
                |playlist| playlist.id.as_str(),
            ),
        });
    }
    Err(format!(
        "search failed with all {} persisted-query hashes; last error: {last_error}",
        SEARCH_DESKTOP_HASHES.len()
    ))
}

#[cfg(test)]
mod tests {
    use librespot_core::FileId;
    use librespot_metadata::album::AlbumType;
    use librespot_metadata::album::Discs;
    use librespot_metadata::artist::{Artists, Biography};
    use librespot_metadata::image::{Image, ImageSize, PictureSize, PictureSizes};
    use librespot_metadata::playlist::attribute::PlaylistAttributes;
    use librespot_metadata::track::Tracks;

    use super::*;

    use librespot_core::date::Date;

    #[test]
    fn canvas_response_keeps_only_official_video_records_for_the_track() {
        let track_uri = "spotify:track:0123456789ABCDEFGHIJKL";
        let mut record = canvaz::entity_canvaz_response::Canvaz::new();
        record.url = "https://canvaz.scdn.co/upload/artist/video.mp4".to_owned();
        record.entity_uri = track_uri.to_owned();
        record.type_ = EnumOrUnknown::new(canvaz::Type::VIDEO_LOOPING);
        let encoded = record.write_to_bytes().unwrap();
        assert!(encoded.len() < 128, "test fixture keeps a one-byte length");
        let mut response = vec![0x0a, encoded.len() as u8];
        response.extend_from_slice(&encoded);

        let canvas = parse_canvas_response(&response, track_uri)
            .unwrap()
            .expect("video canvas");
        assert_eq!(canvas.url, record.url);
        assert_eq!(canvas.canvas_type, "video_looping");
        assert!(
            parse_canvas_response(&response, "spotify:track:other")
                .unwrap()
                .is_none()
        );
    }

    fn file_id(byte: u8) -> FileId {
        FileId::from_raw(&[byte; 20])
    }

    fn image(size: ImageSize, byte: u8) -> Image {
        Image {
            id: file_id(byte),
            size,
            width: 300,
            height: 300,
        }
    }

    fn test_artist(id: &str, name: &str) -> Artist {
        Artist {
            id: SpotifyUri::from_uri(&format!("spotify:artist:{id}")).unwrap(),
            name: name.to_owned(),
            popularity: 0,
            top_tracks: Default::default(),
            albums: Default::default(),
            singles: Default::default(),
            compilations: Default::default(),
            appears_on_albums: Default::default(),
            external_ids: Default::default(),
            portraits: Default::default(),
            biographies: Default::default(),
            activity_periods: Default::default(),
            restrictions: Default::default(),
            related: Default::default(),
            is_portrait_album_cover: false,
            portrait_group: Default::default(),
            sales_periods: Default::default(),
            availabilities: Default::default(),
        }
    }

    pub(super) fn test_album(id: &str, name: &str, artists: Vec<Artist>, covers: Images) -> Album {
        Album {
            id: SpotifyUri::from_uri(&format!("spotify:album:{id}")).unwrap(),
            name: name.to_owned(),
            artists: Artists(artists),
            album_type: AlbumType::ALBUM,
            label: String::new(),
            date: Date::from_timestamp_ms(0).unwrap(),
            popularity: 0,
            covers,
            external_ids: Default::default(),
            discs: Discs::default(),
            reviews: Vec::new(),
            copyrights: Default::default(),
            restrictions: Default::default(),
            related: Default::default(),
            sale_periods: Default::default(),
            cover_group: Default::default(),
            original_title: String::new(),
            version_title: String::new(),
            type_str: String::new(),
            availability: Default::default(),
        }
    }

    pub(super) fn test_track(
        id: &str,
        name: &str,
        duration: i32,
        album: Album,
        artists: Vec<Artist>,
    ) -> Track {
        Track {
            id: SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap(),
            name: name.to_owned(),
            album,
            artists: Artists(artists),
            number: 1,
            disc_number: 1,
            duration,
            popularity: 0,
            is_explicit: false,
            external_ids: Default::default(),
            restrictions: Default::default(),
            files: Default::default(),
            alternatives: Tracks::default(),
            sale_periods: Default::default(),
            previews: Default::default(),
            tags: Vec::new(),
            earliest_live_timestamp: Date::from_timestamp_ms(0).unwrap(),
            has_lyrics: false,
            availability: Default::default(),
            licensor: Default::default(),
            language_of_performance: Vec::new(),
            content_ratings: Default::default(),
            original_title: String::new(),
            version_title: String::new(),
            artists_with_role: Default::default(),
        }
    }

    #[test]
    fn cover_url_prefers_default_then_first_image() {
        let images = Images(vec![
            image(ImageSize::SMALL, 0x11),
            image(ImageSize::DEFAULT, 0x22),
        ]);
        let url = cover_url(&images).unwrap();
        assert_eq!(url.len(), COVER_BASE.len() + 40);
        assert!(url.starts_with(COVER_BASE));
        assert!(url.ends_with(&"22".repeat(20)));

        let without_default = Images(vec![image(ImageSize::LARGE, 0x33)]);
        assert!(
            cover_url(&without_default)
                .unwrap()
                .ends_with(&"33".repeat(20))
        );
        assert_eq!(cover_url(&Images::default()), None);
    }

    #[test]
    fn track_ref_conversion_uses_the_play_queue_shape() {
        let artist = test_artist("0123456789ABCDEFGHIJKL", "Test Artist");
        let album = test_album(
            "0abcdefghijklmnopqrstu",
            "Test Album",
            vec![artist],
            Images(vec![image(ImageSize::DEFAULT, 0x5a)]),
        );
        let track = test_track(
            "2abcdefghijklmnopqrstu",
            "Test Track",
            123_456,
            album,
            vec![test_artist("0123456789ABCDEFGHIJKL", "Test Artist")],
        );
        let converted = track_ref(&track);
        assert_eq!(converted.id, "2abcdefghijklmnopqrstu");
        assert_eq!(converted.uri, "spotify:track:2abcdefghijklmnopqrstu");
        assert_eq!(converted.name, "Test Track");
        assert_eq!(converted.artist_names, vec!["Test Artist".to_owned()]);
        assert_eq!(converted.artist_id, "0123456789ABCDEFGHIJKL");
        assert_eq!(converted.album_id, "0abcdefghijklmnopqrstu");
        assert_eq!(converted.album_name, "Test Album");
        assert!(converted.cover_url.starts_with(COVER_BASE));
        assert_eq!(converted.duration_ms, 123_456);
    }

    #[test]
    fn permanent_restrictions_are_classified_without_transient_failures() {
        let policy = AvailabilityPolicy {
            country: "US".to_owned(),
            catalogue: "premium".to_owned(),
            filter_explicit: true,
        };
        let album = test_album(
            "0abcdefghijklmnopqrstu",
            "Album",
            Vec::new(),
            Images::default(),
        );
        let mut track = test_track(
            "2abcdefghijklmnopqrstu",
            "Track",
            123_456,
            album,
            Vec::new(),
        );

        assert_eq!(
            permanent_unavailability(&track, &policy).as_deref(),
            Some("no playable audio file")
        );
        track.alternatives = Tracks(vec![
            SpotifyUri::from_uri("spotify:track:3abcdefghijklmnopqrstu").unwrap(),
        ]);
        assert_eq!(
            permanent_unavailability(&track, &policy),
            None,
            "an alternative lets librespot resolve a playable recording"
        );
        track.is_explicit = true;
        assert_eq!(
            permanent_unavailability(&track, &policy).as_deref(),
            Some("explicit content is filtered")
        );
        track.is_explicit = false;
        track.duration = 0;
        assert_eq!(
            permanent_unavailability(&track, &policy).as_deref(),
            Some("invalid track duration")
        );
    }

    #[test]
    fn catalogue_country_and_embargo_rules_are_stable_unavailability() {
        let policy = AvailabilityPolicy {
            country: "US".to_owned(),
            catalogue: "premium".to_owned(),
            filter_explicit: false,
        };
        let album = test_album(
            "0abcdefghijklmnopqrstu",
            "Album",
            Vec::new(),
            Images::default(),
        );
        let mut track = test_track(
            "2abcdefghijklmnopqrstu",
            "Track",
            123_456,
            album,
            Vec::new(),
        );
        track.alternatives = Tracks(vec![
            SpotifyUri::from_uri("spotify:track:3abcdefghijklmnopqrstu").unwrap(),
        ]);
        track.restrictions = librespot_metadata::restriction::Restrictions(vec![
            librespot_metadata::restriction::Restriction {
                catalogues: librespot_metadata::restriction::RestrictionCatalogues(Vec::new()),
                restriction_type: Default::default(),
                catalogue_strs: vec!["premium".to_owned()],
                countries_allowed: Some(vec!["GB".to_owned()]),
                countries_forbidden: None,
            },
        ]);
        assert_eq!(
            permanent_unavailability(&track, &policy).as_deref(),
            Some("not available in your country")
        );

        track.restrictions = Default::default();
        track.earliest_live_timestamp =
            Date::from_timestamp_ms(Date::now_utc().as_timestamp_ms() + 60_000).unwrap();
        assert_eq!(
            permanent_unavailability(&track, &policy).as_deref(),
            Some("not available until its release date")
        );
    }

    #[test]
    fn album_ref_converts_ids_and_names() {
        let album = test_album(
            "0abcdefghijklmnopqrstu",
            "Test Album",
            vec![test_artist("0123456789ABCDEFGHIJKL", "Test Artist")],
            Images::default(),
        );
        let converted = album_ref(&album);
        assert_eq!(converted.id, "0abcdefghijklmnopqrstu");
        assert_eq!(converted.uri, "spotify:album:0abcdefghijklmnopqrstu");
        assert_eq!(converted.name, "Test Album");
        assert_eq!(converted.artist_names, vec!["Test Artist".to_owned()]);
        assert_eq!(converted.cover_url, None);
    }

    #[test]
    fn rootlist_json_maps_playlists_from_parallel_arrays() {
        let body = r#"{
            "length": 2,
            "revision": "AAEC",
            "contents": {
                "pos": 0,
                "truncated": false,
                "items": [
                    {"uri": "spotify:user:alice:playlist:0123456789ABCDEFGHIJKL"},
                    {"uri": "spotify:playlist:1abcdefghijklmnopqrstu"}
                ],
                "metaItems": [
                    {
                        "length": 42,
                        "ownerUsername": "alice",
                        "attributes": {
                            "name": "Road Trip",
                            "picture": "EREREREREREREREREREREQ=="
                        }
                    },
                    {
                        "length": 7,
                        "ownerUsername": "bob",
                        "attributes": {
                            "name": "Focus",
                            "pictureSize": [{"target_name": "any", "url": "https://mosaic.scdn.co/640/abc"}]
                        }
                    }
                ]
            }
        }"#;
        let parsed: RootlistJson = serde_json::from_str(body).unwrap();
        let contents = parsed.contents.unwrap();
        let refs: Vec<PlaylistRef> = contents
            .items
            .iter()
            .zip(contents.meta_items.iter())
            .filter_map(|(item, meta)| playlist_ref_from_rootlist(item, meta))
            .collect();

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "0123456789ABCDEFGHIJKL");
        assert_eq!(
            refs[0].uri,
            "spotify:user:alice:playlist:0123456789ABCDEFGHIJKL"
        );
        // Rootlist attributes carry name/picture/length only; description is
        // intentionally absent until a playlist metadata/search payload.
        assert_eq!(refs[0].description, None);
        assert_eq!(refs[0].name, "Road Trip");
        assert_eq!(refs[0].owner_id, "alice");
        assert_eq!(refs[0].owner_name, "");
        assert_eq!(refs[0].track_count, Some(42));
        // The base64 picture decodes to a 16-byte file id rendered as hex.
        assert!(
            refs[0]
                .cover_url
                .as_deref()
                .unwrap()
                .starts_with(COVER_BASE)
        );
        assert_eq!(
            refs[0].cover_url.as_deref().unwrap().len(),
            COVER_BASE.len() + 32
        );

        assert_eq!(refs[1].id, "1abcdefghijklmnopqrstu");
        assert_eq!(refs[1].owner_id, "bob");
        assert_eq!(refs[1].track_count, Some(7));
        assert_eq!(
            refs[1].cover_url.as_deref(),
            Some("https://mosaic.scdn.co/640/abc")
        );
    }

    #[test]
    fn rootlist_empty_owner_username_falls_back_to_uri_owner() {
        let item = RootlistItemJson {
            uri: Some("spotify:user:alice:playlist:0123456789ABCDEFGHIJKL".to_owned()),
        };
        let meta = RootlistMetaItemJson {
            owner_username: Some(String::new()),
            ..RootlistMetaItemJson::default()
        };
        let reference = playlist_ref_from_rootlist(&item, &meta).unwrap();
        assert_eq!(reference.owner_id, "alice");
    }

    #[test]
    fn rootlist_cover_prefers_the_picture_file_id_over_picture_size_urls() {
        // Both present: the raw picture (base64 file id -> i.scdn.co hex)
        // wins; a pictureSize URL is only the fallback.
        let both = RootlistAttributesJson {
            name: None,
            picture: Some("EREREREREREREREREREREQ==".to_owned()),
            picture_size: vec![PictureSizeJson {
                url: Some("https://mosaic.scdn.co/640/abc".to_owned()),
            }],
        };
        let cover = rootlist_cover(&both).unwrap();
        assert!(cover.starts_with(COVER_BASE));
        assert_eq!(
            cover.len(),
            COVER_BASE.len() + 32,
            "16 decoded bytes -> 32 hex chars"
        );

        // No picture: the ready-made URL is used.
        let url_only = RootlistAttributesJson {
            name: None,
            picture: None,
            picture_size: vec![PictureSizeJson {
                url: Some(
                    "https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567".to_owned(),
                ),
            }],
        };
        assert_eq!(
            rootlist_cover(&url_only).as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567")
        );

        // Undecodable or empty picture data degrades to the fallback / None.
        let garbage = RootlistAttributesJson {
            name: None,
            picture: Some("!!!not-base64!!!".to_owned()),
            picture_size: vec![PictureSizeJson {
                url: Some("https://mosaic.scdn.co/640/abc".to_owned()),
            }],
        };
        assert_eq!(
            rootlist_cover(&garbage).as_deref(),
            Some("https://mosaic.scdn.co/640/abc")
        );
        let empty = RootlistAttributesJson::default();
        assert_eq!(rootlist_cover(&empty), None);

        // A URL-safe/unpadded spelling of the same file id still resolves
        // (server variants must not drop every cover).
        let url_safe = RootlistAttributesJson {
            name: None,
            picture: Some("EREREREREREREREREREREQ".to_owned()),
            picture_size: Vec::new(),
        };
        let cover = rootlist_cover(&url_safe).unwrap();
        assert!(cover.starts_with(COVER_BASE));
        assert_eq!(
            cover.len(),
            COVER_BASE.len() + 32,
            "16 decoded bytes -> 32 hex chars"
        );
    }

    #[test]
    fn rootlist_skips_non_playlist_rows_and_missing_meta() {
        let body = r#"{
            "contents": {
                "items": [
                    {"uri": "spotify:folder:0123456789ABCDEFGHIJKL"},
                    {"uri": "spotify:user:alice:playlist:1abcdefghijklmnopqrstu"}
                ],
                "metaItems": []
            }
        }"#;
        let parsed: RootlistJson = serde_json::from_str(body).unwrap();
        let contents = parsed.contents.unwrap();
        let refs: Vec<PlaylistRef> = contents
            .items
            .iter()
            .zip(contents.meta_items.iter())
            .filter_map(|(item, meta)| playlist_ref_from_rootlist(item, meta))
            .collect();
        assert!(refs.is_empty());
    }

    /// Spotify ranks `topResultsV2` by popularity, so "taylor" puts the track
    /// "Style" above the artist Taylor Swift — the whole catalogue outranks the
    /// person who made it. Someone typing an artist's name is asking for the
    /// artist, so the name match decides and their order only breaks ties.
    #[test]
    fn search_top_result_prefers_what_the_query_actually_names() {
        let body = r#"{
            "itemsV2": [
                {"item": {"__typename": "TrackResponseWrapper", "data": {
                    "__typename": "Track",
                    "uri": "spotify:track:1abcdefghijklmnopqrstu",
                    "name": "Style"
                }}},
                {"item": {"__typename": "ArtistResponseWrapper", "data": {
                    "__typename": "Artist",
                    "uri": "spotify:artist:2abcdefghijklmnopqrstu",
                    "profile": {"name": "Taylor Swift"}
                }}}
            ]
        }"#;
        let section: SearchTopSectionJson = serde_json::from_str(body).unwrap();

        match search_top_ref("taylor", &section).expect("a top result") {
            renderer_engine::protocol::SearchTopRef::Artist(artist) => {
                assert_eq!(artist.name, "Taylor Swift");
            }
            other => panic!("expected the artist the query names, got {other:?}"),
        }

        // The same response, asked a different question: now the track is what
        // was named, and Spotify's own leader is also the right answer.
        match search_top_ref("style", &section).expect("a top result") {
            renderer_engine::protocol::SearchTopRef::Track(track) => {
                assert_eq!(track.name, "Style");
            }
            other => panic!("expected the track, got {other:?}"),
        }

        // Nothing matches the words typed, so their ranking stands unaltered.
        match search_top_ref("zzzz", &section).expect("a top result") {
            renderer_engine::protocol::SearchTopRef::Track(track) => {
                assert_eq!(track.name, "Style");
            }
            other => panic!("expected Spotify's own leader, got {other:?}"),
        }
    }

    #[test]
    fn top_match_rank_orders_exact_over_prefix_over_word_over_nothing() {
        assert_eq!(top_match_rank("kygo", "Kygo"), 3);
        assert_eq!(top_match_rank("taylor", "Taylor Swift"), 2);
        assert_eq!(top_match_rank("rhapsody", "Bohemian Rhapsody"), 1);
        assert_eq!(top_match_rank("psod", "Bohemian Rhapsody"), 0);
        assert_eq!(top_match_rank("", "Kygo"), 0);
        assert_eq!(top_match_rank("kygo", ""), 0);
    }

    /// The whole point of reading `topResultsV2` rather than "first artist":
    /// a query whose best answer is a playlist must come back as that playlist,
    /// even when the response also carries artists. Verified against a live
    /// `top 50` response, which ranks "Top 50 – Deutschland" first and an
    /// unrelated artist somewhere in the artists section.
    #[test]
    fn search_desktop_top_result_takes_the_cross_kind_ranking() {
        let body = r#"{
            "data": {
                "searchV2": {
                    "topResultsV2": {
                        "itemsV2": [{
                            "item": {
                                "__typename": "PlaylistResponseWrapper",
                                "data": {
                                    "__typename": "Playlist",
                                    "uri": "spotify:playlist:1abcdefghijklmnopqrstu",
                                    "name": "Top 50 - Deutschland",
                                    "ownerV2": {"data": {"name": "Spotify", "username": "spotify"}}
                                }
                            }
                        }]
                    },
                    "artists": {"items": [{"data": {
                        "__typename": "Artist",
                        "uri": "spotify:artist:0123456789ABCDEFGHIJKL",
                        "profile": {"name": "Some Artist"}
                    }}]}
                }
            }
        }"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        let top = search_top_ref("top 50", &parsed.data.searchV2.topResultsV2)
            .expect("a top result");
        match top {
            renderer_engine::protocol::SearchTopRef::Playlist(playlist) => {
                assert_eq!(playlist.name, "Top 50 - Deutschland");
                assert_eq!(playlist.id, "1abcdefghijklmnopqrstu");
            }
            other => panic!("expected the ranked playlist, got {other:?}"),
        }
    }

    /// Podcasts and users outrank playable things often enough that taking
    /// `[0]` blindly would leave the panel empty. The walk skips what the app
    /// cannot open and keeps going.
    #[test]
    fn search_desktop_top_result_skips_kinds_with_nowhere_to_go() {
        let body = r#"{
            "data": {
                "searchV2": {
                    "topResultsV2": {
                        "itemsV2": [
                            {"item": {"data": {"__typename": "Podcast", "uri": "spotify:show:2abcdefghijklmnopqrstu", "name": "A Show"}}},
                            {"item": {"data": {"__typename": "Artist", "uri": "spotify:artist:0123456789ABCDEFGHIJKL", "profile": {"name": "Real Answer"}}}}
                        ]
                    }
                }
            }
        }"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        let top = search_top_ref("real answer", &parsed.data.searchV2.topResultsV2)
            .expect("a top result");
        match top {
            renderer_engine::protocol::SearchTopRef::Artist(artist) => {
                assert_eq!(artist.name, "Real Answer");
            }
            other => panic!("expected the artist under the podcast, got {other:?}"),
        }
    }

    /// A response with the section renamed or absent degrades to no top
    /// result, never an error: the groups below it are still a useful page.
    #[test]
    fn search_desktop_top_result_absent_is_not_an_error() {
        let parsed: SearchDesktopResponse =
            serde_json::from_str(r#"{"data": {"searchV2": {}}}"#).unwrap();
        assert!(search_top_ref("anything", &parsed.data.searchV2.topResultsV2).is_none());
    }

    #[test]
    fn search_desktop_json_maps_tracks_albums_and_artists() {
        let body = r#"{
            "data": {
                "searchV2": {
                    "tracksV2": {
                        "totalCount": 1,
                        "items": [{
                            "item": {
                                "data": {
                                    "__typename": "Track",
                                    "uri": "spotify:track:2abcdefghijklmnopqrstu",
                                    "name": "Search Track",
                                    "artists": {"items": [{"uri": "spotify:artist:0123456789ABCDEFGHIJKL", "profile": {"name": "Search Artist"}}]},
                                    "albumOfTrack": {
                                        "uri": "spotify:album:0abcdefghijklmnopqrstu",
                                        "name": "Search Album",
                                        "coverArt": {"sources": [
                                            {"height": 64, "url": "https://i.scdn.co/image/ab67616d000048510123456789abcdef01234567", "width": 64},
                                            {"height": 300, "url": "https://i.scdn.co/image/ab67616d00001e020123456789abcdef01234567", "width": 300}
                                        ]}
                                    },
                                    "duration": {"totalMilliseconds": 211000}
                                }
                            }
                        }]
                    },
                    "albumsV2": {
                        "items": [{"data": {
                            "uri": "spotify:album:1abcdefghijklmnopqrstu",
                            "name": "Album Hit",
                            "artists": {"items": [{"profile": {"name": "Album Artist"}}]},
                            "coverArt": {"sources": [{"url": "https://i.scdn.co/image/ab67616d000048511abcdefghijklmnopqrstu", "width": 64}]},
                            "date": {"year": 2014}
                        }}]
                    },
                    "artists": {
                        "items": [{"data": {
                            "uri": "spotify:artist:3abcdefghijklmnopqrstu",
                            "profile": {"name": "Artist Hit"},
                            "visuals": {"avatarImage": {"sources": [
                                {"height": 640, "url": "https://i.scdn.co/image/ab6761610000e5eb3abcdefghijklmnopqrstu", "width": 640},
                                {"height": 160, "url": "https://i.scdn.co/image/ab6761610000f1783abcdefghijklmnopqrstu", "width": 160},
                                {"height": 320, "url": "https://i.scdn.co/image/ab676161000051743abcdefghijklmnopqrstu", "width": 320}
                            ]}}
                        }}]
                    }
                }
            }
        }"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        let converted = renderer_engine::protocol::SearchBrowse {
            top: search_top_ref("anything", &parsed.data.searchV2.topResultsV2),
            tracks: parsed
                .data
                .searchV2
                .tracksV2
                .items
                .iter()
                .map(|wrapper| track_ref_from_hit(&wrapper.item.data))
                .collect(),
            albums: parsed
                .data
                .searchV2
                .albumsV2
                .items
                .iter()
                .map(|wrapper| album_ref_from_hit(&wrapper.data))
                .collect(),
            artists: parsed
                .data
                .searchV2
                .artists
                .items
                .iter()
                .map(|wrapper| artist_ref_from_hit(&wrapper.data))
                .collect(),
            playlists: Vec::new(),
        };

        assert_eq!(converted.tracks.len(), 1);
        let track = &converted.tracks[0];
        assert_eq!(track.id, "2abcdefghijklmnopqrstu");
        assert_eq!(track.uri, "spotify:track:2abcdefghijklmnopqrstu");
        assert_eq!(track.name, "Search Track");
        assert_eq!(track.artist_names, vec!["Search Artist".to_owned()]);
        assert_eq!(track.artist_id, "0123456789ABCDEFGHIJKL");
        assert_eq!(track.album_id, "0abcdefghijklmnopqrstu");
        assert_eq!(track.album_name, "Search Album");
        assert_eq!(
            track.cover_url, "https://i.scdn.co/image/ab67616d00001e020123456789abcdef01234567",
            "the ≥300px source wins over the 64px one"
        );
        assert_eq!(track.duration_ms, 211_000);

        assert_eq!(converted.albums.len(), 1);
        assert_eq!(converted.albums[0].id, "1abcdefghijklmnopqrstu");
        assert_eq!(
            converted.albums[0].artist_names,
            vec!["Album Artist".to_owned()]
        );
        assert_eq!(
            converted.albums[0].cover_url.as_deref(),
            Some("https://i.scdn.co/image/ab67616d000048511abcdefghijklmnopqrstu"),
            "the only source is used when no ≥300px source exists"
        );
        assert_eq!(
            converted.albums[0].year,
            Some(2014),
            "the search response carries the release year for free"
        );
        // A hit with no date at all reports no year rather than 0.
        let undated: SearchAlbumHitJson =
            serde_json::from_str(r#"{"uri": "spotify:album:9abcdefghijklmnopqrstu"}"#).unwrap();
        assert_eq!(album_ref_from_hit(&undated).year, None);
        let zeroed: SearchAlbumHitJson = serde_json::from_str(
            r#"{"uri": "spotify:album:9abcdefghijklmnopqrstu", "date": {"year": 0}}"#,
        )
        .unwrap();
        assert_eq!(album_ref_from_hit(&zeroed).year, None);

        assert_eq!(converted.artists.len(), 1);
        assert_eq!(converted.artists[0].id, "3abcdefghijklmnopqrstu");
        assert_eq!(converted.artists[0].name, "Artist Hit");
        assert_eq!(
            converted.artists[0].portrait_url.as_deref(),
            Some("https://i.scdn.co/image/ab676161000051743abcdefghijklmnopqrstu"),
            "the smallest ≥300px avatar wins, not the first one the array happens to list"
        );
    }

    #[test]
    fn search_desktop_json_maps_playlist_metadata_without_extra_fetches() {
        let body = r#"{
            "data": {
                "searchV2": {
                    "playlists": {
                        "items": [
                            {"data": {
                                "uri": "spotify:playlist:0123456789ABCDEFGHIJKL",
                                "name": "Public Mix",
                                "description": "<p>The essential <b>tracks</b>, all in <a href='https://example.invalid'>one playlist</a>.</p>",
                                "ownerV2": {"data": {
                                    "name": "Alice Example",
                                    "username": "alice",
                                    "uri": "spotify:user:alice"
                                }},
                                "images": {"items": [{"sources": [
                                    {"url": "https://i.scdn.co/image/small", "width": 64},
                                    {"url": "https://i.scdn.co/image/cover", "width": 300}
                                ]}]},
                                "content": {"totalCount": "42"}
                            }},
                            {"data": {
                                "uri": "spotify:playlist:1abcdefghijklmnopqrstu",
                                "name": "Owner URI fallback",
                                "ownerV2": {"data": {"uri": "spotify:user:bob"}}
                            }}
                        ]
                    }
                }
            }
        }"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        let playlists: Vec<PlaylistRef> = parsed
            .data
            .searchV2
            .playlists
            .as_ref()
            .and_then(|section| section.items.as_deref())
            .unwrap_or_default()
            .iter()
            .filter_map(|wrapper| wrapper.data.as_ref())
            .map(playlist_ref_from_hit)
            .collect();

        assert_eq!(playlists.len(), 2);
        assert_eq!(playlists[0].id, "0123456789ABCDEFGHIJKL");
        assert_eq!(playlists[0].name, "Public Mix");
        assert_eq!(
            playlists[0].description.as_deref(),
            Some("The essential tracks, all in one playlist.")
        );
        assert_eq!(playlists[0].owner_id, "alice");
        assert_eq!(playlists[0].owner_name, "Alice Example");
        assert_eq!(playlists[0].track_count, Some(42));
        assert_eq!(
            playlists[0].cover_url.as_deref(),
            Some("https://i.scdn.co/image/cover")
        );
        assert_eq!(playlists[1].owner_id, "bob");
        assert_eq!(playlists[1].track_count, None);

        // Missing/null sections, nullable item lists, and unavailable union
        // entries are all an empty section rather than a failed search.
        for body in [
            r#"{"data":{"searchV2":{}}}"#,
            r#"{"data":{"searchV2":{"playlists":null}}}"#,
            r#"{"data":{"searchV2":{"playlists":{"items":null}}}}"#,
            r#"{"data":{"searchV2":{"playlists":{"items":[{"data":null}]}}}}"#,
        ] {
            let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
            let playlists: Vec<PlaylistRef> = parsed
                .data
                .searchV2
                .playlists
                .as_ref()
                .and_then(|section| section.items.as_deref())
                .unwrap_or_default()
                .iter()
                .filter_map(|wrapper| wrapper.data.as_ref())
                .map(playlist_ref_from_hit)
                .collect();
            assert!(
                playlists.is_empty(),
                "variant playlist sections must not fail or add blank cards"
            );
        }
    }

    #[test]
    fn songwriter_title_normalization_is_unicode_whitespace_and_case_aware() {
        assert_eq!(
            normalize_playlist_title(" \u{2003}Written\u{00a0}BY  BÉYONCÉ\u{202f} "),
            "written by béyoncé"
        );
        assert_eq!(
            songwriter_playlist_title("  BÉYONCÉ  ").as_deref(),
            Some("Written by BÉYONCÉ")
        );
    }

    #[test]
    fn songwriter_candidates_fail_closed_on_owner_uri_and_ambiguity() {
        let reference = |uri: &str, name: &str, owner_id: &str| PlaylistRef {
            id: String::new(),
            uri: uri.to_owned(),
            name: name.to_owned(),
            description: None,
            owner_id: owner_id.to_owned(),
            owner_name: String::new(),
            cover_url: None,
            track_count: None,
        };
        let official = reference(
            "spotify:user:spotify:playlist:0123456789ABCDEFGHIJKL",
            " Written\u{00a0}by  BEYONCÉ ",
            "spotify",
        );
        let selected = official_songwriter_candidate(&[official.clone()], "Beyoncé")
            .unwrap()
            .expect("the exact official candidate");
        assert_eq!(selected.id, "0123456789ABCDEFGHIJKL");
        assert_eq!(
            selected.uri,
            "spotify:user:spotify:playlist:0123456789ABCDEFGHIJKL"
        );

        // Duplicate search rows for one playlist are one candidate, not an
        // ambiguity.
        let deduped =
            official_songwriter_candidate(&[official.clone(), official.clone()], "Beyoncé")
                .unwrap()
                .expect("duplicate ids are deduplicated");
        assert_eq!(deduped.id, selected.id);

        let wrong_owner = reference(
            "spotify:user:alice:playlist:0123456789ABCDEFGHIJKL",
            "Written by Beyoncé",
            "alice",
        );
        let malformed = reference(
            "https://open.spotify.com/playlist/not-a-uri",
            "Written by Beyoncé",
            "spotify",
        );
        let wrong_kind = reference(
            "spotify:album:0123456789ABCDEFGHIJKL",
            "Written by Beyoncé",
            "spotify",
        );
        assert!(
            official_songwriter_candidate(&[wrong_owner, malformed, wrong_kind], "Beyoncé")
                .unwrap()
                .is_none(),
            "owner, malformed URI, and non-playlist rows are rejected before metadata"
        );

        let second = reference(
            "spotify:user:spotify:playlist:1123456789ABCDEFGHIJKL",
            "Written by Beyoncé",
            "spotify",
        );
        let error = official_songwriter_candidate(&[official, second], "Beyoncé").unwrap_err();
        assert!(error.contains("ambiguous"));

        assert_eq!(
            official_playlist_owner(
                &SpotifyUri::from_uri("spotify:user:spotify:playlist:0123456789ABCDEFGHIJKL")
                    .unwrap()
            ),
            Some("spotify")
        );
        assert!(
            official_playlist_owner(
                &SpotifyUri::from_uri("spotify:user:alice:playlist:0123456789ABCDEFGHIJKL")
                    .unwrap()
            )
            .is_none()
        );
        assert!(
            SpotifyUri::from_uri("spotify:user:spotify:playlist:not-an-id").is_err(),
            "malformed playlist ids never reach metadata verification"
        );
    }

    #[test]
    fn songwriter_track_limit_keeps_source_order_and_caps_at_ten() {
        let ids = [
            "2abcdefghijklmnopqrstu",
            "3abcdefghijklmnopqrstu",
            "4abcdefghijklmnopqrstu",
            "5abcdefghijklmnopqrstu",
            "6abcdefghijklmnopqrstu",
            "7abcdefghijklmnopqrstu",
            "0abcdefghijklmnopqrstu",
            "0bcdefghijklmnopqrstuv",
            "0cdefghijklmnopqrstuvw",
            "0defghijklmnopqrstuvwx",
            "0efghijklmnopqrstuvwxy",
        ];
        let items: Vec<(SpotifyUri, Option<i64>)> = ids
            .iter()
            .map(|id| {
                (
                    SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap(),
                    None,
                )
            })
            .collect();
        let resolved: Vec<TrackRef> = items
            .iter()
            .rev()
            .enumerate()
            .map(|(reverse_index, (uri, _))| TrackRef {
                id: id_of(uri),
                uri: uri_of(uri),
                name: format!("Song {}", ids.len() - reverse_index - 1),
                ..TrackRef::default()
            })
            .collect();
        let limited = limited_playlist_items(&items, SONGWRITER_TRACK_LIMIT);
        let tracks = playlist_tracks_from_resolved(&limited, resolved);
        assert_eq!(tracks.len(), SONGWRITER_TRACK_LIMIT);
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Song 0", "Song 1", "Song 2", "Song 3", "Song 4", "Song 5", "Song 6", "Song 7",
                "Song 8", "Song 9",
            ]
        );
    }

    #[test]
    fn songwriter_cache_caches_negatives_expires_them_and_stays_bounded() {
        let reference = PlaylistRef {
            id: "0123456789ABCDEFGHIJKL".to_owned(),
            uri: "spotify:user:spotify:playlist:0123456789ABCDEFGHIJKL".to_owned(),
            name: "Written by Artist".to_owned(),
            description: None,
            owner_id: "spotify".to_owned(),
            owner_name: "Spotify".to_owned(),
            cover_url: None,
            track_count: Some(1),
        };
        let value = SongwriterPlaylist {
            playlist: reference,
            tracks: vec![TrackRef {
                id: "2abcdefghijklmnopqrstu".to_owned(),
                uri: "spotify:track:2abcdefghijklmnopqrstu".to_owned(),
                name: "Song".to_owned(),
                ..TrackRef::default()
            }],
        };
        let now = Instant::now();
        let mut cache = SongwriterPlaylistCache::default();
        cache.insert("positive".to_owned(), Some(value.clone()), now);
        assert_eq!(
            cache
                .get("positive", now)
                .and_then(|entry| entry)
                .map(|entry| entry.tracks.len()),
            Some(1)
        );
        assert!(
            cache
                .get("positive", now + Duration::from_millis(1))
                .is_some(),
            "a fresh success answers from the cache"
        );
        assert!(
            cache
                .get("positive", now + SONGWRITER_PLAYLIST_SUCCESS_TTL)
                .is_none(),
            "successes expire on the overview schedule"
        );
        cache.insert("negative".to_owned(), None, now);
        assert!(
            cache
                .get(
                    "negative",
                    now + SONGWRITER_PLAYLIST_FAILURE_TTL - Duration::from_millis(1)
                )
                .is_some_and(|entry| entry.is_none()),
            "a miss is cached as a negative result"
        );
        assert!(
            cache
                .get("negative", now + SONGWRITER_PLAYLIST_FAILURE_TTL)
                .is_none(),
            "misses share the overview failure lifetime"
        );

        let mut bounded = SongwriterPlaylistCache::default();
        for index in 0..SONGWRITER_PLAYLIST_CACHE_CAPACITY {
            bounded.insert(
                format!("artist-{index}"),
                None,
                now + Duration::from_millis(index as u64),
            );
        }
        bounded.insert(
            "artist-new".to_owned(),
            None,
            now + Duration::from_millis(SONGWRITER_PLAYLIST_CACHE_CAPACITY as u64),
        );
        assert_eq!(bounded.entries.len(), SONGWRITER_PLAYLIST_CACHE_CAPACITY);
        assert!(!bounded.entries.contains_key("artist-0"));
        assert!(bounded.entries.contains_key("artist-new"));
    }

    /// The shared bounded-growth rule must never evict on refresh, and must
    /// always sacrifice the stalest resident rather than an arbitrary one.
    #[test]
    fn eviction_rule_never_touches_a_refresh_and_drops_the_stalest_fresh_key() {
        let now = Instant::now();
        let mut entries: HashMap<String, Instant> = HashMap::new();
        for index in 0..3usize {
            entries.insert(index.to_string(), now + Duration::from_secs(index as u64));
        }

        evict_oldest_for_fresh_key(&mut entries, 3, false, |stamp| *stamp);
        assert_eq!(
            entries.len(),
            3,
            "refreshing an existing key evicts nothing"
        );

        evict_oldest_for_fresh_key(&mut entries, 3, true, |stamp| *stamp);
        assert_eq!(entries.len(), 2);
        assert!(!entries.contains_key("0"), "the stalest resident made room");
        assert!(entries.contains_key("2"), "the freshest resident survived");
    }

    #[test]
    fn track_credits_group_the_flat_contributor_list() {
        // Shaped after the official client's own consumption of
        // `queryTrackCreditsGroupedModal`: one flat list, one role per entry,
        // grouped and deduped on this side.
        let body = r#"{
            "data": {
                "trackUnion": {
                    "__typename": "Track",
                    "name": "Cruel Summer",
                    "creditsTrait": {
                        "contributors": {"items": [
                            {"name": "Taylor Swift", "role": "main artist", "uri": "spotify:artist:06HL4z0CvFAxyc27GXpf02", "roleGroup": {"name": "Performers"}},
                            {"name": "Annie Clark", "role": "composer", "uri": "spotify:artist:2XecPkCBJp99480lrtKlIp", "roleGroup": {"name": "Writers"}},
                            {"name": "Annie Clark", "role": "lyricist", "uri": "spotify:artist:2XecPkCBJp99480lrtKlIp", "url": "https://artists.spotify.com/songwriter/1pTJCipDqvUaFmILQLnMsC", "roleGroup": {"name": "Writers"}},
                            {"name": "Annie Clark", "role": "composer", "uri": "spotify:artist:06HL4z0CvFAxyc27GXpf02", "roleGroup": {"name": "Writers"}},
                            {"name": "Uncredited Ghost", "role": "composer", "roleGroup": {"name": "Writers"}},
                            {"name": "Linked By Reference", "role": "producer", "reference": {"url": "https://example.invalid/ref"}, "roleGroup": {"name": "Producers"}},
                            {"name": "Ungrouped Person", "role": "engineer"},
                            {"role": "nameless", "roleGroup": {"name": "Writers"}}
                        ]},
                        "sources": {"items": [{"name": "Republic Records"}, {"name": "Big Machine"}]}
                    }
                }
            }
        }"#;
        let parsed: CreditsQueryJson = serde_json::from_str(body).unwrap();
        let credits = track_credits_from_query(&parsed, "spotify:track:1BxfuPKGuaTgP7aM0Bbdwr");

        assert_eq!(credits.track_uri, "spotify:track:1BxfuPKGuaTgP7aM0Bbdwr");
        assert_eq!(credits.track_name, "Cruel Summer");
        // Sources is a list here, unlike the single value the retired v0 view
        // returned.
        assert_eq!(credits.source, "Republic Records, Big Machine");

        // Groups appear in first-seen order, and a contributor with no
        // roleGroup still has to land somewhere.
        let titles: Vec<&str> = credits.roles.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["Performers", "Writers", "Producers", "Other"]);

        let writers = &credits.roles[1];
        // The two Annie Clark entries are one person with two roles, not two
        // rows -- this merging is the whole reason the flat list is grouped
        // here rather than rendered as it arrives.
        assert_eq!(writers.artists[0].name, "Annie Clark");
        assert_eq!(writers.artists[0].subroles, vec!["composer", "lyricist"]);
        assert_eq!(writers.artists[0].id, "2XecPkCBJp99480lrtKlIp");
        assert_eq!(
            writers.artists[0].url, "https://artists.spotify.com/songwriter/1pTJCipDqvUaFmILQLnMsC",
            "the songwriter link is taken verbatim from the service, never built"
        );

        // No name is nothing to render.
        assert_eq!(writers.artists.len(), 3);
        assert_eq!(writers.artists[1].name, "Annie Clark");
        assert_eq!(writers.artists[1].id, "06HL4z0CvFAxyc27GXpf02");
        assert_eq!(writers.artists[2].name, "Uncredited Ghost");
        assert_eq!(
            writers.artists[2].url, "",
            "no link when the service sends none"
        );
        assert_eq!(writers.artists[2].id, "");

        // `reference.url` is the documented fallback for the external link.
        assert_eq!(
            credits.roles[2].artists[0].url,
            "https://example.invalid/ref"
        );
    }

    #[test]
    fn credit_ids_are_only_taken_from_valid_artist_uris() {
        // The id is used for nothing but diagnostics now -- links come from the
        // service's own `url` -- but a malformed URI must still never turn into
        // path data.
        assert_eq!(
            credit_artist_id("spotify:artist:2XecPkCBJp99480lrtKlIp"),
            "2XecPkCBJp99480lrtKlIp"
        );
        assert_eq!(
            credit_artist_id("spotify:songwriter:1pTJCipDqvUaFmILQLnMsC"),
            "",
            "a songwriter uri is not an artist id"
        );
        assert_eq!(credit_artist_id("spotify:artist:x"), "");
        assert_eq!(credit_artist_id("spotify:artist:bad/id"), "");
    }

    /// `pinnedItem.itemV2` is a union, and the parser used to keep only its
    /// Playlist arm. Each kind has to survive to the browse payload with its
    /// tag intact, because the card that renders it plays a track and opens
    /// everything else.
    #[test]
    fn artist_pick_maps_every_pinned_item_kind_and_drops_unknown_ones() {
        let pinned = |comment: &str, item: &str| {
            let body = format!(
                r#"{{"data": {{"artistUnion": {{"profile": {{"pinnedItem": {{
                    "comment": {comment}, "itemV2": {{"data": {item}}}
                }}}}}}}}}}"#
            );
            parse_artist_overview_payload(body.as_bytes())
                .unwrap()
                .overview
                .artist_pick
        };

        let track = pinned(
            r#""  On repeat all summer.  ""#,
            r#"{
                "__typename": "Track",
                "uri": "spotify:track:5aAx2yzptFyBCsvcXQqvcc",
                "name": "Salt Flats",
                "artists": {"items": [{"uri": "spotify:artist:1a2b3c4d5e6f7g8h9i0jkl", "profile": {"name": "Mora Vex"}}]},
                "albumOfTrack": {"uri": "spotify:album:0abcdefghijklmnopqrstu", "name": "Salt Flats", "coverArt": {"sources": [{"url": "cover", "width": 640}]}},
                "duration": {"totalMilliseconds": 214000}
            }"#,
        )
        .expect("a pinned track is an Artist Pick");
        // Trimmed, and kept: the comment is what makes a pick a pick.
        assert_eq!(track.comment.as_deref(), Some("On repeat all summer."));
        match track.item {
            ArtistPickItem::Track(track) => {
                assert_eq!(track.name, "Salt Flats");
                assert_eq!(track.artist_names, vec!["Mora Vex".to_owned()]);
                assert_eq!(track.duration_ms, 214_000);
                assert_eq!(track.cover_url, "cover");
            }
            other => panic!("expected a track, got {other:?}"),
        }

        let album = pinned(
            "null",
            r#"{
                "__typename": "Album",
                "uri": "spotify:album:0abcdefghijklmnopqrstu",
                "name": "Low Country",
                "artists": {"items": [{"uri": "spotify:artist:1a2b3c4d5e6f7g8h9i0jkl", "profile": {"name": "Mora Vex"}}]},
                "coverArt": {"sources": [{"url": "sleeve", "width": 640}]},
                "date": {"year": 2024}
            }"#,
        )
        .expect("a pinned album is an Artist Pick");
        assert!(album.comment.is_none(), "an absent comment stays absent");
        match album.item {
            ArtistPickItem::Album(album) => {
                assert_eq!(album.name, "Low Country");
                assert_eq!(album.year, Some(2024));
            }
            other => panic!("expected an album, got {other:?}"),
        }

        let playlist = pinned(
            r#""""#,
            r#"{
                "__typename": "Playlist",
                "uri": "spotify:playlist:0abcdefghijklmnopqrstu",
                "name": "Late Shift",
                "ownerV2": {"data": {"name": "Mora Vex", "uri": "spotify:user:moravex"}},
                "images": {"items": [{"sources": [{"url": "tile", "width": 640}]}]},
                "content": {"totalCount": 46}
            }"#,
        )
        .expect("a pinned playlist is still an Artist Pick");
        assert!(
            playlist.comment.is_none(),
            "an empty comment is not a comment"
        );
        match playlist.item {
            ArtistPickItem::Playlist(playlist) => {
                assert_eq!(playlist.name, "Late Shift");
                assert_eq!(playlist.track_count, Some(46));
            }
            other => panic!("expected a playlist, got {other:?}"),
        }

        // A kind this build has never heard of is not a card with no name in
        // it, and neither is a well-formed one with nothing to open.
        assert!(
            pinned(
                "null",
                r#"{"__typename": "PreRelease", "uri": "spotify:prerelease:x"}"#
            )
            .is_none()
        );
        assert!(pinned("null", r#"{"__typename": "Track", "name": "No id here"}"#).is_none());
        assert!(pinned("null", "null").is_none());
    }

    #[test]
    fn playcount_queries_match_the_official_persisted_documents() {
        let album: serde_json::Value =
            serde_json::from_str(&album_tracks_body("spotify:album:abc", 300, 17)).unwrap();
        assert_eq!(album["operationName"], "queryAlbumTracks");
        assert_eq!(
            album["extensions"]["persistedQuery"]["sha256Hash"],
            ALBUM_TRACKS_QUERY_HASH
        );
        assert_eq!(album["variables"]["uri"], "spotify:album:abc");
        assert_eq!(album["variables"]["offset"], 300);
        assert_eq!(album["variables"]["limit"], 17);

        let track: serde_json::Value =
            serde_json::from_str(&get_track_body("spotify:track:def")).unwrap();
        assert_eq!(track["operationName"], "getTrack");
        assert_eq!(
            track["extensions"]["persistedQuery"]["sha256Hash"],
            GET_TRACK_QUERY_HASH
        );
        assert_eq!(track["variables"]["uri"], "spotify:track:def");
        assert_eq!(track["variables"]["includeVideoAssociationItems"], false);
    }

    #[test]
    fn album_playcounts_accept_strings_numbers_and_missing_values() {
        let body = r#"{
            "data": {"albumUnion": {"tracksV2": {"items": [
                {"track": {"uri": "spotify:track:one", "playcount": "1234567"}},
                {"track": {"uri": "spotify:track:two", "playcount": 42}},
                {"track": {"uri": "spotify:track:zero", "playcount": "0"}},
                {"track": {"uri": "spotify:track:missing"}},
                {"track": null}
            ]}}}
        }"#;
        let parsed: AlbumCountsResponse = serde_json::from_str(body).unwrap();
        let mut counts = HashMap::new();
        collect_playcount_items(
            parsed
                .data
                .as_ref()
                .and_then(|data| data.albumUnion.as_ref())
                .and_then(|album| album.tracksV2.as_ref())
                .unwrap(),
            &mut counts,
        );
        assert_eq!(counts.get("spotify:track:one"), Some(&1_234_567));
        assert_eq!(counts.get("spotify:track:two"), Some(&42));
        assert!(!counts.contains_key("spotify:track:zero"));
        assert!(!counts.contains_key("spotify:track:missing"));
    }

    #[test]
    fn artist_overview_parser_maps_sparse_facts_in_server_order() {
        let body = br#"{
            "data": {"artistUnion": {
                "profile": {"biography": {"text": "  A real biography.\nSecond paragraph.  "}},
                "visuals": {
                    "avatarImage": {"sources": [
                        {"url": "header-small", "width": 320},
                        {"url": "header-large", "width": 1200}
                    ]},
                    "gallery": {"items": [
                        {"sources": [
                            {"url": "bio-small", "width": 400},
                            {"url": "bio-large", "width": 1600}
                        ]},
                        {"sources": [{"url": "bio-second", "width": 1800}]}
                    ]}
                },
                "stats": {
                    "followers": 1200,
                    "monthlyListeners": "3400",
                    "worldRank": 42,
                    "topCities": {"items": [
                        {"city": "London", "country": "GB", "region": "England", "numberOfListeners": 900},
                        {"city": "Sparse City", "country": null, "numberOfListeners": null},
                        {"city": null, "numberOfListeners": 1}
                    ]}
                },
                "discography": {
                    "topTracks": {"items": [
                        {"track": {"uri": "spotify:track:first", "playcount": "99"}},
                        {"track": null},
                        {"track": {"uri": "spotify:track:zero", "playcount": 0}}
                    ]},
                    "popularReleasesAlbums": {"items": [
                        {"uri": "spotify:album:first", "name": "Album One",
                         "date": {"year": 2024},
                         "coverArt": {"sources": [{"url": "small", "width": 64}, {"url": "cover-one", "width": 300}]},
                         "artists": {"items": [{"uri": "spotify:artist:owner", "profile": {"name": "Owner"}}]}},
                        {"uri": "spotify:album:second", "name": "Album Two"}
                    ]},
                    "popularReleasesSingles": {"items": [
                        {"uri": "spotify:album:single", "name": "Single One"}
                    ]},
                    "popularReleasesCompilations": null
                },
                "relatedContent": {"relatedArtists": {"items": [
                    {"uri": "spotify:artist:related", "profile": {"name": "Related"},
                     "visuals": {"avatarImage": {"sources": [
                         {"url": "related-large", "width": 640},
                         {"url": "related-right", "width": 320}
                     ]}}},
                    {"uri": null, "profile": {"name": "Not a card"}}
                ]}}
            }}
        }"#;

        let parsed = parse_artist_overview_payload(body).unwrap();
        assert_eq!(
            parsed.overview.biography.as_deref(),
            Some("A real biography.\nSecond paragraph.")
        );
        assert_eq!(
            parsed.overview.header_image_url.as_deref(),
            Some("header-large")
        );
        assert_eq!(
            parsed.overview.biography_image_url.as_deref(),
            Some("bio-large")
        );
        assert_eq!(parsed.overview.followers, Some(1200));
        assert_eq!(parsed.overview.monthly_listeners, Some(3400));
        assert_eq!(parsed.overview.world_rank, Some(42));
        assert_eq!(parsed.overview.top_cities.len(), 2);
        assert_eq!(parsed.overview.top_cities[1].country, "");
        assert_eq!(parsed.overview.top_cities[1].listeners, None);
        assert_eq!(
            parsed
                .overview
                .popular_releases
                .iter()
                .map(|release| release.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Album One", "Album Two", "Single One"],
            "server order is preserved; the client never sorts popular releases"
        );
        assert_eq!(
            parsed.overview.popular_releases[0].cover_url.as_deref(),
            Some("cover-one")
        );
        assert_eq!(parsed.overview.related_artists.len(), 1);
        assert_eq!(
            parsed.overview.related_artists[0].portrait_url.as_deref(),
            Some("related-right")
        );
        assert_eq!(parsed.top_playcounts.get("spotify:track:first"), Some(&99));
        assert!(!parsed.top_playcounts.contains_key("spotify:track:zero"));
    }

    #[test]
    fn artist_overview_rotation_rejects_bad_hash_payload_then_accepts_sparse_artist() {
        let mut last_error = String::new();
        assert!(
            accept_artist_overview_attempt(
                &mut last_error,
                Ok(br#"{"errors":[{"message":"PersistedQueryNotFound"}]}"#.to_vec()),
            )
            .is_none()
        );
        assert!(last_error.contains("rejected"));

        let accepted = accept_artist_overview_attempt(
            &mut last_error,
            Ok(br#"{"data":{"artistUnion":{"profile":null,"stats":null,"discography":{"popularReleasesAlbums":{"items":[{"uri":"spotify:album:sparse","name":null,"coverArt":{"sources":null},"artists":{"items":null}}]}}}}}"#.to_vec()),
        )
        .expect("a present sparse artist is a successful fallback");
        assert!(accepted.overview.biography.is_none());
        assert!(accepted.overview.popular_releases.is_empty());
        assert!(accepted.top_playcounts.is_empty());
    }

    #[test]
    fn artist_overview_hashes_keep_their_verified_variable_shapes() {
        assert_eq!(
            ARTIST_OVERVIEW_SCHEMAS[0].hash,
            "1ac33ddab5d39a3a9c27802774e6d78b9405cc188c6f75aed007df2a32737c72"
        );
        let bodies: Vec<serde_json::Value> = ARTIST_OVERVIEW_SCHEMAS
            .iter()
            .map(|schema| {
                serde_json::from_str(&artist_overview_body("spotify:artist:id", *schema)).unwrap()
            })
            .collect();
        for (body, schema) in bodies.iter().zip(ARTIST_OVERVIEW_SCHEMAS) {
            assert_eq!(body["operationName"], "queryArtistOverview");
            assert_eq!(
                body["extensions"]["persistedQuery"]["sha256Hash"],
                schema.hash
            );
            assert_eq!(body["variables"]["uri"], "spotify:artist:id");
        }
        assert_eq!(bodies[0]["variables"]["locale"], "");
        assert!(bodies[0]["variables"].get("preReleaseV2").is_none());
        assert_eq!(bodies[1]["variables"]["locale"], "");
        assert_eq!(bodies[1]["variables"]["preReleaseV2"], false);
        assert!(bodies[2]["variables"].get("locale").is_none());
        assert_eq!(bodies[2]["variables"]["preReleaseV2"], false);
        assert_eq!(
            bodies[3]["variables"].as_object().unwrap().len(),
            1,
            "the oldest document declared only uri"
        );
    }

    #[test]
    fn artist_overview_cache_models_positive_negative_expiry_and_capacity() {
        let now = Instant::now();
        let key = ArtistOverviewCacheKey {
            artist_id: "artist".to_owned(),
            schema_hash: ARTIST_OVERVIEW_SCHEMAS[0].hash,
        };
        let mut cache = ArtistOverviewCache::default();
        let mut query = ArtistOverviewQuery::default();
        query.overview.biography = Some("Cached".to_owned());
        cache.insert(key.clone(), Some(query), now);
        assert_eq!(
            cache
                .get(&key, now + Duration::from_secs(60))
                .unwrap()
                .unwrap()
                .overview
                .biography
                .as_deref(),
            Some("Cached")
        );
        assert!(cache.get(&key, now + ARTIST_OVERVIEW_SUCCESS_TTL).is_none());

        cache.insert(key.clone(), None, now);
        assert!(
            cache
                .get(&key, now + Duration::from_secs(1))
                .is_some_and(|value| value.is_none()),
            "a failed rotation is a cache hit too"
        );
        assert!(cache.get(&key, now + ARTIST_OVERVIEW_FAILURE_TTL).is_none());

        let mut bounded = ArtistOverviewCache::default();
        let first_key = ArtistOverviewCacheKey {
            artist_id: "artist-0".to_owned(),
            schema_hash: ARTIST_OVERVIEW_SCHEMAS[0].hash,
        };
        for index in 0..ARTIST_OVERVIEW_CACHE_CAPACITY {
            bounded.insert(
                ArtistOverviewCacheKey {
                    artist_id: format!("artist-{index}"),
                    schema_hash: ARTIST_OVERVIEW_SCHEMAS[0].hash,
                },
                None,
                now + Duration::from_millis(index as u64),
            );
        }
        assert_eq!(bounded.entries.len(), ARTIST_OVERVIEW_CACHE_CAPACITY);
        let newest_key = ArtistOverviewCacheKey {
            artist_id: "artist-new".to_owned(),
            schema_hash: ARTIST_OVERVIEW_SCHEMAS[0].hash,
        };
        bounded.insert(
            newest_key.clone(),
            None,
            now + Duration::from_millis(ARTIST_OVERVIEW_CACHE_CAPACITY as u64),
        );
        assert_eq!(bounded.entries.len(), ARTIST_OVERVIEW_CACHE_CAPACITY);
        assert!(!bounded.entries.contains_key(&first_key));
        assert!(bounded.entries.contains_key(&newest_key));
    }

    #[test]
    fn metadata4_keeps_the_artist_page_useful_when_every_overview_hash_fails() {
        let mut artist = test_artist("0123456789ABCDEFGHIJKL", "Artist");
        artist.popularity = 73;
        artist.portrait_group = Images(vec![
            image(ImageSize::SMALL, 0xa1),
            image(ImageSize::LARGE, 0xa2),
        ]);
        artist.biographies.0.push(Biography {
            text: "Metadata biography".to_owned(),
            portraits: Images(vec![
                image(ImageSize::SMALL, 0xb1),
                image(ImageSize::LARGE, 0xb2),
            ]),
            portrait_group: Vec::new(),
        });
        let mut related = test_artist("1123456789ABCDEFGHIJKL", "Related");
        related.portrait_group = Images(vec![image(ImageSize::DEFAULT, 0xef)]);
        artist.related = Artists(vec![related]);

        let overview = merge_artist_overview(&artist, None, None).unwrap();
        assert_eq!(overview.biography.as_deref(), Some("Metadata biography"));
        assert_eq!(overview.popularity, Some(73));
        assert_eq!(overview.related_artists.len(), 1);
        assert_eq!(
            overview.header_image_url.as_deref(),
            Some(format!("{COVER_BASE}{}", "a2".repeat(20))).as_deref()
        );
        assert_eq!(
            overview.biography_image_url.as_deref(),
            Some(format!("{COVER_BASE}{}", "b2".repeat(20))).as_deref()
        );
        assert!(overview.followers.is_none());
        assert!(overview.popular_releases.is_empty());
    }

    #[test]
    fn get_track_supplies_the_artist_popular_counts_in_one_response() {
        let body = r#"{
            "data": {"trackUnion": {
                "uri": "spotify:track:seed",
                "playcount": "999",
                "firstArtist": {"items": [{"discography": {"topTracks": {"items": [
                    {"track": {"uri": "spotify:track:seed", "playcount": "999"}},
                    {"track": {"uri": "spotify:track:other", "playcount": "123"}}
                ]}}}]}
            }}
        }"#;
        let parsed: GetTrackCountsResponse = serde_json::from_str(body).unwrap();
        let mut counts = HashMap::new();
        let track = parsed
            .data
            .as_ref()
            .and_then(|data| data.trackUnion.as_ref())
            .unwrap();
        counts.insert(
            track.uri.clone().unwrap(),
            playcount_value(track.playcount.as_ref()).unwrap(),
        );
        collect_playcount_items(
            track
                .firstArtist
                .as_ref()
                .and_then(|artist| artist.items.as_deref())
                .and_then(|items| items[0].discography.as_ref())
                .and_then(|discography| discography.topTracks.as_ref())
                .unwrap(),
            &mut counts,
        );

        let mut tracks = vec![
            TrackRef {
                uri: "spotify:track:seed".to_owned(),
                ..TrackRef::default()
            },
            TrackRef {
                uri: "spotify:track:other".to_owned(),
                ..TrackRef::default()
            },
        ];
        apply_playcounts(&mut tracks, &counts);
        assert_eq!(tracks[0].play_count, Some(999));
        assert_eq!(tracks[1].play_count, Some(123));
    }

    #[test]
    fn playcount_json_tolerates_null_unavailable_entities() {
        for body in [
            r#"{"data": null}"#,
            r#"{"data": {"albumUnion": null}}"#,
            r#"{"data": {"albumUnion": {"tracksV2": null}}}"#,
            r#"{"data": {"albumUnion": {"tracksV2": {"items": null}}}}"#,
        ] {
            let parsed: AlbumCountsResponse = serde_json::from_str(body).unwrap();
            let items = parsed
                .data
                .as_ref()
                .and_then(|data| data.albumUnion.as_ref())
                .and_then(|album| album.tracksV2.as_ref());
            let mut counts = HashMap::new();
            if let Some(items) = items {
                collect_playcount_items(items, &mut counts);
            }
            assert!(counts.is_empty());
        }

        let parsed: GetTrackCountsResponse =
            serde_json::from_str(r#"{"data": {"trackUnion": {"firstArtist": null}}}"#).unwrap();
        assert!(
            parsed
                .data
                .as_ref()
                .and_then(|data| data.trackUnion.as_ref())
                .and_then(|track| track.firstArtist.as_ref())
                .is_none()
        );
    }

    #[test]
    fn artist_release_selection_is_explicit_and_validated() {
        assert_eq!(
            selected_release_group_indices(&[]).unwrap(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            selected_release_group_indices(&["singles".into(), "albums".into()]).unwrap(),
            vec![1, 0]
        );
        assert!(selected_release_group_indices(&["podcasts".into()]).is_err());
    }

    #[test]
    fn initial_artist_releases_round_robin_preserves_each_group_order() {
        let album_uri = |id: &str| SpotifyUri::from_uri(&format!("spotify:album:{id}")).unwrap();
        let groups = [
            vec![
                album_uri("0abcdefghijklmnopqrst0"),
                album_uri("0abcdefghijklmnopqrst1"),
                album_uri("0abcdefghijklmnopqrst2"),
            ],
            vec![
                album_uri("0abcdefghijklmnopqrst3"),
                album_uri("0abcdefghijklmnopqrst4"),
                album_uri("0abcdefghijklmnopqrst5"),
            ],
            vec![
                album_uri("0abcdefghijklmnopqrst6"),
                album_uri("0abcdefghijklmnopqrst7"),
                album_uri("0abcdefghijklmnopqrst8"),
            ],
            vec![
                album_uri("0abcdefghijklmnopqrst9"),
                album_uri("0abcdefghijklmnopqrstA"),
                album_uri("0abcdefghijklmnopqrstB"),
            ],
        ];

        let page = balanced_artist_release_page(&groups, INITIAL_ARTIST_RELEASES);
        assert_eq!(page.len(), INITIAL_ARTIST_RELEASES);
        assert_eq!(
            page.iter().map(|(_, uri)| id_of(uri)).collect::<Vec<_>>(),
            vec![
                "0abcdefghijklmnopqrst0",
                "0abcdefghijklmnopqrst3",
                "0abcdefghijklmnopqrst6",
                "0abcdefghijklmnopqrst9",
                "0abcdefghijklmnopqrst1",
                "0abcdefghijklmnopqrst4",
                "0abcdefghijklmnopqrst7",
                "0abcdefghijklmnopqrstA",
                "0abcdefghijklmnopqrst2",
                "0abcdefghijklmnopqrst5",
                "0abcdefghijklmnopqrst8",
                "0abcdefghijklmnopqrstB",
            ],
        );
    }

    #[test]
    fn initial_artist_releases_skip_empty_groups_without_wasting_capacity() {
        let album_uri = |id: &str| SpotifyUri::from_uri(&format!("spotify:album:{id}")).unwrap();
        let groups = [
            vec![
                album_uri("0abcdefghijklmnopqrstu"),
                album_uri("1abcdefghijklmnopqrstu"),
            ],
            Vec::new(),
            vec![album_uri("2abcdefghijklmnopqrstu")],
            Vec::new(),
        ];

        let page = balanced_artist_release_page(&groups, INITIAL_ARTIST_RELEASES);
        assert_eq!(page.len(), 3);
        assert_eq!(
            page.iter().map(|(group, _)| *group).collect::<Vec<_>>(),
            vec![0, 2, 0],
        );
        assert_eq!(
            page.iter().map(|(_, uri)| id_of(uri)).collect::<Vec<_>>(),
            vec![
                "0abcdefghijklmnopqrstu",
                "2abcdefghijklmnopqrstu",
                "1abcdefghijklmnopqrstu",
            ],
        );
    }

    #[test]
    fn mixed_catalogue_is_sorted_before_pagination_with_stable_year_ties() {
        let seed = |name: &str, year| CatalogueReleaseSeed {
            header: renderer_engine::protocol::AlbumBrowse {
                name: name.to_owned(),
                year,
                ..Default::default()
            },
            track_uris: Vec::new(),
        };
        let mut releases = vec![
            seed("old album", Some(2010)),
            seed("new single", Some(2024)),
            seed("recent album", Some(2023)),
            seed("new album", Some(2024)),
            seed("undated", None),
        ];

        order_catalogue_manifest(&mut releases, 3);

        assert_eq!(
            releases
                .iter()
                .map(|release| release.header.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "new single",
                "new album",
                "recent album",
                "old album",
                "undated",
            ],
        );

        let mut single_group = releases.clone();
        single_group.reverse();
        let source_order = single_group
            .iter()
            .map(|release| release.header.name.clone())
            .collect::<Vec<_>>();
        order_catalogue_manifest(&mut single_group, 1);
        assert_eq!(
            single_group
                .iter()
                .map(|release| release.header.name.clone())
                .collect::<Vec<_>>(),
            source_order,
        );
    }

    #[test]
    fn official_discography_uses_exact_dates_and_preserves_equal_date_order() {
        let albums = br#"{
            "data":{"artistUnion":{"discography":{"albums":{
                "totalCount":2,
                "items":[
                    {"releases":{"items":[
                        {"uri":"spotify:album:0abcdefghijklmnopqrstu",
                         "date":{"isoString":"2025-10-03T00:00:00Z","year":2025}},
                        {"uri":"spotify:album:1abcdefghijklmnopqrstu",
                         "date":{"isoString":"2025-10-03T00:00:00Z","year":2025}}
                    ]}},
                    {"releases":{"items":[
                        {"uri":"spotify:album:2abcdefghijklmnopqrstu",
                         "date":{"isoString":"2024-04-19T00:00:00Z","year":2024}}
                    ]}}
                ]
            }}}}
        }"#;
        let singles = br#"{
            "data":{"artistUnion":{"discography":{"singles":{
                "totalCount":2,
                "items":[
                    {"releases":{"items":[
                        {"uri":"spotify:album:3abcdefghijklmnopqrstu",
                         "date":{"isoString":"2025-10-03T00:00:00Z","year":2025}}
                    ]}},
                    {"releases":{"items":[
                        {"uri":"spotify:album:4abcdefghijklmnopqrstu",
                         "date":{"isoString":"2025-09-01T00:00:00Z","year":2025}}
                    ]}}
                ]
            }}}}
        }"#;
        let (albums, consumed, total) = parse_artist_discography_page(albums, 0).unwrap();
        assert_eq!((consumed, total), (2, 2));
        assert_eq!(id_of(&albums[0].uri), "0abcdefghijklmnopqrstu");
        let (singles, _, _) = parse_artist_discography_page(singles, 1).unwrap();

        let ordered = merge_official_discography([albums, singles]);

        assert_eq!(
            ordered.iter().map(id_of).collect::<Vec<_>>(),
            vec![
                "0abcdefghijklmnopqrstu",
                "3abcdefghijklmnopqrstu",
                "4abcdefghijklmnopqrstu",
                "2abcdefghijklmnopqrstu",
            ],
            "exact dates sort globally; same-day releases retain section/server order",
        );
        let body: serde_json::Value =
            serde_json::from_str(&artist_discography_body("spotify:artist:id", 1, 20, 40).unwrap())
                .unwrap();
        assert_eq!(body["operationName"], "queryArtistDiscographySingles");
        assert_eq!(body["variables"]["order"], "DATE_DESC");
        assert_eq!(
            body["extensions"]["persistedQuery"]["sha256Hash"],
            ARTIST_DISCOGRAPHY_HASH,
        );
    }

    #[test]
    fn album_years_drop_the_placeholder_zero() {
        let covers = Images(vec![image(ImageSize::DEFAULT, 0x11)]);
        let mut album = test_album("0abcdefghijklmnopqrstu", "Album", Vec::new(), covers);

        // A real release date surfaces as its year.
        album.date = Date::from_timestamp_ms(1_388_534_400_000).unwrap();
        assert_eq!(album_year(&album), Some(2014));
        assert_eq!(album_ref(&album).year, Some(2014));

        // No date at all: the protobuf field is absent, librespot's
        // `get_or_default` reads year 0 out of it, and 0000-01-01 UTC is
        // 719_528 days before the epoch. That is a placeholder, not a release
        // year, and must not be rendered under a cover.
        album.date = Date::from_timestamp_ms(-719_528 * 86_400 * 1_000).unwrap();
        assert_eq!(
            album.date.as_utc().year(),
            0,
            "the placeholder really is year 0"
        );
        assert_eq!(album_year(&album), None);
        assert_eq!(album_ref(&album).year, None);
    }

    #[test]
    fn an_artist_portrait_falls_back_to_the_portrait_group() {
        let mut artist = test_artist("0123456789ABCDEFGHIJKL", "Artist");
        assert_eq!(artist_portrait(&artist), None, "no artwork at all is None");

        // The live ARTIST_V4 shape: `portrait` empty, sizes in the group.
        artist.portrait_group = Images(vec![image(ImageSize::DEFAULT, 0xab)]);
        assert_eq!(
            artist_portrait(&artist).as_deref(),
            Some(format!("{COVER_BASE}{}", "ab".repeat(20))).as_deref()
        );

        // A bare `portrait` still wins where the server does send one.
        artist.portraits = Images(vec![image(ImageSize::DEFAULT, 0xcd)]);
        assert_eq!(
            artist_portrait(&artist).as_deref(),
            Some(format!("{COVER_BASE}{}", "cd".repeat(20))).as_deref()
        );
    }

    #[test]
    fn visual_identity_prefers_the_largest_dedicated_wide_header() {
        fn varint(mut value: u64) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let byte = (value & 0x7f) as u8;
                value >>= 7;
                out.push(if value == 0 { byte } else { byte | 0x80 });
                if value == 0 {
                    return out;
                }
            }
        }
        fn message(field: u8, payload: &[u8]) -> Vec<u8> {
            let mut out = vec![(field << 3) | 2];
            out.extend(varint(payload.len() as u64));
            out.extend(payload);
            out
        }
        fn instance(url: &str, size: u8) -> Vec<u8> {
            let mut out = message(1, &message(1, url.as_bytes()));
            out.extend([2 << 3, size]);
            out
        }
        fn group(images: &[(&str, u8)]) -> Vec<u8> {
            images
                .iter()
                .flat_map(|(url, size)| message(1, &instance(url, *size)))
                .collect()
        }

        let mut payload = message(1, &group(&[("avatar", 2)]));
        payload.extend(message(2, &group(&[("biography", 2)])));
        payload.extend(message(
            6,
            &group(&[("header-small", 0), ("header-wide", 2)]),
        ));
        let visuals = parse_artist_visual_identity(&payload).unwrap();

        assert_eq!(visuals.header(), Some("header-wide"));
        assert_eq!(visuals.biography(), Some("biography"));
        assert_ne!(visuals.header(), visuals.square_cover.as_deref());
    }

    #[test]
    fn an_artist_hit_without_a_picture_has_no_portrait() {
        // The live shape for a picture-less artist: `visuals` is present and
        // `avatarImage` is an explicit null, so this must not fail the parse.
        let body = r#"{"data": {"searchV2": {"artists": {"items": [
            {"data": {"uri": "spotify:artist:4abcdefghijklmnopqrstu", "profile": {"name": "No Picture"},
                      "visuals": {"avatarImage": null}}},
            {"data": {"uri": "spotify:artist:5abcdefghijklmnopqrstu", "profile": {"name": "No Visuals"}}}
        ]}}}}"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        let artists: Vec<ArtistRef> = parsed
            .data
            .searchV2
            .artists
            .items
            .iter()
            .map(|wrapper| artist_ref_from_hit(&wrapper.data))
            .collect();
        assert_eq!(artists.len(), 2);
        assert_eq!(artists[0].name, "No Picture");
        assert_eq!(artists[0].portrait_url, None);
        assert_eq!(artists[1].name, "No Visuals");
        assert_eq!(artists[1].portrait_url, None);
    }

    #[test]
    fn hit_images_prefer_the_smallest_source_that_is_big_enough() {
        fn sources(widths: &[u32]) -> SearchCoverArtJson {
            SearchCoverArtJson {
                sources: widths
                    .iter()
                    .map(|width| SearchImageSourceJson {
                        url: Some(format!("https://i.scdn.co/image/w{width}")),
                        width: Some(*width),
                    })
                    .collect(),
            }
        }

        // Artist avatars arrive 640/160/320: order must not decide.
        let avatar = sources(&[640, 160, 320]);
        assert_eq!(
            hit_image(Some(&avatar)).as_deref(),
            Some("https://i.scdn.co/image/w320")
        );
        // Album covers already list the 300px source first; unchanged.
        let cover = sources(&[300, 64, 640]);
        assert_eq!(
            hit_image(Some(&cover)).as_deref(),
            Some("https://i.scdn.co/image/w300")
        );
        // Nothing is big enough: the widest available beats a thumbnail.
        let tiny = sources(&[64, 160]);
        assert_eq!(
            hit_image(Some(&tiny)).as_deref(),
            Some("https://i.scdn.co/image/w160")
        );
        // A source with no width at all is still better than no artwork.
        let unsized_source = SearchCoverArtJson {
            sources: vec![SearchImageSourceJson {
                url: Some("https://i.scdn.co/image/unsized".to_owned()),
                width: None,
            }],
        };
        assert_eq!(
            hit_image(Some(&unsized_source)).as_deref(),
            Some("https://i.scdn.co/image/unsized")
        );
        assert_eq!(hit_image(None), None);
        assert_eq!(hit_image(Some(&sources(&[]))), None);
    }

    #[test]
    fn search_desktop_json_tolerates_unknown_shapes() {
        let body =
            r#"{"weird": true, "data": {"searchV2": {"tracksV2": {"somethingElse": [1, 2]}}}}"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.data.searchV2.tracksV2.items.is_empty());
        assert!(parsed.data.searchV2.albumsV2.items.is_empty());
        assert!(parsed.data.searchV2.artists.items.is_empty());

        // A track with only a URI yields empty strings, never an error.
        let bare = r#"{"data": {"searchV2": {"tracksV2": {"items": [{"item": {"data": {"uri": "spotify:track:2abcdefghijklmnopqrstu"}}}]}}}}"#;
        let parsed: SearchDesktopResponse = serde_json::from_str(bare).unwrap();
        let track = track_ref_from_hit(&parsed.data.searchV2.tracksV2.items[0].item.data);
        assert_eq!(track.id, "2abcdefghijklmnopqrstu");
        assert_eq!(track.name, "");
        assert!(track.artist_names.is_empty());
        assert_eq!(track.cover_url, "");
        assert_eq!(track.duration_ms, 0);

        // Duration may arrive as a JSON number as well as a string.
        let duration: SearchDurationJson =
            serde_json::from_str(r#"{"totalMilliseconds": 184000}"#).unwrap();
        assert_eq!(parse_millis(duration.totalMilliseconds.as_ref()), 184_000);
        let bad: SearchDurationJson =
            serde_json::from_str(r#"{"totalMilliseconds": "not-a-number"}"#).unwrap();
        assert_eq!(parse_millis(bad.totalMilliseconds.as_ref()), 0);
    }

    #[test]
    fn hit_id_takes_the_trailing_uri_segment() {
        assert_eq!(
            hit_id(Some("spotify:user:alice:playlist:0123456789ABCDEFGHIJKL")),
            "0123456789ABCDEFGHIJKL"
        );
        assert_eq!(
            hit_id(Some("spotify:track:1abcdefghijklmnopqrstu")),
            "1abcdefghijklmnopqrstu"
        );
        assert_eq!(hit_id(None), "");
    }

    #[test]
    fn search_desktop_body_builds_the_persisted_query_payload() {
        let body = search_desktop_body("fire & ice?", 10, SEARCH_DESKTOP_HASHES[0]);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["operationName"], "searchDesktop");
        assert_eq!(
            parsed["extensions"]["persistedQuery"]["sha256Hash"],
            SEARCH_DESKTOP_HASHES[0]
        );
        assert_eq!(parsed["extensions"]["persistedQuery"]["version"], 1);
        assert_eq!(parsed["variables"]["searchTerm"], "fire & ice?");
        assert_eq!(parsed["variables"]["limit"], 10);
        assert_eq!(parsed["variables"]["offset"], 0);

        let clamped = search_desktop_body("q", 5000, SEARCH_DESKTOP_HASHES[0]);
        let parsed: serde_json::Value = serde_json::from_str(&clamped).unwrap();
        assert_eq!(
            parsed["variables"]["limit"], 50,
            "limit clamped to the endpoint bound"
        );

        // The newest known hash is tried first; the previous one is the
        // rotation fallback, so a hash rejection degrades gracefully.
        assert_eq!(SEARCH_DESKTOP_HASHES.len(), 2);
        assert_ne!(SEARCH_DESKTOP_HASHES[0], SEARCH_DESKTOP_HASHES[1]);
    }

    #[test]
    fn backoff_sequence_doubles_and_caps() {
        assert_eq!(
            backoff_sequence(3, 250, 2000),
            vec![250, 500, 1000],
            "base doubling"
        );
        assert_eq!(
            backoff_sequence(4, 250, 2000),
            vec![250, 500, 1000, 2000],
            "capped at the maximum"
        );
        assert_eq!(backoff_sequence(0, 250, 2000), Vec::<u64>::new());
    }

    #[test]
    fn browse_error_is_transient_only_retries_server_side_failures() {
        use librespot_core::error::ErrorKind;
        // Network/timeout failures are always transient (librespot maps
        // Mercury errors and 500/503 to `Unavailable`).
        assert!(browse_error_is_transient(ErrorKind::Unavailable, None));
        assert!(browse_error_is_transient(ErrorKind::DeadlineExceeded, None));
        // 5xx HTTP statuses are transient: 500/503 arrive as `Unavailable`,
        // and unmapped ones like 502 Bad Gateway arrive as `Unknown`
        // wrapping the status.
        assert!(browse_error_is_transient(ErrorKind::Unknown, Some(502)));
        assert!(browse_error_is_transient(ErrorKind::Unknown, Some(503)));
        assert!(browse_error_is_transient(ErrorKind::Unavailable, Some(503)));
        // Client errors (4xx) and non-HTTP `Unknown`s are permanent.
        assert!(!browse_error_is_transient(ErrorKind::Unknown, Some(400)));
        assert!(!browse_error_is_transient(ErrorKind::Unknown, None));
        assert!(!browse_error_is_transient(ErrorKind::NotFound, None));
        assert!(!browse_error_is_transient(
            ErrorKind::ResourceExhausted,
            None
        ));
        assert!(!browse_error_is_transient(ErrorKind::InvalidArgument, None));
    }

    #[test]
    fn browse_retry_schedule_is_bounded_and_fits_the_ui_timeout() {
        // The whole rootlist/playlist/search retry cycle must stay well
        // inside the UI's 20-second browse round-trip timeout
        // (playback_engine_client.h default timeoutMs). 5 retries at
        // 500 ms..4 s cap: 500+1000+2000+4000+4000 = 11.5 s of sleep max.
        let backoffs = backoff_sequence(
            BROWSE_RETRY_ATTEMPTS,
            BROWSE_BACKOFF_BASE_MS,
            BROWSE_BACKOFF_MAX_MS,
        );
        assert_eq!(backoffs.len(), BROWSE_RETRY_ATTEMPTS);
        assert_eq!(backoffs, vec![500, 1000, 2000, 4000, 4000]);
        let total_sleep_ms: u64 = backoffs.iter().sum();
        assert!(
            total_sleep_ms < 20_000,
            "bounded retries must not blow the UI round-trip timeout"
        );
        assert!(
            BROWSE_BACKOFF_BASE_MS <= BROWSE_BACKOFF_MAX_MS,
            "backoff must never regress to a decreasing schedule"
        );
    }

    #[test]
    fn dedupe_uris_keeps_first_appearance_order() {
        let a = SpotifyUri::from_uri("spotify:track:0123456789ABCDEFGHIJKL").unwrap();
        let b = SpotifyUri::from_uri("spotify:track:1123456789ABCDEFGHIJKL").unwrap();
        let c = SpotifyUri::from_uri("spotify:track:2123456789ABCDEFGHIJKL").unwrap();
        let repeated = vec![&a, &b, &a, &c, &b];
        let unique = dedupe_uris(repeated.into_iter());
        assert_eq!(
            unique.iter().map(uri_of).collect::<Vec<_>>(),
            vec![
                "spotify:track:0123456789ABCDEFGHIJKL",
                "spotify:track:1123456789ABCDEFGHIJKL",
                "spotify:track:2123456789ABCDEFGHIJKL",
            ],
            "first-appearance order, repeats dropped"
        );
    }

    #[test]
    fn uri_index_maps_each_uri_to_its_first_appearance_slot() {
        let a = SpotifyUri::from_uri("spotify:track:0123456789ABCDEFGHIJKL").unwrap();
        let b = SpotifyUri::from_uri("spotify:track:1123456789ABCDEFGHIJKL").unwrap();
        let index = build_uri_index(&[a.clone(), b, a]);
        assert_eq!(
            index.get("spotify:track:0123456789ABCDEFGHIJKL"),
            Some(&0),
            "first appearance wins, like the scan it replaces"
        );
        assert_eq!(index.get("spotify:track:1123456789ABCDEFGHIJKL"), Some(&1));
        // Every requested URI is addressable; a batch response places each
        // entry in O(1) instead of scanning the request list.
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn missing_playlist_added_timestamp_is_not_synthesized() {
        assert_eq!(playlist_added_at(0), None);
        assert_eq!(playlist_added_at(-1), None);
        assert_eq!(
            playlist_added_at(1_725_000_123_456),
            Some(1_725_000_123_456)
        );
    }

    #[test]
    fn metadata_chunks_split_at_batch_size() {
        let uris: Vec<SpotifyUri> = (0..85)
            .map(|i| {
                let id = format!("{i:0>21}0");
                SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap()
            })
            .collect();
        let chunks = metadata_chunks(&uris, 40);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 40);
        assert_eq!(chunks[1].len(), 40);
        assert_eq!(chunks[2].len(), 5);
        assert_eq!(
            metadata_chunks(&uris, 0).len(),
            85,
            "batch size clamps to 1: every URI gets its own batch"
        );
    }

    #[test]
    fn build_batched_request_carries_one_entity_per_uri_with_the_kind() {
        let a = SpotifyUri::from_uri("spotify:track:0123456789ABCDEFGHIJKL").unwrap();
        let b = SpotifyUri::from_uri("spotify:track:1123456789ABCDEFGHIJKL").unwrap();
        let request = build_batched_request(&[a, b], ExtensionKind::TRACK_V4, "US");
        assert_eq!(request.header.country, "US");
        assert_eq!(request.entity_request.len(), 2);
        assert_eq!(
            request.entity_request[0].entity_uri,
            "spotify:track:0123456789ABCDEFGHIJKL"
        );
        assert_eq!(
            request.entity_request[1].entity_uri,
            "spotify:track:1123456789ABCDEFGHIJKL"
        );
        assert_eq!(
            request.entity_request[0].query[0]
                .extension_kind
                .enum_value_or(ExtensionKind::UNKNOWN_EXTENSION),
            ExtensionKind::TRACK_V4
        );
        assert_eq!(
            request.entity_request[1].query[0]
                .extension_kind
                .enum_value_or(ExtensionKind::UNKNOWN_EXTENSION),
            ExtensionKind::TRACK_V4
        );
    }

    #[test]
    fn collect_extension_payloads_keeps_matching_200_entries_only() {
        use librespot_protocol::entity_extension_data::{
            EntityExtensionData, EntityExtensionDataHeader,
        };
        use librespot_protocol::extended_metadata::EntityExtensionDataArray;
        use protobuf::well_known_types::any::Any;

        let mut ok = EntityExtensionData::new();
        ok.entity_uri = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa".to_owned();
        ok.header = protobuf::MessageField::some(EntityExtensionDataHeader {
            status_code: 200,
            ..Default::default()
        });
        ok.extension_data = protobuf::MessageField::some(Any {
            type_url: "type.googleapis.com/spotify.metadata.Track".to_owned(),
            value: vec![0xAA, 0xBB],
            ..Default::default()
        });

        let mut not_found = EntityExtensionData::new();
        not_found.entity_uri = "spotify:track:bbbbbbbbbbbbbbbbbbbbbb".to_owned();
        not_found.header = protobuf::MessageField::some(EntityExtensionDataHeader {
            status_code: 404,
            ..Default::default()
        });

        let mut no_payload = EntityExtensionData::new();
        no_payload.entity_uri = "spotify:track:cccccccccccccccccccccc".to_owned();
        no_payload.header = protobuf::MessageField::some(EntityExtensionDataHeader {
            status_code: 200,
            ..Default::default()
        });

        let mut track_array = EntityExtensionDataArray::new();
        track_array.extension_kind = EnumOrUnknown::new(ExtensionKind::TRACK_V4);
        track_array.extension_data = vec![ok.clone(), not_found, no_payload];

        let mut album_array = EntityExtensionDataArray::new();
        album_array.extension_kind = EnumOrUnknown::new(ExtensionKind::ALBUM_V4);
        album_array.extension_data = vec![ok];

        let response = BatchedExtensionResponse {
            extended_metadata: vec![track_array, album_array],
            ..Default::default()
        };

        let entries = collect_extension_payloads(&response, ExtensionKind::TRACK_V4);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "spotify:track:aaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(entries[0].1, vec![0xAA, 0xBB]);
    }
    fn radio_test_track(id: &str) -> TrackRef {
        TrackRef {
            id: id.to_owned(),
            uri: format!("spotify:track:{id}"),
            name: id.to_owned(),
            ..TrackRef::default()
        }
    }

    #[test]
    fn inspired_by_parser_accepts_ranked_media_items_and_playlist_envelopes() {
        let direct = parse_inspired_by(
            br#"{"mediaItems":[
                {"uri":"spotify:track:0123456789ABCDEFGHIJKL"},
                {"uri":7},
                {"uri":"spotify:episode:0123456789ABCDEFGHIJKL"},
                {"uri":"spotify:track:1123456789ABCDEFGHIJKL"}
            ],"futureField":true}"#,
        )
        .unwrap();
        let InspiredBySource::Tracks(tracks) = direct else {
            panic!("mediaItems must win over other response fields");
        };
        assert_eq!(
            tracks.iter().map(uri_of).collect::<Vec<_>>(),
            [
                "spotify:track:0123456789ABCDEFGHIJKL",
                "spotify:track:1123456789ABCDEFGHIJKL",
            ]
        );

        let playlist = parse_inspired_by(
            br#"{"result":{"context":{"uri":"spotify:playlist:2123456789ABCDEFGHIJKL"}}}"#,
        )
        .unwrap();
        let InspiredBySource::Playlist(uri) = playlist else {
            panic!("nested playlist URI must be accepted");
        };
        assert_eq!(uri_of(&uri), "spotify:playlist:2123456789ABCDEFGHIJKL");

        assert!(parse_inspired_by(br#"{"mediaItems":[{"uri":null}]}"#).is_err());
        assert!(parse_inspired_by(b"not json").is_err());
    }

    #[test]
    fn playlist_radio_cover_prefers_picture_file_id_over_picture_sizes() {
        let attributes = |picture, picture_sizes| PlaylistAttributes {
            name: String::new(),
            description: String::new(),
            picture,
            is_collaborative: false,
            pl3_version: String::new(),
            is_deleted_by_owner: false,
            client_id: String::new(),
            format: String::new(),
            format_attributes: Default::default(),
            picture_sizes,
        };
        let sizes = PictureSizes(vec![PictureSize {
            target_name: "default".to_owned(),
            url: "https://i.scdn.co/image/from-size".to_owned(),
        }]);

        assert_eq!(
            playlist_attributes_cover(&attributes(vec![0xAB, 0xCD], sizes.clone())),
            Some("https://i.scdn.co/image/abcd".to_owned())
        );
        assert_eq!(
            playlist_attributes_cover(&attributes(Vec::new(), sizes)),
            Some("https://i.scdn.co/image/from-size".to_owned())
        );
        assert_eq!(
            playlist_attributes_cover(&attributes(
                Vec::new(),
                PictureSizes(vec![PictureSize {
                    target_name: "default".to_owned(),
                    url: String::new(),
                }])
            )),
            None
        );
    }

    #[test]
    fn apollo_parser_accepts_original_gid_and_legacy_uri() {
        let tracks = parse_apollo_tracks(
            br#"{"tracks":[
                {"original_gid":"0123456789ABCDEFGHIJKL"},
                {"original_gid":"00000000000000000000000000000001"},
                {"uri":"spotify:track:1123456789ABCDEFGHIJKL"},
                {"original_gid":"bad","uri":"spotify:episode:2123456789ABCDEFGHIJKL"},
                {"future":"ignored"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(uri_of(&tracks[0]), "spotify:track:0123456789ABCDEFGHIJKL");
        assert_eq!(uri_of(&tracks[2]), "spotify:track:1123456789ABCDEFGHIJKL");
        assert!(matches!(tracks[1], SpotifyUri::Track { .. }));
        assert!(parse_apollo_tracks(br#"{"tracks":"wrong"}"#).is_err());
    }

    #[test]
    fn radio_seed_parser_distinguishes_track_and_artist_contexts() {
        let RadioSeed::Track(track) = parse_radio_seed("0123456789ABCDEFGHIJKL").unwrap() else {
            panic!("plain ids must remain song-radio seeds");
        };
        assert_eq!(uri_of(&track), "spotify:track:0123456789ABCDEFGHIJKL");

        let RadioSeed::Artist(artist) = parse_radio_seed("artist:0123456789ABCDEFGHIJKL").unwrap()
        else {
            panic!("artist-prefixed ids must select artist radio");
        };
        assert_eq!(uri_of(&artist), "spotify:artist:0123456789ABCDEFGHIJKL");
        assert!(parse_radio_seed("artist:not-an-id").is_err());
    }

    #[test]
    fn radio_seed_is_first_once_and_ranked_recommendations_are_capped() {
        let seed = radio_test_track("seed");
        let mut ranked = vec![
            radio_test_track("first"),
            radio_test_track("seed"),
            radio_test_track("first"),
            radio_test_track("second"),
        ];
        ranked.extend((0..60).map(|i| radio_test_track(&format!("rank-{i}"))));
        let tracks = ranked_radio_tracks(seed, ranked);
        assert_eq!(tracks.len(), MAX_RADIO_RECOMMENDATIONS + 1);
        assert_eq!(tracks[0].id, "seed");
        assert_eq!(tracks[1].id, "first");
        assert_eq!(tracks[2].id, "second");
        assert_eq!(tracks.iter().filter(|track| track.id == "seed").count(), 1);
        assert_eq!(tracks.iter().filter(|track| track.id == "first").count(), 1);
    }
}

#[cfg(test)]
mod file_index_tests {
    use super::*;
    use librespot_metadata::track::Tracks;

    /// The index is a process-wide map, so every test in here uses ids of its
    /// own rather than sharing fixtures.
    fn uri(id: &str) -> SpotifyUri {
        SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap()
    }

    #[test]
    fn a_substitute_recording_is_remembered_so_it_can_be_followed_later() {
        let mut track = tests::test_track(
            "1indexAAAAAAAAAAAAAAAA",
            "Region locked",
            1000,
            tests::test_album(
                "1indexBBBBBBBBBBBBBBBB",
                "Album",
                Vec::new(),
                Default::default(),
            ),
            Vec::new(),
        );
        // The shape this exists for: no files of its own, so the only way to
        // answer honestly is through whatever librespot would substitute.
        track.alternatives = Tracks(vec![uri("1indexCCCCCCCCCCCCCCCC")]);

        remember_track_files("1indexAAAAAAAAAAAAAAAA", &track);

        let index = FILE_IDS.read().unwrap();
        let entry = index
            .get("1indexAAAAAAAAAAAAAAAA")
            .expect("a parsed track is in the index");
        assert!(
            entry.files.is_empty(),
            "this fixture has no files of its own"
        );
        assert_eq!(
            entry.alternatives,
            vec!["1indexCCCCCCCCCCCCCCCC".to_owned()],
            "the substitute must be recorded, or the track reads as uncached forever"
        );
    }

    #[test]
    fn without_a_cache_nothing_is_reported_as_cached() {
        let ids = vec!["1indexDDDDDDDDDDDDDDDD".to_owned()];
        assert!(cached_track_ids(&ids, None).is_empty());
    }
}
