//! Shared serde types for the Tauri <-> frontend contract.
//!
//! Field names are the exact JSON keys the frontend consumes; they mirror
//! the engine's protocol payloads (see `spotify_playback_engine::protocol`)
//! so the engine's line-JSON state and browse messages deserialize straight
//! into these types.

use serde::{Deserialize, Serialize};

use spotify_playback_engine::protocol::{
    AlbumBrowse, AlbumRef, ArtistBrowse, ArtistCataloguePage, ArtistOverview, ArtistRef,
    ArtistReleaseCounts, ArtistReleasePage, ArtistReleases, ArtistTopCity, CreditArtist,
    CreditRole, LikedSongsPage, PlaylistBrowse, PlaylistRecommendations, PlaylistRef, RadioBrowse,
    SearchBrowse, TrackCredits, TrackRef,
};

/// One playable track. Field-for-field identical to the engine's `TrackRef`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to `artist_names` — same length, same order — so
    /// every credited artist on a track is individually linkable instead of
    /// just the primary.
    ///
    /// The two are always the same length: the engine builds them in one pass
    /// over one list, and [`align_artist_ids`] repairs entries loaded from a
    /// cache written before this field existed. An individual id may still be
    /// empty for an artist with no resolvable id, so render a name as a link
    /// only when its id is non-empty.
    pub artist_ids: Vec<String>,
    /// Primary artist, unchanged and still used on its own in several places.
    pub artist_id: String,
    pub album_id: String,
    pub album_name: String,
    pub cover_url: String,
    pub duration_ms: u32,
    /// Lifetime Spotify play count on surfaces whose official payload carries
    /// it (albums and artist Popular). Missing everywhere else.
    pub play_count: Option<u64>,
    /// Unix timestamp in milliseconds when this playlist item was added.
    /// Absent for tracks sourced from albums, search, or legacy caches.
    pub added_at: Option<i64>,
    /// Stable metadata/account restriction. Runtime player/network failures
    /// never mutate this persisted field.
    pub unavailable: bool,
    pub unavailable_reason: Option<String>,
}

impl From<TrackRef> for Track {
    fn from(track: TrackRef) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_ids: track.artist_ids,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason,
        }
    }
}

impl From<&TrackRef> for Track {
    fn from(track: &TrackRef) -> Self {
        Self {
            id: track.id.clone(),
            uri: track.uri.clone(),
            name: track.name.clone(),
            artist_names: track.artist_names.clone(),
            artist_ids: track.artist_ids.clone(),
            artist_id: track.artist_id.clone(),
            album_id: track.album_id.clone(),
            album_name: track.album_name.clone(),
            cover_url: track.cover_url.clone(),
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason.clone(),
        }
    }
}

