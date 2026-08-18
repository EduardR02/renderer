//! spclient-backed browsing: turns librespot metadata and the internal
//! `/playlist/v2` + `/searchview` JSON endpoints into the protocol's browse
//! payloads. All network traffic goes through the engine session's spclient
//! (login5 Bearer auth + automatic client token), so no developer Web API
//! client id is involved.
//!
//! Response-mapping notes:
//! - Rootlist: `/playlist/v2/user/{user}/rootlist` answered as
//!   protobuf-JSON with `contents.items`/`contents.metaItems` parallel
//!   arrays (shape cross-checked against other spclient clients). The
//!   rootlist carries owner usernames but no owner display names, so
//!   `owner_name` is empty there. Playlist covers come from the raw
//!   `attributes.picture` file id (base64 bytes -> `https://i.scdn.co/image/
//!   {hex}`), with ready-made `pictureSize` URLs as fallback.
//! - Search: `/searchview/km/v4/search/{q}` answered as protobuf-JSON with
//!   `results.tracks|albums|artists.hits`. The query parameters follow
//!   librespot-java's SearchManager: `entityVersion=2` and — critically —
//!   non-empty `country` (from the session) and `locale` values; the
//!   searchview service rejects requests with empty `country`/`locale` with
//!   a 400 INVALID_ARGUMENT. Parsing stays tolerant: unknown/missing fields
//!   degrade to empty strings, zeros, and empty lists instead of failing the
//!   whole browse.

use std::collections::HashSet;

use base64::Engine as _;
use futures_util::stream::{StreamExt, iter};
use http::Method;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track, image::Images};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::protocol::{AlbumRef, ArtistRef, PlaylistRef, TrackRef};

/// Public artwork base; every cover URL is `{COVER_BASE}{40-hex-file-id}`.
const COVER_BASE: &str = "https://i.scdn.co/image/";

/// Concurrent in-flight metadata fetches when resolving a batch of track or
/// album URIs (playlist/album/artist contents arrive as bare URIs).
const FETCH_CONCURRENCY: usize = 8;

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


