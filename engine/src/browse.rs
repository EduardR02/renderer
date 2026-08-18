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
//! - Search: the searchview service is reached over Mercury
//!   (`hm://searchview/km/v4/search/{q}`) exactly like the working
//!   implementations in librespot-java's `SearchManager` and
//!   librespot-python's `SearchManager` (both still shipping this shape as
//!   of 2026): `entityVersion=2` plus non-empty `country` (from the
//!   session), `locale`, `imageSize`, `catalogue`, and `username` values.
//!   The engine's earlier HTTP spclient variant (`/searchview/...` with
//!   `request_as_json`) kept answering `400 INVALID_ARGUMENT` even with
//!   country/locale filled — the HTTP front door also receives spclient's
//!   auto-appended `product=0&country=..&salt=..` query params, which the
//!   strict searchview transcoding rejects. Mercury carries exactly the
//!   accepted parameter set and is what both reference clients use. Parsing
//!   stays tolerant: unknown/missing fields degrade to empty strings, zeros,
//!   and empty lists instead of failing the whole browse, and failures log
//!   the gRPC status and response body.

use std::collections::HashSet;

use base64::Engine as _;
use http::Method;
use librespot_core::error::ErrorKind;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track, image::Images};
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use protobuf::{EnumOrUnknown, Message};
use serde::Deserialize;
use std::time::Duration;

use crate::protocol::{AlbumRef, ArtistRef, PlaylistRef, TrackRef};

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
/// them before an error reaches the UI. Same capped exponential schedule as
/// the metadata batches; the whole cycle (3 retries, at most 1750 ms of
/// sleep) fits comfortably inside the UI's 20-second browse round-trip
/// timeout.
const BROWSE_RETRY_ATTEMPTS: usize = 3;
const BROWSE_BACKOFF_BASE_MS: u64 = 250;
const BROWSE_BACKOFF_MAX_MS: u64 = 2_000;

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

/// Converts resolved track metadata into the protocol's `TrackRef` shape
/// (identical to the one `play_queue` receives from the UI).
pub fn track_ref(track: &Track) -> TrackRef {
    TrackRef {
        id: id_of(&track.id),
        uri: uri_of(&track.id),
        name: track.name.clone(),
        artist_names: track.artists.iter().map(|artist| artist.name.clone()).collect(),
        artist_id: track
            .artists
            .first()
            .map(|artist| id_of(&artist.id))
            .unwrap_or_default(),
        album_id: id_of(&track.album.id),
        album_name: track.album.name.clone(),
        cover_url: cover_url(&track.album.covers).unwrap_or_default(),
        duration_ms: u32::try_from(track.duration).unwrap_or(0),
    }
}