impl From<Track> for TrackRef {
    fn from(track: Track) -> Self {
        Self {
            id: track.id,
            uri: track.uri,
            name: track.name,
            artist_names: track.artist_names,
            artist_ids: track.artist_ids,
            artist_id: track.artist_id,
            album_id: track.album_id,
            album_name: track.album_name,
            cover_url: track.cover_url,
            duration_ms: track.duration_ms,
            play_count: track.play_count,
            added_at: track.added_at,
            unavailable: track.unavailable,
            unavailable_reason: track.unavailable_reason,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LikedSongsDetail {
    pub tracks: Vec<Track>,
    pub next_cursor: Option<String>,
}

impl From<LikedSongsPage> for LikedSongsDetail {
    fn from(page: LikedSongsPage) -> Self {
        Self {
            tracks: page.tracks.into_iter().map(Track::from).collect(),
            next_cursor: page.next_cursor,
        }
    }
}
/// A playlist in the user's library (rootlist).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub description: String,
    /// Display name of the owning user (falls back to the username).
    pub owner: String,
    pub owner_id: String,
    pub cover_url: String,
    /// Up to [`COVER_URL_CANDIDATES`] distinct track covers, in track order,
    /// derived the last time the playlist was browsed. Spotify's rootlist
    /// returns no artwork at all for most playlists, so this is what the
    /// surfaces that only ever see the library payload — the sidebar, the home
    /// grid — mosaic instead of dropping to a monogram tile. Empty until a
    /// browse fills it in; a real `cover_url` still outranks it.
    pub cover_urls: Vec<String>,
    pub collaborative: bool,
    pub tracks_total: u32,
    /// Playlist4 revision hex (the Web API "snapshot id"); empty when the
    /// library listing did not carry a revision.
    pub snapshot_id: String,
    /// Unix seconds when playback was last started *from* this playlist, or
    /// `None` for one that never has been. Set only after a successful
    /// `play_queue` from this playlist, via the `touch_playlist` command.
    ///
    /// Home's "Recently played" shelf is ordered by this field. It is never
    /// changed merely by opening, adding to, or otherwise editing a playlist.
    /// Local to this app — Spotify has no equivalent and none is sent.
    pub last_played: Option<i64>,
    /// Unix seconds when this playlist was last used in the library, or
    /// `None` for one with no local activity. A successful add-to-playlist or
    /// play stamps it through its corresponding local command.
    ///
    /// Sidebar and library ordering use this field rather than
    /// [`last_played`], so a playlist can be promoted by an add without
    /// appearing in Home's listening history.
    /// Local to this app — Spotify has no equivalent and none is sent.
    pub last_activity: Option<i64>,
}

impl From<&PlaylistRef> for Playlist {
    fn from(reference: &PlaylistRef) -> Self {
        Self {
            id: reference.id.clone(),
            uri: reference.uri.clone(),
            name: reference.name.clone(),
            description: reference.description.clone().unwrap_or_default(),
            owner: if reference.owner_name.is_empty() {
                reference.owner_id.clone()
            } else {
                reference.owner_name.clone()
            },
            owner_id: reference.owner_id.clone(),
            cover_url: reference.cover_url.clone().unwrap_or_default(),
            // The rootlist reference carries neither the collaborative flag,
            // nor a revision, nor any track — so the cover candidates have to
            // be carried over from a browse (see `carry_browse_fields`).
            collaborative: false,
            tracks_total: reference.track_count.unwrap_or(0),
            cover_urls: Vec::new(),
            snapshot_id: String::new(),
            last_played: None,
            last_activity: None,
        }
    }
}

/// How many distinct track covers a playlist keeps as mosaic candidates: the
/// 2x2 grid the frontend paints for a playlist with no cover of its own.
pub const COVER_URL_CANDIDATES: usize = 4;

/// The first [`COVER_URL_CANDIDATES`] distinct non-empty track covers, in
/// track order.
///
/// Distinctness is the point: a playlist that is one album top to bottom would
/// otherwise mosaic four copies of the same square, which reads as a rendering
/// bug rather than as a cover. Scanning the kept urls beats a set — there are
/// never more than four of them, and the loop stops at the fourth.
pub fn cover_urls_from_tracks(tracks: &[Track]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::with_capacity(COVER_URL_CANDIDATES);
    for track in tracks {
        if track.cover_url.is_empty() || urls.iter().any(|url| url == &track.cover_url) {
            continue;
        }
        urls.push(track.cover_url.clone());
        if urls.len() == COVER_URL_CANDIDATES {
            break;
        }
    }
    urls
}

/// Makes `artist_ids` the same length as `artist_names`, padding with empty
/// ids or dropping any excess.
///
/// This exists for one real case: `playlist_tracks_cache.json` files written
/// before `artist_ids` existed deserialize with names but no ids, and every
/// existing user has one. Without this the two lists would disagree in length
/// for exactly as long as it takes the background refresh to replace them —
/// which is precisely the first paint after an upgrade, when the user is most
/// likely to click. Padding keeps "same length, same order" true
/// unconditionally, so the frontend can zip without a guard, and the padded
/// entries are empty ids, which already means "not linkable".
pub fn align_artist_ids(track: &mut Track) {
    if track.artist_ids.len() != track.artist_names.len() {
        track.artist_ids.resize(track.artist_names.len(), String::new());
    }
}

/// A playlist opened for browsing: playlist metadata plus its tracks.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistDetail {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

impl From<PlaylistBrowse> for PlaylistDetail {
    fn from(browse: PlaylistBrowse) -> Self {
        let revision = browse.revision.unwrap_or_default();
        let tracks: Vec<Track> = browse.tracks.into_iter().map(Track::from).collect();
        Self {
            playlist: Playlist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                description: String::new(),
                owner: if browse.owner_name.is_empty() {
                    browse.owner_id.clone()
                } else {
                    browse.owner_name
                },
                owner_id: browse.owner_id,
                cover_url: browse.cover_url.unwrap_or_default(),
                // Derived here rather than at the call sites so the candidates
                // and the revision they came from are always written as a pair:
                // a revision bump can replace exactly the tracks they were
                // taken from, so candidates outliving their revision are stale.
                cover_urls: cover_urls_from_tracks(&tracks),
                collaborative: false,
                tracks_total: tracks.len() as u32,
                snapshot_id: revision,
                // A browse cannot know this; the library entry keeps it (see
                // `upsert_playlist`).
                last_played: None,
                last_activity: None,
            },
            tracks,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RadioDetail {
    pub seed: Track,
    pub tracks: Vec<Track>,
    pub seed_kind: String,
    pub seed_artist: Option<Artist>,
    pub cover_url: Option<String>,
}

impl From<RadioBrowse> for RadioDetail {
    fn from(browse: RadioBrowse) -> Self {
        Self {
            seed: browse.seed.into(),
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
            seed_kind: browse.seed_kind,
            seed_artist: browse.seed_artist.map(Artist::from),
            cover_url: browse.cover_url,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistRecommendationsDetail {
    pub playlist_id: String,
    pub tracks: Vec<Track>,
}

impl From<PlaylistRecommendations> for PlaylistRecommendationsDetail {
    fn from(recommendations: PlaylistRecommendations) -> Self {
        Self {
            playlist_id: recommendations.playlist_id,
            tracks: recommendations.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artist_names: Vec<String>,
    /// Artist ids parallel to `artist_names`; empty or missing entries are
    /// rendered as plain text rather than navigable links.
    #[serde(default)]
    pub artist_ids: Vec<String>,
    pub cover_url: String,
    /// Release year, or `null` when the release carries no date. An absent
    /// string field is the empty string by this contract's convention, but a
    /// year has no such spelling — 0 would be a date, and a wrong one — so
    /// this one stays an option.
    pub year: Option<u32>,
}

/// Keeps an album's artist ids index-aligned with its names. Album payloads
/// are not currently disk-cached, but applying the same repair as tracks makes
/// older or hand-authored payloads safe to render without false links.
pub fn align_album_artist_ids(album: &mut Album) {
    if album.artist_ids.len() != album.artist_names.len() {
        album
            .artist_ids
            .resize(album.artist_names.len(), String::new());
    }
}

impl From<AlbumRef> for Album {
    fn from(reference: AlbumRef) -> Self {
        let mut album = Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            artist_names: reference.artist_names,
            artist_ids: reference.artist_ids,
            cover_url: reference.cover_url.unwrap_or_default(),
            year: reference.year,
        };
        align_album_artist_ids(&mut album);
        album
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: Album,
    pub tracks: Vec<Track>,
}

impl From<AlbumBrowse> for AlbumDetail {
    fn from(browse: AlbumBrowse) -> Self {
        let mut album = Album {
            id: browse.id,
            uri: browse.uri,
            name: browse.name,
            artist_names: browse.artist_names,
            artist_ids: browse.artist_ids,
            cover_url: browse.cover_url.unwrap_or_default(),
            year: browse.year,
        };
        align_album_artist_ids(&mut album);
        Self {
            album,
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Artist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub cover_url: String,
}

impl From<ArtistRef> for Artist {
    fn from(reference: ArtistRef) -> Self {
        Self {
            id: reference.id,
            uri: reference.uri,
            name: reference.name,
            cover_url: reference.portrait_url.unwrap_or_default(),
        }
    }
}

/// An artist's catalogue grouped by release type, mirroring the engine's
/// `ArtistReleases`. A group may be shorter than the artist's true catalogue
/// when the engine's per-browse resolution budget bit; `appears_on` is the
/// one that hits it in practice.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistReleaseGroups {
    pub albums: Vec<Album>,
    pub singles: Vec<Album>,
    pub compilations: Vec<Album>,
    pub appears_on: Vec<Album>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistReleaseTotals {
    pub albums: usize,
    pub singles: usize,
    pub compilations: usize,
    pub appears_on: usize,
}

impl From<ArtistReleaseCounts> for ArtistReleaseTotals {
    fn from(counts: ArtistReleaseCounts) -> Self {
        Self {
            albums: counts.albums,
            singles: counts.singles,
            compilations: counts.compilations,
            appears_on: counts.appears_on,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistReleasePageDetail {
    pub releases: ArtistReleaseGroups,
    pub total: usize,
    pub next_offset: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistCataloguePageDetail {
    pub releases: Vec<AlbumDetail>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

impl From<ArtistCataloguePage> for ArtistCataloguePageDetail {
    fn from(page: ArtistCataloguePage) -> Self {
        Self {
            releases: page.releases.into_iter().map(AlbumDetail::from).collect(),
            total: page.total,
            next_offset: page.next_offset,
        }
    }
}

impl From<ArtistReleasePage> for ArtistReleasePageDetail {
    fn from(page: ArtistReleasePage) -> Self {
        Self {
            releases: page.releases.into(),
            total: page.total,
            next_offset: page.next_offset,
        }
    }
}

impl From<ArtistReleases> for ArtistReleaseGroups {
    fn from(releases: ArtistReleases) -> Self {
        let convert = |group: Vec<AlbumRef>| group.into_iter().map(Album::from).collect();
        Self {
            albums: convert(releases.albums),
            singles: convert(releases.singles),
            compilations: convert(releases.compilations),
            appears_on: convert(releases.appears_on),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistTopCityDetail {
    pub city: String,
    pub country: String,
    pub region: String,
    pub listeners: Option<u64>,
}

impl From<ArtistTopCity> for ArtistTopCityDetail {
    fn from(city: ArtistTopCity) -> Self {
        Self {
            city: city.city,
            country: city.country,
            region: city.region,
            listeners: city.listeners,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistOverviewDetail {
    pub biography: Option<String>,
    pub header_image_url: Option<String>,
    pub biography_image_url: Option<String>,
    pub popularity: Option<u32>,
    pub followers: Option<u64>,
    pub monthly_listeners: Option<u64>,
    pub world_rank: Option<u32>,
    pub top_cities: Vec<ArtistTopCityDetail>,
    pub popular_releases: Vec<Album>,
    pub related_artists: Vec<Artist>,
    pub discovered_on: Vec<Playlist>,
    pub artist_playlists: Vec<Playlist>,
    pub artist_pick: Option<Playlist>,
}

impl From<ArtistOverview> for ArtistOverviewDetail {
    fn from(overview: ArtistOverview) -> Self {
        Self {
            biography: overview.biography,
            header_image_url: overview.header_image_url,
            biography_image_url: overview.biography_image_url,
            popularity: overview.popularity,
            followers: overview.followers,
            monthly_listeners: overview.monthly_listeners,
            world_rank: overview.world_rank,
            top_cities: overview
                .top_cities
                .into_iter()
                .map(ArtistTopCityDetail::from)
                .collect(),
            popular_releases: overview
                .popular_releases
                .into_iter()
                .map(Album::from)
                .collect(),
            related_artists: overview
                .related_artists
                .into_iter()
                .map(Artist::from)
                .collect(),
            discovered_on: overview
                .discovered_on
                .iter()
                .map(Playlist::from)
                .collect(),
            artist_playlists: overview
                .artist_playlists
                .iter()
                .map(Playlist::from)
                .collect(),
            artist_pick: overview.artist_pick.as_ref().map(Playlist::from),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtistDetail {
    #[serde(flatten)]
    pub artist: Artist,
    pub top_tracks: Vec<Track>,
    pub releases: ArtistReleaseGroups,
    pub release_counts: ArtistReleaseTotals,
    pub releases_next_offset: Option<usize>,
    pub overview: Option<ArtistOverviewDetail>,
}

impl From<ArtistBrowse> for ArtistDetail {
    fn from(browse: ArtistBrowse) -> Self {
        Self {
            artist: Artist {
                id: browse.id,
                uri: browse.uri,
                name: browse.name,
                cover_url: browse.portrait_url.unwrap_or_default(),
            },
            top_tracks: browse.top_tracks.into_iter().map(Track::from).collect(),
            releases: browse.releases.into(),
            release_counts: browse.release_counts.into(),
            releases_next_offset: browse.releases_next_offset,
            overview: browse.overview.map(ArtistOverviewDetail::from),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchResult {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

impl From<SearchBrowse> for SearchResult {
    fn from(browse: SearchBrowse) -> Self {
        Self {
            tracks: browse.tracks.into_iter().map(Track::from).collect(),
            albums: browse.albums.into_iter().map(Album::from).collect(),
            artists: browse.artists.into_iter().map(Artist::from).collect(),
            playlists: browse.playlists.iter().map(Playlist::from).collect(),
        }
    }
}


/// One contributor in a track's credits.
///
/// `id` is the validated Spotify artist id from the source credit URI. The
/// frontend uses it only to open the external
/// `https://artists.spotify.com/songwriter/{id}` page; it must never route to
/// the in-app artist view. An empty id means the contributor remains plain
/// text. `subroles` are the service's own labels (`composer`, `lyricist`,
/// `producer`, `main artist`, ...), possibly empty.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Contributor {
    pub id: String,
    pub uri: String,
    /// External page for this contributor, supplied finished by the service.
    /// The frontend opens it verbatim and never builds one; empty means the
    /// name renders as plain text.
    pub url: String,
    pub name: String,
    /// The service's own labels, kept verbatim for truthful per-person detail.
    pub subroles: Vec<String>,
}

impl From<CreditArtist> for Contributor {
    fn from(artist: CreditArtist) -> Self {
        Self {
            id: artist.id,
            uri: artist.uri,
            url: artist.url,
            name: artist.name,
            subroles: artist.subroles,
        }
    }
}
/// One source-provided role group of a track's credits, e.g. Artist or
/// Composition & Lyrics. The title is passed through exactly as the service
/// called it; an unrecognised group is still worth showing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CreditGroup {
    pub title: String,
    pub contributors: Vec<Contributor>,
}

impl From<CreditRole> for CreditGroup {
    fn from(role: CreditRole) -> Self {
        Self {
            title: role.title,
            contributors: role.artists.into_iter().map(Contributor::from).collect(),
        }
    }
}

/// Songwriter/producer/performer credits for one track, fetched on demand.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrackCreditsDetail {
    pub track_uri: String,
    pub track_name: String,
    pub groups: Vec<CreditGroup>,
    /// The licensor, shown under the groups exactly as the official client
    /// does. Empty when the service does not supply one.
    pub source: String,
}

impl From<TrackCredits> for TrackCreditsDetail {
    fn from(credits: TrackCredits) -> Self {
        Self {
            track_uri: credits.track_uri,
            track_name: credits.track_name,
            groups: credits.roles.into_iter().map(CreditGroup::from).collect(),
            source: credits.source,
        }
    }
}

/// Deserializes a nullable/missing string field into an owned `String`
/// (the engine serializes some optional state fields as `null`).
fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Mirror of the engine's `state` line, projected locally between the
/// engine's 2-second heartbeats.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaybackState {
    pub ready: bool,
    /// One of `authenticating`, `needs_login`, `ready`, `error`.
    pub auth_state: String,
    #[serde(deserialize_with = "string_or_default")]
    pub auth_url: String,
    pub playing: bool,
    #[serde(deserialize_with = "string_or_default")]
    pub username: String,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    /// One of `off`, `context`, `track`.
    pub repeat: String,
    pub current_index: Option<usize>,
    #[serde(deserialize_with = "string_or_default")]
    pub current_uri: String,
    pub queue: Vec<Track>,
    #[serde(deserialize_with = "string_or_default")]
    pub error: String,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            ready: false,
            auth_state: "needs_login".to_owned(),
            auth_url: String::new(),
            playing: false,
            username: String::new(),
            position_ms: 0,
            duration_ms: 0,
            // The engine starts at 50% volume; mirror it so the first paint
            // matches the first heartbeat.
            volume: 50,
            shuffle: false,
            repeat: "off".to_owned(),
            current_index: None,
            current_uri: String::new(),
            queue: Vec::new(),
            error: String::new(),
        }
    }
}

/// Full snapshot served to the frontend for the initial render.
#[derive(Clone, Debug, Serialize)]
pub struct AppState {
    pub playback: PlaybackState,
    pub playlists: Vec<Playlist>,
    pub me_id: String,
}

/// What one cache directory is costing on disk.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheUsage {
    pub files: u64,
    pub bytes: u64,
}

/// Disk usage of both on-disk caches, for the Settings page.
///
/// `audio` is librespot's own audio cache under the engine state dir — the
/// songs kept for offline replay — and `covers` is the artwork the shell
/// downloads for `cover://`. They are reported separately because they are
/// bounded separately: the audio cache has a 1 GiB ceiling enforced by
/// librespot, the cover cache has none.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheStats {
    pub audio: CacheUsage,
    pub covers: CacheUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(cover_url: &str) -> Track {
        Track {
            cover_url: cover_url.to_owned(),
            ..Track::default()
        }
    }

    fn browse(revision: &str, covers: &[&str]) -> PlaylistBrowse {
        PlaylistBrowse {
            id: "p1".to_owned(),
            uri: "spotify:playlist:p1".to_owned(),
            name: "Mixtape".to_owned(),
            revision: Some(revision.to_owned()),
            owner_id: "me".to_owned(),
            owner_name: String::new(),
            cover_url: None,
            tracks: covers.iter().map(|url| track(url).into()).collect(),
        }
    }

    #[test]
    fn cover_candidates_take_the_first_four_distinct_covers() {
        let covers = ["a", "a", "b", "c", "b", "d", "e"];
        let tracks: Vec<Track> = covers.iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn cover_candidates_skip_tracks_without_a_cover() {
        let tracks: Vec<Track> = ["", "a", "", "b"].iter().map(|url| track(url)).collect();
        assert_eq!(cover_urls_from_tracks(&tracks), vec!["a", "b"]);
        // Fewer than four distinct covers is normal (short playlists, one
        // album); the frontend renders whatever it is given.
        assert!(cover_urls_from_tracks(&[]).is_empty());
        assert!(cover_urls_from_tracks(&[track("")]).is_empty());
    }

    #[test]
    fn browsing_derives_the_candidates_alongside_the_revision() {
        let detail = PlaylistDetail::from(browse("rev-a", &["a", "b"]));
        assert_eq!(detail.playlist.cover_urls, vec!["a", "b"]);
        assert_eq!(detail.playlist.snapshot_id, "rev-a");
        assert_eq!(detail.playlist.tracks_total, 2);

        // A revision bump can replace exactly the tracks the old candidates
        // came from, so a browse never reuses them.
        let next = PlaylistDetail::from(browse("rev-b", &["c", "d"]));
        assert_eq!(next.playlist.cover_urls, vec!["c", "d"]);
        assert_eq!(next.playlist.snapshot_id, "rev-b");
    }

    #[test]
    fn artist_ids_stay_parallel_to_artist_names() {
        // A track cached before the field existed: names but no ids.
        let mut legacy: Track = serde_json::from_str(
            r#"{"id":"t1","name":"Track","artist_names":["A","B","C"],"artist_id":"a1"}"#,
        )
        .unwrap();
        assert!(legacy.artist_ids.is_empty(), "old caches carry no ids");
        align_artist_ids(&mut legacy);
        assert_eq!(
            legacy.artist_ids,
            vec!["", "", ""],
            "padded so the two lists zip, with every name unlinkable"
        );
        assert_eq!(legacy.artist_id, "a1", "the primary is untouched");

        // A track from the engine already matches and is left alone.
        let mut fresh = Track {
            artist_names: vec!["A".into(), "B".into()],
            artist_ids: vec!["a1".into(), "b1".into()],
            ..Track::default()
        };
        align_artist_ids(&mut fresh);
        assert_eq!(fresh.artist_ids, vec!["a1", "b1"]);

        // Excess ids (a hand-edited or future-written cache) are trimmed
        // rather than left to mispair with the names.
        let mut extra = Track {
            artist_names: vec!["A".into()],
            artist_ids: vec!["a1".into(), "stale".into()],
            ..Track::default()
        };
        align_artist_ids(&mut extra);
        assert_eq!(extra.artist_ids, vec!["a1"]);
    }

    #[test]
    fn artist_ids_survive_the_round_trip_through_the_engine_type() {
        let track = Track {
            id: "t1".into(),
            artist_names: vec!["A".into(), "B".into()],
            artist_ids: vec!["a1".into(), "b1".into()],
            artist_id: "a1".into(),
            added_at: Some(1_725_000_123_456),
            ..Track::default()
        };
        let reference: TrackRef = track.clone().into();
        assert_eq!(reference.artist_ids, vec!["a1", "b1"]);
        assert_eq!(reference.added_at, Some(1_725_000_123_456));
        assert_eq!(Track::from(&reference), track);
        assert_eq!(Track::from(reference), track);
    }

    #[test]
    fn a_missing_cover_urls_field_deserializes_to_no_candidates() {
        // Caches written before the field existed must still load.
        let playlist: Playlist =
            serde_json::from_str(r#"{"id":"p1","name":"Mixtape","cover_url":""}"#).unwrap();
        assert_eq!(playlist.id, "p1");
        assert!(playlist.cover_urls.is_empty());
    }

    #[test]
    fn search_result_maps_playlist_references_and_defaults_legacy_payloads() {
        let result = SearchResult::from(SearchBrowse {
            playlists: vec![PlaylistRef {
                id: "0123456789ABCDEFGHIJKL".into(),
                uri: "spotify:playlist:0123456789ABCDEFGHIJKL".into(),
                name: "Public Mix".into(),
                description: Some("A public playlist.".into()),
                owner_id: "alice".into(),
                owner_name: "Alice Example".into(),
                cover_url: Some("https://i.scdn.co/image/cover".into()),
                track_count: Some(42),
            }],
            ..SearchBrowse::default()
        });
        assert_eq!(
            result.playlists,
            vec![Playlist {
                id: "0123456789ABCDEFGHIJKL".into(),
                uri: "spotify:playlist:0123456789ABCDEFGHIJKL".into(),
                name: "Public Mix".into(),
                description: "A public playlist.".into(),
                owner: "Alice Example".into(),
                owner_id: "alice".into(),
                cover_url: "https://i.scdn.co/image/cover".into(),
                tracks_total: 42,
                ..Playlist::default()
            }]
        );

        let legacy: SearchResult =
            serde_json::from_str(r#"{"tracks":[],"albums":[],"artists":[]}"#).unwrap();
        assert!(legacy.playlists.is_empty());
    }

    #[test]
    fn artist_overview_survives_the_engine_to_frontend_conversion() {
        let overview = ArtistOverview {
            biography: Some("Biography".into()),
            header_image_url: Some("header".into()),
            biography_image_url: Some("biography-image".into()),
            popularity: Some(77),
            followers: Some(1_200),
            monthly_listeners: Some(3_400),
            world_rank: Some(42),
            top_cities: vec![ArtistTopCity {
                city: "London".into(),
                country: "GB".into(),
                region: "England".into(),
                listeners: Some(900),
            }],
            popular_releases: vec![AlbumRef {
                id: "album".into(),
                uri: "spotify:album:album".into(),
                name: "Popular".into(),
                artist_names: vec!["Artist".into()],
                artist_ids: vec!["artist".into()],
                cover_url: Some("cover".into()),
                year: Some(2024),
            }],
            related_artists: vec![ArtistRef {
                id: "related".into(),
                uri: "spotify:artist:related".into(),
                name: "Related".into(),
                portrait_url: Some("portrait".into()),
            }],
            discovered_on: Vec::new(),
            artist_playlists: Vec::new(),
            artist_pick: None,
        };

        let frontend = ArtistOverviewDetail::from(overview);
        assert_eq!(frontend.biography.as_deref(), Some("Biography"));
        assert_eq!(frontend.header_image_url.as_deref(), Some("header"));
        assert_eq!(
            frontend.biography_image_url.as_deref(),
            Some("biography-image")
        );
        assert_eq!(frontend.top_cities[0].listeners, Some(900));
        assert_eq!(frontend.popular_releases[0].artist_ids, vec!["artist"]);
        assert_eq!(frontend.related_artists[0].cover_url, "portrait");
        let json = serde_json::to_value(frontend).unwrap();
        assert_eq!(json["monthly_listeners"], 3_400);
        assert_eq!(json["popular_releases"][0]["name"], "Popular");
        assert_eq!(json["related_artists"][0]["name"], "Related");
    }
}