/// Resolves a batch of track URIs into `TrackRef`s. URIs are deduplicated by
/// id (playlists repeat tracks), fetched with bounded concurrency in
/// first-appearance order, and items that fail to resolve (episodes, local
/// files, removed tracks) are skipped.
pub async fn fetch_tracks<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Vec<TrackRef> {
    let mut seen = HashSet::new();
    let unique: Vec<SpotifyUri> = uris
        .into_iter()
        .filter(|uri| seen.insert(id_of(uri)))
        .cloned()
        .collect();
    iter(unique.into_iter().map(|uri| async move {
        match Track::get(session, &uri).await {
            Ok(track) => Some(track_ref(&track)),
            Err(error) => {
                eprintln!("skipping unresolvable item {uri}: {error}");
                None
            }
        }
    }))
    .buffered(FETCH_CONCURRENCY)
    .filter_map(|track| async move { track })
    .collect()
    .await
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
fn rootlist_cover(attributes: &RootlistAttributesJson) -> Option<String> {
    if let Some(picture) = attributes
        .picture
        .as_deref()
        .filter(|picture| !picture.is_empty())
    {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(picture) {
            if !bytes.is_empty() {
                return Some(format!("{COVER_BASE}{}", hex(&bytes)));
            }
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

/// The user's playlist library from the spclient rootlist endpoint.
pub async fn playlists_browse(session: &Session, length: usize) -> Result<Vec<PlaylistRef>, String> {
    let length = length.clamp(1, MAX_PLAYLISTS);
    let endpoint = format!(
        "/playlist/v2/user/{user}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length={length}",
        user = session.username(),
    );
    let body = session
        .spclient()
        .request_as_json(&Method::GET, &endpoint, None, None)
        .await
        .map_err(|error| format!("rootlist request failed: {error}"))?;
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

/// Playlist header and tracks via the spclient playlist4 endpoint.
pub async fn playlist_browse(
    session: &Session,
    id: &str,
) -> Result<crate::protocol::PlaylistBrowse, String> {
    let uri = playlist_uri(id)?;
    let playlist = Playlist::get(session, &uri)
        .await
        .map_err(|error| format!("playlist fetch failed: {error}"))?;
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
    let album = Album::get(session, &uri)
        .await
        .map_err(|error| format!("album fetch failed: {error}"))?;
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

/// Resolves a batch of album URIs into `AlbumRef`s, preserving order and
/// skipping albums that fail to resolve.
async fn fetch_albums<'a>(
    session: &Session,
    uris: impl IntoIterator<Item = &'a SpotifyUri>,
) -> Vec<AlbumRef> {
    let unique: Vec<SpotifyUri> = uris.into_iter().cloned().collect();
    iter(unique.into_iter().map(|uri| async move {
        match Album::get(session, &uri).await {
            Ok(album) => Some(album_ref(&album)),
            Err(error) => {
                eprintln!("skipping unresolvable album {uri}: {error}");
                None
            }
        }
    }))
    .buffered(FETCH_CONCURRENCY)
    .filter_map(|album| async move { album })
    .collect()
    .await
}

/// Artist portrait, top tracks, and albums via the extended-metadata artist
/// endpoint.
pub async fn artist_browse(
    session: &Session,
    id: &str,
) -> Result<crate::protocol::ArtistBrowse, String> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))
        .map_err(|error| format!("invalid artist id: {error}"))?;
    let artist = Artist::get(session, &uri)
        .await
        .map_err(|error| format!("artist fetch failed: {error}"))?;
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

/// Builds the searchview request path. `country` must be the session's
/// two-letter country code and `locale` a language tag: the searchview
/// service answers 400 INVALID_ARGUMENT when either is empty (the query
/// parameters mirror librespot-java's SearchManager, which fills both from
/// the session).
pub fn search_endpoint(
    query: &str,
    limit: usize,
    country: &str,
    locale: &str,
    username: &str,
) -> String {
    let encoded = utf8_percent_encode(query.trim(), NON_ALPHANUMERIC);
    format!(
        "/searchview/km/v4/search/{encoded}?entityVersion=2&limit={limit}&country={country}&locale={locale}&username={user}",
        limit = limit.clamp(1, MAX_SEARCH_LIMIT),
        country = utf8_percent_encode(country, NON_ALPHANUMERIC),
        locale = utf8_percent_encode(locale, NON_ALPHANUMERIC),
        user = username,
    )
}

/// Search via the spclient searchview endpoint. The response mapping is
/// tolerant: any section the server leaves out (or names differently) simply
/// yields an empty list.
pub async fn search_browse(
    session: &Session,
    query: &str,
    limit: usize,
) -> Result<crate::protocol::SearchBrowse, String> {
    if query.trim().is_empty() {
        return Err("search query must not be empty".to_owned());
    }
    let endpoint = search_endpoint(
        query,
        limit,
        &session.country(),
        SEARCH_LOCALE,
        &session.username(),
    );    let body = session
        .spclient()
        .request_as_json(&Method::GET, &endpoint, None, None)
        .await
        .map_err(|error| format!("search request failed: {error}"))?;
    let parsed: SearchJson = serde_json::from_slice(&body)
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
        // INVALID_ARGUMENT; the endpoint must always carry the session's
        // two-letter country code and a locale (librespot-java fills both
        // from the session the same way).
        let endpoint = search_endpoint("fire & ice?", 10, "US", "en", "alice");
        assert!(endpoint.starts_with("/searchview/km/v4/search/fire%20%26%20ice%3F?"));
        assert!(endpoint.contains("entityVersion=2"));
        assert!(endpoint.contains("&limit=10"));
        assert!(endpoint.contains("&country=US"));
        assert!(endpoint.contains("&locale=en"));
        assert!(endpoint.contains("&username=alice"));

        let clamped = search_endpoint("q", 5000, "US", "en", "alice");
        assert!(clamped.contains("&limit=50"));
        assert!(clamped.contains("&country=US"), "country must never be empty");
        assert!(clamped.contains("&locale=en"), "locale must never be empty");
    }
}