/// Converts resolved album metadata into the protocol's `AlbumRef` shape.
pub fn album_ref(album: &Album) -> AlbumRef {
    AlbumRef {
        id: id_of(&album.id),
        uri: uri_of(&album.id),
        name: album.name.clone(),
        artist_names: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        cover_url: cover_url(&album.covers),
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

/// Resolves extended-metadata entities (tracks, albums, ...) in batches of
/// [`METADATA_BATCH_SIZE`]. URIs are deduplicated by id in first-appearance
/// order; each batch is one POST; items missing from the response or failing
/// per-entity resolution are skipped; a batch is skipped only after its
/// retries with backoff are exhausted.
async fn fetch_extended<'a, T>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
    kind: ExtensionKind,
    parse: impl Fn(&str, &[u8]) -> Option<T>,
) -> Vec<T> {
    let unique = dedupe_uris(uris.into_iter());
    let mut results: Vec<Option<T>> = (0..unique.len()).map(|_| None).collect();
    for chunk in metadata_chunks(&unique, METADATA_BATCH_SIZE) {
        match fetch_extended_batch(session, chunk, kind).await {
            Ok(entries) => {
                for (entity_uri, payload) in entries {
                    let Some(index) = unique.iter().position(|uri| uri_of(uri) == entity_uri)
                    else {
                        continue;
                    };
                    results[index] = parse(&entity_uri, &payload);
                }
            }
            Err(error) => {
                // Only a fully retried batch failure skips its items.
                eprintln!("skipping {count} unresolvable item(s): {error}", count = chunk.len());
            }
        }
    }
    results.into_iter().flatten().collect()
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

/// Resolves a batch of track URIs into `TrackRef`s via the extended-metadata
/// endpoint: URIs are deduplicated by id (playlists repeat tracks), fetched
/// in ~40-URI batches in first-appearance order, and items that fail to
/// resolve (episodes, local files, removed tracks) are skipped. No per-item
/// network calls: one POST per batch.
pub async fn fetch_tracks<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Vec<TrackRef> {
    fetch_extended(session, uris, ExtensionKind::TRACK_V4, parse_track_payload).await
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
) -> Result<crate::protocol::PlaylistBrowse, String> {
    let uri = playlist_uri(id)?;
    let playlist: Playlist = metadata_get(session, &uri, "playlist").await?;
    let (owner_id, owner_name) = match &playlist.id {
        SpotifyUri::Playlist { user, .. } => (
            user.clone().unwrap_or_default(),
            user.clone().unwrap_or_default(),
        ),
        _ => (String::new(), String::new()),
    };
    let tracks = fetch_tracks(session, playlist.tracks()).await;
    Ok(crate::protocol::PlaylistBrowse {
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
) -> Result<crate::protocol::AlbumBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}"))
        .map_err(|error| format!("invalid album id: {error}"))?;
    let album: Album = metadata_get(session, &uri, "album").await?;
    let tracks = fetch_tracks(session, album.tracks()).await;
    Ok(crate::protocol::AlbumBrowse {
        id: id_of(&album.id),
        uri: uri_of(&album.id),
        name: album.name.clone(),
        artist_names: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        cover_url: cover_url(&album.covers),
        tracks,
    })
}

/// Resolves a batch of album URIs into `AlbumRef`s via the extended-metadata
/// endpoint, preserving order and skipping albums that fail to resolve.
async fn fetch_albums<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Vec<AlbumRef> {
    fetch_extended(session, uris, ExtensionKind::ALBUM_V4, parse_album_payload).await
}

/// Artist portrait, top tracks, and albums via the extended-metadata artist
/// endpoint.
pub async fn artist_browse(
    session: &Session,
    id: &str,
) -> Result<crate::protocol::ArtistBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
        .map_err(|error| format!("invalid artist id: {error}"))?;
    let artist: Artist = metadata_get(session, &uri, "artist").await?;
    let top_tracks = artist.top_tracks.for_country(&session.country());
    let top_tracks = fetch_tracks(session, top_tracks.iter()).await;
    let albums = fetch_albums(session, artist.albums_current()).await;
    Ok(crate::protocol::ArtistBrowse {
        id: id_of(&artist.id),
        uri: uri_of(&artist.id),
        name: artist.name.clone(),
        portrait_url: cover_url(&artist.portraits),
        top_tracks,
        albums,
    })
}

// ---------------------------------------------------------------------------
// search (searchview)
// ---------------------------------------------------------------------------

/// Raw protobuf-JSON of the searchview response. Field names follow the
/// official client's protobuf-JSON mapping and stay optional everywhere so a
/// renamed/missing field degrades to an empty section, never an error.
#[derive(Default, Deserialize)]
struct SearchJson {
    #[serde(default)]
    results: SearchResultsJson,
}

#[derive(Default, Deserialize)]
struct SearchResultsJson {
    #[serde(default)]
    tracks: TrackHitSectionJson,
    #[serde(default)]
    albums: AlbumHitSectionJson,
    #[serde(default)]
    artists: ArtistHitSectionJson,
}

#[derive(Default, Deserialize)]
struct TrackHitSectionJson {
    #[serde(default)]
    hits: Vec<SearchTrackHitJson>,
}

#[derive(Default, Deserialize)]
struct AlbumHitSectionJson {
    #[serde(default)]
    hits: Vec<SearchAlbumHitJson>,
}

#[derive(Default, Deserialize)]
struct ArtistHitSectionJson {
    #[serde(default)]
    hits: Vec<SearchArtistHitJson>,
}


#[derive(Default, Deserialize)]
struct SearchTrackHitJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artists: Vec<SearchArtistJson>,
    #[serde(default)]
    album: Option<SearchAlbumJson>,
    #[serde(default)]
    image: Vec<SearchImageJson>,
    #[serde(default)]
    duration: Option<SearchDurationJson>,
}

#[derive(Default, Deserialize)]
struct SearchAlbumHitJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artists: Vec<SearchArtistJson>,
    #[serde(default)]
    image: Vec<SearchImageJson>,
}

#[derive(Default, Deserialize)]
struct SearchArtistHitJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Vec<SearchImageJson>,
}

