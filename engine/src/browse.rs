//! spclient-backed browsing: turns librespot metadata and the internal
//! `/playlist/v2` + `/searchview` endpoints into the protocol's browse
//! payloads. All network traffic goes through the engine session (login5
//! Bearer auth + automatic client token), so no developer Web API client id
//! is involved.
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
//!   `queryAlbumTracks` document; artist Popular counts are recovered from the
//!   first top track's `getTrack` document, whose first-artist discography
//!   carries the same top-track play counts in one response.

use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use bytes::Bytes;
use http::{Method, Request};
use librespot_core::error::ErrorKind;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track, image::Images};
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use protobuf::{EnumOrUnknown, Message};
use serde::Deserialize;
use std::time::Duration;

use spotify_playback_engine::protocol::{
    AlbumRef, ArtistRef, CreditArtist, CreditRole, PlaylistRef, TrackCredits, TrackRef,
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

fn id_of(uri: &SpotifyUri) -> String {
    uri.to_id().unwrap_or_default()
}

fn uri_of(uri: &SpotifyUri) -> String {
    uri.to_uri().unwrap_or_default()
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
/// anything. One line, first resolved track, no per-track cost.
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

/// Converts resolved track metadata into the protocol's `TrackRef` shape
/// (identical to the one `play_queue` receives from the UI).
pub fn track_ref(track: &Track) -> TrackRef {
    log_available_formats_once(track);
    TrackRef {
        id: id_of(&track.id),
        uri: uri_of(&track.id),
        name: track.name.clone(),
        artist_names: track.artists.iter().map(|artist| artist.name.clone()).collect(),
        // Same list, same order, same pass: the ids were already parsed here
        // and simply discarded, so every credited artist becomes linkable for
        // no extra request.
        artist_ids: track.artists.iter().map(|artist| id_of(&artist.id)).collect(),
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
    }
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
        artist_names: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        artist_ids: album.artists.iter().map(|artist| id_of(&artist.id)).collect(),
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
    uris
        .into_iter()
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
        if array.extension_kind.enum_value_or(ExtensionKind::UNKNOWN_EXTENSION) != kind {
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
        match session.spclient().get_extended_metadata(request.clone()).await {
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
                eprintln!("skipping {count} unresolvable item(s): {error}", count = chunk.len());
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
fn parse_track_payload(entity_uri: &str, payload: &[u8]) -> Option<TrackRef> {
    let message = librespot_protocol::metadata::Track::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let track = Track::parse(&message, &uri).ok()?;
    Some(track_ref(&track))
}

/// Parses one extended-metadata album payload into the protocol's `AlbumRef`
/// shape. Returns `None` for unparseable or non-album payloads.
fn parse_album_payload(entity_uri: &str, payload: &[u8]) -> Option<AlbumRef> {
    let message = librespot_protocol::metadata::Album::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let album = Album::parse(&message, &uri).ok()?;
    Some(album_ref(&album))
}

/// Album metadata already carries its ordered track URI list, so a catalogue
/// queue can resolve albums in batches and then resolve all tracks in batches;
/// it never needs one network request per release or per song.
fn parse_album_track_uris(entity_uri: &str, payload: &[u8]) -> Option<Vec<SpotifyUri>> {
    let message = librespot_protocol::metadata::Album::parse_from_bytes(payload).ok()?;
    let uri = SpotifyUri::from_uri(entity_uri).ok()?;
    let album = Album::parse(&message, &uri).ok()?;
    Some(album.tracks().cloned().collect())
}

/// Resolves a batch of track URIs into `TrackRef`s via the extended-metadata
/// endpoint: URIs are deduplicated by id (playlists repeat tracks), fetched
/// in ~40-URI batches in first-appearance order, and items that fail to
/// resolve (episodes, local files, removed tracks) are skipped. No per-item
/// network calls: one POST per batch.
pub async fn fetch_tracks<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Result<Vec<TrackRef>, String> {
    fetch_extended(session, uris, ExtensionKind::TRACK_V4, parse_track_payload).await
}

/// Playlist item attributes carry the only trustworthy "added" timestamp.
/// A protobuf default of zero means the field was absent, not January 1970,
/// so it must remain missing in the browse payload.
fn playlist_added_at(timestamp_ms: i64) -> Option<i64> {
    (timestamp_ms > 0).then_some(timestamp_ms)
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
    let by_uri: HashMap<String, TrackRef> = resolved
        .into_iter()
        .map(|track| (track.uri.clone(), track))
        .collect();
    Ok(items
        .iter()
        .filter_map(|(uri, added_at)| {
            let mut track = by_uri.get(&uri_of(uri))?.clone();
            track.added_at = *added_at;
            Some(track)
        })
        .collect())
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
        owner_id: meta.owner_username.clone().or(user).unwrap_or_default(),
        // The rootlist carries owner usernames but no display names.
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
pub async fn playlists_browse(session: &Session, length: usize) -> Result<Vec<PlaylistRef>, String> {
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
) -> Result<spotify_playback_engine::protocol::PlaylistBrowse, String> {
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
    Ok(spotify_playback_engine::protocol::PlaylistBrowse {
        id: id_of(&playlist.id),
        uri: uri_of(&playlist.id),
        name: playlist.name().to_owned(),
        revision: Some(hex(&playlist.revision)),
        owner_id,
        owner_name,
        cover_url: playlist_attributes_cover(&playlist.attributes),
        tracks,
    })
}

/// Playlist artwork: the attributes' raw picture file id (i.scdn.co) wins;
/// ready-made `picture_sizes` URLs are the fallback.
fn playlist_attributes_cover(attributes: &librespot_metadata::playlist::attribute::PlaylistAttributes) -> Option<String> {
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
) -> Result<spotify_playback_engine::protocol::AlbumBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}"))
        .map_err(|error| format!("invalid album id: {error}"))?;
    let album: Album = metadata_get(session, &uri, "album").await?;
    let album_track_count = album.tracks().count();
    let mut tracks = fetch_tracks(session, album.tracks()).await?;
    match album_playcounts(session, &uri_of(&album.id), album_track_count).await {
        Ok(counts) => apply_playcounts(&mut tracks, &counts),
        Err(error) => eprintln!("album play counts unavailable: {error}"),
    }
    Ok(spotify_playback_engine::protocol::AlbumBrowse {
        id: id_of(&album.id),
        uri: uri_of(&album.id),
        name: album.name.clone(),
        artist_names: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        artist_ids: album.artists.iter().map(|artist| id_of(&artist.id)).collect(),
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

/// Album URIs one artist browse will resolve, across all four release groups.
///
/// The groups themselves are free — they arrive inside the single artist
/// metadata response as lists of URIs — but turning those URIs into names,
/// covers and years costs one extended-metadata POST per 40. Catalogue sizes
/// are wildly uneven and the tail is entirely in `appears_on`: measured live
/// (2026-08-19) Taylor Swift has 33/79/1/158 across albums/singles/
/// compilations/appears-on, David Bowie 60/102/22/154, and Drake 21/62/0/836.
/// Resolving everything would therefore cost 7 requests for one artist and 23
/// for another, against the 1 this browse used to cost.
///
/// A flat budget keeps that predictable instead: whoever the artist is, an
/// artist page costs at most `200 / 40 = 5` album requests. Normal artists
/// stay well inside it and see their whole catalogue.
const ARTIST_RELEASE_BUDGET: usize = 200;

/// Splits [`ARTIST_RELEASE_BUDGET`] across the four groups in priority order:
/// the artist's own albums first, then singles, then compilations, and
/// "appears on" last with whatever is left.
///
/// Priority order rather than equal shares because the groups are not equally
/// wanted: an artist's own releases are the page, while `appears_on` is the
/// long tail nobody scrolls to the end of — and it is precisely the group
/// that runs to the hundreds. Pure so the arithmetic is unit-testable.
fn allocate_release_budget(sizes: [usize; 4], budget: usize) -> [usize; 4] {
    let mut remaining = budget;
    let mut taken = [0usize; 4];
    for (slot, size) in taken.iter_mut().zip(sizes) {
        *slot = size.min(remaining);
        remaining -= *slot;
    }
    taken
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
) -> Result<spotify_playback_engine::protocol::ArtistBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
        .map_err(|error| format!("invalid artist id: {error}"))?;
    let artist: Artist = metadata_get(session, &uri, "artist").await?;
    let top_tracks = artist.top_tracks.for_country(&session.country());
    let mut top_tracks = fetch_tracks(session, top_tracks.iter()).await?;
    if let Some(seed) = top_tracks
        .iter()
        .find(|track| track.artist_id == id)
        .or_else(|| top_tracks.first())
    {
        match artist_top_playcounts(session, &seed.uri).await {
            Ok(counts) => apply_playcounts(&mut top_tracks, &counts),
            Err(error) => eprintln!("artist play counts unavailable: {error}"),
        }
    }

    let groups: [Vec<SpotifyUri>; 4] = [
        artist.albums_current().cloned().collect(),
        artist.singles_current().cloned().collect(),
        artist.compilations_current().cloned().collect(),
        artist.appears_on_albums_current().cloned().collect(),
    ];
    let takes = allocate_release_budget(
        [groups[0].len(), groups[1].len(), groups[2].len(), groups[3].len()],
        ARTIST_RELEASE_BUDGET,
    );
    let wanted: Vec<&SpotifyUri> = groups
        .iter()
        .zip(takes)
        .flat_map(|(group, take)| group.iter().take(take))
        .collect();
    let resolved = fetch_albums(session, wanted).await?;

    // Resolution drops albums that fail per-entity (region-locked, pulled),
    // so the groups are rebuilt by lookup rather than by position.
    let by_id: std::collections::HashMap<&str, &AlbumRef> = resolved
        .iter()
        .map(|album| (album.id.as_str(), album))
        .collect();
    let group_of = |index: usize| -> Vec<AlbumRef> {
        groups[index]
            .iter()
            .take(takes[index])
            .filter_map(|uri| by_id.get(id_of(uri).as_str()).map(|album| (*album).clone()))
            .collect()
    };

    Ok(spotify_playback_engine::protocol::ArtistBrowse {
        id: id_of(&artist.id),
        uri: uri_of(&artist.id),
        name: artist.name.clone(),
        portrait_url: artist_portrait(&artist),
        top_tracks,
        releases: spotify_playback_engine::protocol::ArtistReleases {
            albums: group_of(0),
            singles: group_of(1),
            compilations: group_of(2),
            appears_on: group_of(3),
        },
    })
}

/// Explicit, lazy cross-release queue for an artist catalogue. Album and
/// single groups are the useful default; compilations and appears-on releases
/// are opt-in because they are duplicate-heavy and can be enormous. The
/// request only happens when the user presses Play catalogue.
pub async fn artist_catalogue_tracks_browse(
    session: &Session,
    id: &str,
    release_types: &[String],
) -> Result<Vec<TrackRef>, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
        .map_err(|error| format!("invalid artist id: {error}"))?;
    let artist: Artist = metadata_get(session, &uri, "artist").await?;

    let wanted: HashSet<&str> = if release_types.is_empty() {
        ["albums", "singles"].into_iter().collect()
    } else {
        release_types.iter().map(String::as_str).collect()
    };
    if let Some(invalid) = wanted
        .iter()
        .find(|kind| !matches!(**kind, "albums" | "singles" | "compilations" | "appears_on"))
    {
        return Err(format!("unsupported artist release type: {invalid}"));
    }

    let groups: [(&str, Vec<SpotifyUri>); 4] = [
        ("albums", artist.albums_current().cloned().collect()),
        ("singles", artist.singles_current().cloned().collect()),
        ("compilations", artist.compilations_current().cloned().collect()),
        (
            "appears_on",
            artist.appears_on_albums_current().cloned().collect(),
        ),
    ];
    const MAX_CATALOGUE_RELEASES: usize = 250;
    let mut seen_releases = HashSet::new();
    let releases: Vec<SpotifyUri> = groups
        .iter()
        .filter(|(kind, _)| wanted.contains(kind))
        .flat_map(|(_, releases)| releases.iter())
        .filter(|uri| seen_releases.insert(uri_of(uri)))
        .take(MAX_CATALOGUE_RELEASES)
        .cloned()
        .collect();
    if releases.is_empty() {
        return Ok(Vec::new());
    }

    let album_tracks = fetch_extended(
        session,
        releases.iter(),
        ExtensionKind::ALBUM_V4,
        parse_album_track_uris,
    )
    .await?;
    let track_uris: Vec<SpotifyUri> = album_tracks.into_iter().flatten().collect();
    fetch_tracks(session, track_uris.iter()).await
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
) -> Result<spotify_playback_engine::protocol::LikedSongsPage, String> {
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
    Ok(spotify_playback_engine::protocol::LikedSongsPage {
        tracks,
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
            .or_else(|| item.reference.as_ref().and_then(|reference| reference.url.clone()))
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
            subroles: if role.is_empty() { Vec::new() } else { vec![role] },
        });
    }

    TrackCredits {
        track_uri: track_uri.to_owned(),
        track_name: track.and_then(|track| track.name.clone()).unwrap_or_default(),
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
    tracksV2: SearchTrackSectionJson,
    #[serde(default)]
    albumsV2: SearchAlbumSectionJson,
    #[serde(default)]
    artists: SearchArtistSectionJson,
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
        // Free: the year travels in the same search response. A year of 0
        // would be a placeholder, not a release date.
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
) -> Result<spotify_playback_engine::protocol::SearchBrowse, String> {
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
        return Ok(spotify_playback_engine::protocol::SearchBrowse {
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
    use librespot_metadata::album::Discs;
    use librespot_metadata::artist::Artists;
    use librespot_metadata::image::{Image, ImageSize};
    use librespot_metadata::track::Tracks;
    use librespot_metadata::album::AlbumType;

    use super::*;

    use librespot_core::date::Date;

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

    fn test_album(id: &str, name: &str, artists: Vec<Artist>, covers: Images) -> Album {
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

    fn test_track(id: &str, name: &str, duration: i32, album: Album, artists: Vec<Artist>) -> Track {
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
        assert!(cover_url(&without_default).unwrap().ends_with(&"33".repeat(20)));
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
        assert_eq!(refs[0].uri, "spotify:user:alice:playlist:0123456789ABCDEFGHIJKL");
        assert_eq!(refs[0].name, "Road Trip");
        assert_eq!(refs[0].owner_id, "alice");
        assert_eq!(refs[0].owner_name, "");
        assert_eq!(refs[0].track_count, Some(42));
        // The base64 picture decodes to a 16-byte file id rendered as hex.
        assert!(refs[0].cover_url.as_deref().unwrap().starts_with(COVER_BASE));
        assert_eq!(refs[0].cover_url.as_deref().unwrap().len(), COVER_BASE.len() + 32);

        assert_eq!(refs[1].id, "1abcdefghijklmnopqrstu");
        assert_eq!(refs[1].owner_id, "bob");
        assert_eq!(refs[1].track_count, Some(7));
        assert_eq!(
            refs[1].cover_url.as_deref(),
            Some("https://mosaic.scdn.co/640/abc")
        );
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
        assert_eq!(cover.len(), COVER_BASE.len() + 32, "16 decoded bytes -> 32 hex chars");

        // No picture: the ready-made URL is used.
        let url_only = RootlistAttributesJson {
            name: None,
            picture: None,
            picture_size: vec![PictureSizeJson {
                url: Some("https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567".to_owned()),
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
        assert_eq!(cover.len(), COVER_BASE.len() + 32, "16 decoded bytes -> 32 hex chars");
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
        let converted = spotify_playback_engine::protocol::SearchBrowse {
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
            track.cover_url,
            "https://i.scdn.co/image/ab67616d00001e020123456789abcdef01234567",
            "the ≥300px source wins over the 64px one"
        );
        assert_eq!(track.duration_ms, 211_000);

        assert_eq!(converted.albums.len(), 1);
        assert_eq!(converted.albums[0].id, "1abcdefghijklmnopqrstu");
        assert_eq!(converted.albums[0].artist_names, vec!["Album Artist".to_owned()]);
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
        let zeroed: SearchAlbumHitJson =
            serde_json::from_str(r#"{"uri": "spotify:album:9abcdefghijklmnopqrstu", "date": {"year": 0}}"#)
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
            writers.artists[0].url,
            "https://artists.spotify.com/songwriter/1pTJCipDqvUaFmILQLnMsC",
            "the songwriter link is taken verbatim from the service, never built"
        );

        // No name is nothing to render.
        assert_eq!(writers.artists.len(), 3);
        assert_eq!(writers.artists[1].name, "Annie Clark");
        assert_eq!(writers.artists[1].id, "06HL4z0CvFAxyc27GXpf02");
        assert_eq!(writers.artists[2].name, "Uncredited Ghost");
        assert_eq!(writers.artists[2].url, "", "no link when the service sends none");
        assert_eq!(writers.artists[2].id, "");

        // `reference.url` is the documented fallback for the external link.
        assert_eq!(credits.roles[2].artists[0].url, "https://example.invalid/ref");
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

    #[test]
    fn playcount_queries_match_the_official_persisted_documents() {
        let album: serde_json::Value =
            serde_json::from_str(&album_tracks_body("spotify:album:abc", 300, 17)).unwrap();
        assert_eq!(album["operationName"], "queryAlbumTracks");
        assert_eq!(album["extensions"]["persistedQuery"]["sha256Hash"], ALBUM_TRACKS_QUERY_HASH);
        assert_eq!(album["variables"]["uri"], "spotify:album:abc");
        assert_eq!(album["variables"]["offset"], 300);
        assert_eq!(album["variables"]["limit"], 17);

        let track: serde_json::Value =
            serde_json::from_str(&get_track_body("spotify:track:def")).unwrap();
        assert_eq!(track["operationName"], "getTrack");
        assert_eq!(track["extensions"]["persistedQuery"]["sha256Hash"], GET_TRACK_QUERY_HASH);
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

        let parsed: GetTrackCountsResponse = serde_json::from_str(
            r#"{"data": {"trackUnion": {"firstArtist": null}}}"#,
        )
        .unwrap();
        assert!(parsed
            .data
            .as_ref()
            .and_then(|data| data.trackUnion.as_ref())
            .and_then(|track| track.firstArtist.as_ref())
            .is_none());
    }

    #[test]
    fn the_release_budget_favours_the_artists_own_catalogue() {
        // Measured live: Taylor Swift's four groups. Everything but the
        // "appears on" tail fits, so her own catalogue is complete.
        assert_eq!(
            allocate_release_budget([33, 79, 1, 158], ARTIST_RELEASE_BUDGET),
            [33, 79, 1, 87]
        );
        // Drake: 836 features. The budget stops that from costing 23 requests.
        let drake = allocate_release_budget([21, 62, 0, 836], ARTIST_RELEASE_BUDGET);
        assert_eq!(drake, [21, 62, 0, 117]);
        assert_eq!(drake.iter().sum::<usize>(), ARTIST_RELEASE_BUDGET);
        // David Bowie: own catalogue alone nearly fills the budget, and it is
        // the "appears on" group that gets squeezed, never the albums.
        assert_eq!(
            allocate_release_budget([60, 102, 22, 154], ARTIST_RELEASE_BUDGET),
            [60, 102, 22, 16]
        );

        // A modest artist is fully resolved, and an artist with nothing costs
        // no album request at all.
        assert_eq!(allocate_release_budget([4, 2, 0, 3], ARTIST_RELEASE_BUDGET), [4, 2, 0, 3]);
        assert_eq!(allocate_release_budget([0, 0, 0, 0], ARTIST_RELEASE_BUDGET), [0, 0, 0, 0]);
        // The budget is never exceeded, even when the first group alone busts it.
        assert_eq!(allocate_release_budget([500, 10, 10, 10], 200), [200, 0, 0, 0]);
        assert_eq!(allocate_release_budget([5, 5, 5, 5], 0), [0, 0, 0, 0]);
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
        assert_eq!(album.date.as_utc().year(), 0, "the placeholder really is year 0");
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
        let body = r#"{"weird": true, "data": {"searchV2": {"tracksV2": {"somethingElse": [1, 2]}}}}"#;
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
        assert_eq!(hit_id(Some("spotify:track:1abcdefghijklmnopqrstu")), "1abcdefghijklmnopqrstu");
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
        assert_eq!(parsed["variables"]["limit"], 50, "limit clamped to the endpoint bound");

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
        assert!(!browse_error_is_transient(ErrorKind::ResourceExhausted, None));
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
        assert_eq!(playlist_added_at(1_725_000_123_456), Some(1_725_000_123_456));
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
        assert_eq!(request.entity_request[0].entity_uri, "spotify:track:0123456789ABCDEFGHIJKL");
        assert_eq!(request.entity_request[1].entity_uri, "spotify:track:1123456789ABCDEFGHIJKL");
        assert_eq!(
            request.entity_request[0].query[0].extension_kind.enum_value_or(ExtensionKind::UNKNOWN_EXTENSION),
            ExtensionKind::TRACK_V4
        );
        assert_eq!(request.entity_request[1].query[0].extension_kind.enum_value_or(ExtensionKind::UNKNOWN_EXTENSION),
            ExtensionKind::TRACK_V4);
    }

    #[test]
    fn collect_extension_payloads_keeps_matching_200_entries_only() {
        use librespot_protocol::extended_metadata::EntityExtensionDataArray;
        use librespot_protocol::entity_extension_data::{EntityExtensionData, EntityExtensionDataHeader};
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
}