#[derive(Default, Deserialize)]
struct SearchArtistJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
struct SearchAlbumJson {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
struct SearchImageJson {
    /// `spotify:image:{40-hex-file-id}` (the official client's image uri).
    #[serde(default)]
    uri: Option<String>,
    file_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct SearchDurationJson {
    /// protobuf-JSON maps int64 to a string, so accept both.
    #[serde(default)]
    milliseconds: Option<serde_json::Value>,
}

/// Extracts the 22-char id from a `spotify:{type}:{id}` hit uri.
fn hit_id(raw_uri: Option<&str>) -> String {
    raw_uri
        .and_then(|uri| uri.rsplit(':').next())
        .unwrap_or_default()
        .to_owned()
}

fn hit_image(images: &[SearchImageJson]) -> Option<String> {
    images.iter().find_map(|image| {
        if let Some(uri) = image.uri.as_deref() {
            if let Some(file_id) = uri.strip_prefix("spotify:image:") {
                if !file_id.is_empty() {
                    return Some(format!("{COVER_BASE}{file_id}"));
                }
            }
        }
        image
            .file_id
            .as_deref()
            .filter(|file_id| !file_id.is_empty())
            .map(|file_id| format!("{COVER_BASE}{file_id}"))
    })
}

fn parse_millis(value: Option<&serde_json::Value>) -> u32 {
    let millis = value.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    });
    millis.and_then(|ms| u32::try_from(ms).ok()).unwrap_or(0)
}

fn track_ref_from_hit(hit: &SearchTrackHitJson) -> TrackRef {
    TrackRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        artist_names: hit
            .artists
            .iter()
            .filter_map(|artist| artist.name.clone())
            .collect(),
        artist_id: hit
            .artists
            .first()
            .map(|artist| hit_id(artist.uri.as_deref()))
            .unwrap_or_default(),
        album_id: hit
            .album
            .as_ref()
            .map(|album| hit_id(album.uri.as_deref()))
            .unwrap_or_default(),
        album_name: hit
            .album
            .as_ref()
            .and_then(|album| album.name.clone())
            .unwrap_or_default(),
        cover_url: hit_image(&hit.image).unwrap_or_default(),
        duration_ms: parse_millis(hit.duration.as_ref().and_then(|d| d.milliseconds.as_ref())),
    }
}

fn album_ref_from_hit(hit: &SearchAlbumHitJson) -> AlbumRef {
    AlbumRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        artist_names: hit
            .artists
            .iter()
            .filter_map(|artist| artist.name.clone())
            .collect(),
        cover_url: hit_image(&hit.image),
    }
}

fn artist_ref_from_hit(hit: &SearchArtistHitJson) -> ArtistRef {
    ArtistRef {
        id: hit_id(hit.uri.as_deref()),
        uri: hit.uri.clone().unwrap_or_default(),
        name: hit.name.clone().unwrap_or_default(),
        portrait_url: hit_image(&hit.image),
    }
}

/// Locale sent to searchview; the engine session carries no user locale, and
/// librespot-java's default preferred locale is "en".
const SEARCH_LOCALE: &str = "en";

/// Builds the searchview request URI. `country` must be the session's
/// two-letter country code and `locale` a language tag: the searchview
/// service answers 400 INVALID_ARGUMENT when either is empty. The URI
/// mirrors librespot-java's `SearchManager` byte for byte (path
/// `hm://searchview/km/v4/search/{q}` plus `entityVersion`, `limit`,
/// `imageSize`, `catalogue`, `country`, `locale`, `username`), which is the
/// request shape both working reference clients send over Mercury. Sending
/// the same parameters over the HTTP spclient front door instead fails with
/// 400 INVALID_ARGUMENT (the HTTP path also receives spclient's
/// auto-appended `product`/`salt` query params, which the strict searchview
/// transcoding rejects).
pub fn search_endpoint(
    query: &str,
    limit: usize,
    country: &str,
    locale: &str,
    username: &str,
) -> String {
    let encoded = utf8_percent_encode(query.trim(), NON_ALPHANUMERIC);
    format!(
        "hm://searchview/km/v4/search/{encoded}?entityVersion=2&limit={limit}&imageSize=&catalogue=&country={country}&locale={locale}&username={user}",
        limit = limit.clamp(1, MAX_SEARCH_LIMIT),
        country = utf8_percent_encode(country, NON_ALPHANUMERIC),
        locale = utf8_percent_encode(locale, NON_ALPHANUMERIC),
        user = username,
    )
}

/// Search via the searchview service over Mercury (the transport both
/// librespot-java and librespot-python use for this endpoint). Failures
/// carry the gRPC status and, when present, the server's response body so
/// the next iteration can read the exact rejection message. The response
/// mapping is tolerant: any section the server leaves out (or names
/// differently) simply yields an empty list.
pub async fn search_browse(
    session: &Session,
    query: &str,
    limit: usize,
) -> Result<crate::protocol::SearchBrowse, String> {
    if query.trim().is_empty() {
        return Err("search query must not be empty".to_owned());
    }
    let uri = search_endpoint(
        query,
        limit,
        &session.country(),
        SEARCH_LOCALE,
        &session.username(),
    );
    // The Mercury round-trip is retried with the bounded backoff schedule:
    // transport failures (librespot reports Mercury errors as `Unavailable`)
    // and server-side status codes (gRPC-style 4 deadline / 13 internal /
    // 14 unavailable) are transient; client errors such as 400
    // INVALID_ARGUMENT fail fast.
    let backoffs = backoff_sequence(
        BROWSE_RETRY_ATTEMPTS,
        BROWSE_BACKOFF_BASE_MS,
        BROWSE_BACKOFF_MAX_MS,
    );
    let mut attempt = 0usize;
    let response = loop {
        let response = match session.mercury().get(&uri) {
            Ok(request) => match request.await {
                Ok(response) => response,
                Err(error) => {
                    if !browse_error_is_transient(error.kind, http_status_of(&error))
                        || attempt >= backoffs.len()
                    {
                        return Err(format!(
                            "search request failed: {uri} after {} attempts (last error: {error})",
                            attempt + 1,
                        ));
                    }
                    let backoff = backoffs[attempt];
                    eprintln!(
                        "search request {uri} failed ({error}); retrying in {backoff} ms (attempt {}/{})",
                        attempt + 1,
                        backoffs.len() + 1,
                    );
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                    attempt += 1;
                    continue;
                }
            },
            Err(error) => return Err(format!("search request failed: {error}")),
        };
        let transient = response.status_code != 200
            && matches!(response.status_code, 4 | 13 | 14);
        if !transient || attempt >= backoffs.len() {
            break response;
        }
        let backoff = backoffs[attempt];
        eprintln!(
            "search request {uri} failed (status {}); retrying in {backoff} ms (attempt {}/{})",
            response.status_code,
            attempt + 1,
            backoffs.len() + 1,
        );
        tokio::time::sleep(Duration::from_millis(backoff)).await;
        attempt += 1;
    };
    if response.status_code != 200 {
        let body = response
            .payload
            .first()
            .and_then(|payload| String::from_utf8(payload.clone()).ok())
            .filter(|body| !body.is_empty())
            .map(|body| {
                let mut body = body;
                body.truncate(512);
                format!(": {body}")
            })
            .unwrap_or_default();
        return Err(format!(
            "search request failed: {uri} after {} attempt(s) (status {}):{}",
            attempt + 1,
            response.status_code,
            body
        ));
    }
    let body = response
        .payload
        .first()
        .ok_or_else(|| "search returned no payload".to_owned())?;
    let parsed: SearchJson = serde_json::from_slice(body)
        .map_err(|error| format!("unparseable search response: {error}"))?;
    Ok(crate::protocol::SearchBrowse {
        tracks: parsed.results.tracks.hits.iter().map(track_ref_from_hit).collect(),
        albums: parsed.results.albums.hits.iter().map(album_ref_from_hit).collect(),
        artists: parsed
            .results
            .artists
            .hits
            .iter()
            .map(artist_ref_from_hit)
            .collect(),
    })
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
    fn search_json_maps_tracks_albums_and_artists() {
        let body = r#"{
            "results": {
                "tracks": {
                    "hits": [{
                        "uri": "spotify:track:2abcdefghijklmnopqrstu",
                        "name": "Search Track",
                        "artists": [{"uri": "spotify:artist:0123456789ABCDEFGHIJKL", "name": "Search Artist"}],
                        "album": {"uri": "spotify:album:0abcdefghijklmnopqrstu", "name": "Search Album"},
                        "image": [{"uri": "spotify:image:ab67616d0000b2730123456789abcdef01234567", "size": "DEFAULT"}],
                        "duration": {"milliseconds": "211000"},
                        "playable": true
                    }],
                    "hitLimit": 10
                },
                "albums": {
                    "hits": [{
                        "uri": "spotify:album:1abcdefghijklmnopqrstu",
                        "name": "Album Hit",
                        "artists": [{"name": "Album Artist"}],
                        "image": []
                    }]
                },
                "artists": {
                    "hits": [{
                        "uri": "spotify:artist:3abcdefghijklmnopqrstu",
                        "name": "Artist Hit"
                    }]
                }
            }
        }"#;
        let parsed: SearchJson = serde_json::from_str(body).unwrap();
        let converted = crate::protocol::SearchBrowse {
            tracks: parsed.results.tracks.hits.iter().map(track_ref_from_hit).collect(),
            albums: parsed.results.albums.hits.iter().map(album_ref_from_hit).collect(),
            artists: parsed
                .results
                .artists
                .hits
                .iter()
                .map(artist_ref_from_hit)
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
            "https://i.scdn.co/image/ab67616d0000b2730123456789abcdef01234567"
        );
        assert_eq!(track.duration_ms, 211_000);

        assert_eq!(converted.albums.len(), 1);
        assert_eq!(converted.albums[0].id, "1abcdefghijklmnopqrstu");
        assert_eq!(converted.albums[0].artist_names, vec!["Album Artist".to_owned()]);
        assert_eq!(converted.albums[0].cover_url, None);

        assert_eq!(converted.artists.len(), 1);
        assert_eq!(converted.artists[0].id, "3abcdefghijklmnopqrstu");
        assert_eq!(converted.artists[0].name, "Artist Hit");
        assert_eq!(converted.artists[0].portrait_url, None);
    }

    #[test]
    fn search_json_tolerates_unknown_shapes() {
        let body = r#"{"weird": true, "results": {"tracks": {"somethingElse": [1, 2]}}}"#;
        let parsed: SearchJson = serde_json::from_str(body).unwrap();
        let tracks: Vec<TrackRef> = parsed.results.tracks.hits.iter().map(track_ref_from_hit).collect();
        assert!(tracks.is_empty());
        // Duration may arrive as a JSON number as well as a string.
        let duration: SearchDurationJson =
            serde_json::from_str(r#"{"milliseconds": 184000}"#).unwrap();
        assert_eq!(parse_millis(duration.milliseconds.as_ref()), 184_000);
        let bad: SearchDurationJson =
            serde_json::from_str(r#"{"milliseconds": "not-a-number"}"#).unwrap();
        assert_eq!(parse_millis(bad.milliseconds.as_ref()), 0);
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
    fn search_endpoint_encodes_the_query_and_fills_session_values() {
        // The searchview service rejects empty country/locale with 400
        // INVALID_ARGUMENT; the URI must always carry the session's
        // two-letter country code and a locale. The shape mirrors
        // librespot-java's SearchManager exactly (Mercury `hm://` transport,
        // entityVersion/limit/imageSize/catalogue/country/locale/username).
        let endpoint = search_endpoint("fire & ice?", 10, "US", "en", "alice");
        assert!(endpoint.starts_with("hm://searchview/km/v4/search/fire%20%26%20ice%3F?"));
        assert!(endpoint.contains("entityVersion=2"));
        assert!(endpoint.contains("&limit=10"));
        assert!(endpoint.contains("&imageSize="));
        assert!(endpoint.contains("&catalogue="));
        assert!(endpoint.contains("&country=US"));
        assert!(endpoint.contains("&locale=en"));
        assert!(endpoint.contains("&username=alice"));

        let clamped = search_endpoint("q", 5000, "US", "en", "alice");
        assert!(clamped.contains("&limit=50"));
        assert!(clamped.contains("&country=US"), "country must never be empty");
        assert!(clamped.contains("&locale=en"), "locale must never be empty");
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
        // The whole rootlist/metadata/search retry cycle must stay well
        // inside the UI's 20-second browse round-trip timeout
        // (playback_engine_client.h default timeoutMs).
        let backoffs = backoff_sequence(
            BROWSE_RETRY_ATTEMPTS,
            BROWSE_BACKOFF_BASE_MS,
            BROWSE_BACKOFF_MAX_MS,
        );
        assert_eq!(backoffs.len(), BROWSE_RETRY_ATTEMPTS);
        assert_eq!(backoffs, vec![250, 500, 1000]);
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
