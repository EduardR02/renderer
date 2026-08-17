#include "spotify_api.h"

#include <windows.h>
#include <algorithm>
#include <chrono>
#include <nlohmann/json.hpp>

#include "util.h"

namespace sr {

using nlohmann::json;

namespace {

// Safe JSON accessors that treat missing fields as absent.
const json* JGet(const json& j, const char* key) {
  if (!j.is_object()) return nullptr;
  auto it = j.find(key);
  return it == j.end() ? nullptr : &*it;
}

std::string JStr(const json* j, const char* key, const std::string& dflt = {}) {
  if (!j) return dflt;
  const json* v = JGet(*j, key);
  if (!v || !v->is_string()) return dflt;
  return v->get<std::string>();
}

int JInt(const json* j, const char* key, int dflt = 0) {
  if (!j) return dflt;
  const json* v = JGet(*j, key);
  if (!v || !v->is_number_integer()) return dflt;
  return v->get<int>();
}

bool JBool(const json* j, const char* key, bool dflt = false) {
  if (!j) return dflt;
  const json* v = JGet(*j, key);
  if (!v || !v->is_boolean()) return dflt;
  return v->get<bool>();
}

std::string CoverUrl(const json* j) {
  if (!j) return {};
  const json* imgs = JGet(*j, "images");
  if (!imgs || !imgs->is_array() || imgs->empty()) return {};
  return JStr(&(*imgs)[0], "url");
}

std::string FirstArtistId(const json* artists) {
  if (artists && artists->is_array() && !artists->empty()) return JStr(&(*artists)[0], "id");
  return {};
}

TrackRef ParseTrack(const json& j) {
  TrackRef t;
  t.id = JStr(&j, "id");
  t.uri = JStr(&j, "uri");
  t.name = JStr(&j, "name");
  const json* artists = JGet(j, "artists");
  if (artists && artists->is_array()) {
    for (const auto& a : *artists) t.artist_names.push_back(JStr(&a, "name"));
  }
  t.artist_id = FirstArtistId(artists);
  const json* album = JGet(j, "album");
  if (album) {
    t.album_id = JStr(album, "id");
    t.album_name = JStr(album, "name");
    t.cover_url = CoverUrl(album);
  }
  t.duration_ms = JInt(&j, "duration_ms");
  return t;
}

AlbumRef ParseAlbum(const json& j) {
  AlbumRef a;
  a.id = JStr(&j, "id");
  a.uri = JStr(&j, "uri");
  a.name = JStr(&j, "name");
  const json* artists = JGet(j, "artists");
  if (artists && artists->is_array()) {
    for (const auto& x : *artists) a.artist_names.push_back(JStr(&x, "name"));
  }
  a.cover_url = CoverUrl(&j);
  return a;
}

ArtistRef ParseArtist(const json& j) {
  ArtistRef a;
  a.id = JStr(&j, "id");
  a.uri = JStr(&j, "uri");
  a.name = JStr(&j, "name");
  a.cover_url = CoverUrl(&j);
  return a;
}

std::string QueryStr(const std::string& key, const std::string& value) {
  return key + "=" + UrlEncode(value);
}

std::string AppendQuery(const std::string& path, const std::string& q) {
  return q.empty() ? path : (path + (path.find('?') == std::string::npos ? "?" : "&") + q);
}

int ParseRetryAfter(const std::string& s) {
  if (s.empty()) return 0;
  int v = atoi(s.c_str());
  if (v < 1) return 1;
  if (v > 3600) return 3600;
  return v;
}

}  // namespace

SpotifyApi::SpotifyApi(std::shared_ptr<HttpClient> api, std::shared_ptr<HttpClient> accounts,
                       std::function<std::string()> getToken,
                       std::function<bool(int)> refreshToken)
    : api_(std::move(api)),
      accounts_(std::move(accounts)),
      getToken_(std::move(getToken)),
      refreshToken_(std::move(refreshToken)) {}

HttpResponse SpotifyApi::Authed(const std::string& method, const std::string& path,
                                const std::string& body, const std::string& contentType,
                                int timeoutMs) {
  using Clock = std::chrono::steady_clock;
  const auto deadline = Clock::now() + std::chrono::milliseconds(std::max(0, timeoutMs));
  auto remaining = [&]() {
    return static_cast<int>(
        std::chrono::duration_cast<std::chrono::milliseconds>(deadline - Clock::now()).count());
  };
  auto timedOut = [] {
    HttpResponse response;
    response.error = "request timed out";
    return response;
  };

  std::string token = getToken_();
  for (int attempt = 0; attempt < 2; ++attempt) {
    const int requestTimeout = remaining();
    if (requestTimeout <= 0) return timedOut();
    std::vector<Header> headers = {{"Authorization", "Bearer " + token},
                                   {"Accept", "application/json"}};
    if (!contentType.empty()) headers.push_back({"Content-Type", contentType});
    HttpResponse r = api_->Send(method, path, body, headers, requestTimeout);
    if (r.status == 401 && attempt == 0) {
      // Token expired or revoked: refresh once, then retry within the original
      // request deadline.
      const int refreshTimeout = remaining();
      if (refreshTimeout <= 0) return timedOut();
      if (refreshToken_(refreshTimeout)) {
        token = getToken_();
        continue;
      }
    }
    return r;
  }
  HttpResponse r;
  r.status = 401;
  r.succeeded = true;
  r.error = "unauthorized";
  return r;
}

void SpotifyApi::EnsureOk(const HttpResponse& r, const std::string& what) {
  if (!r.succeeded) throw ApiError(0, 0, what + ": " + r.error);
  if (r.status == 204) return;
  if (r.status == 429) {
    throw ApiError(429, ParseRetryAfter(r.retry_after),
                   what + ": rate limited" +
                       (r.retry_after.empty() ? "" : " (Retry-After " + r.retry_after + "s)"));
  }
  if (r.status >= 400) {
    std::string msg = what + ": HTTP " + std::to_string(r.status);
    try {
      json j = json::parse(r.body);
      if (j.is_object()) {
        const json* e = JGet(j, "error");
        if (e && e->is_object()) msg += " - " + JStr(e, "message", "unknown error");
        else if (e && e->is_string()) msg += " - " + e->get<std::string>();
      }
    } catch (...) {
    }
    throw ApiError((int)r.status, 0, msg);
  }
}

SearchResult SpotifyApi::Search(const std::string& query, int limit) {
  const int safeLimit = std::clamp(limit, 1, 10);
  std::string q = "q=" + UrlEncode(query) + "&type=track,album,artist&limit=" +
                  std::to_string(safeLimit);
  HttpResponse r = Authed("GET", AppendQuery("/v1/search", q));
  EnsureOk(r, "search");
  SearchResult out;
  try {
    json j = json::parse(r.body);
    if (const json* tracks = JGet(j, "tracks"); tracks) {
      if (const json* items = JGet(*tracks, "items")) {
        for (const auto& t : *items) out.tracks.push_back(ParseTrack(t));
      }
    }
    if (const json* albums = JGet(j, "albums"); albums) {
      if (const json* items = JGet(*albums, "items")) {
        for (const auto& a : *items) out.albums.push_back(ParseAlbum(a));
      }
    }
    if (const json* artists = JGet(j, "artists"); artists) {
      if (const json* items = JGet(*artists, "items")) {
        for (const auto& a : *items) out.artists.push_back(ParseArtist(a));
      }
    }
  } catch (...) {
    throw ApiError(0, 0, "search: malformed response");
  }
  return out;
}

std::vector<AlbumRef> SpotifyApi::GetArtistAlbums(const std::string& artistId) {
  HttpResponse r = Authed(
      "GET", AppendQuery("/v1/artists/" + UrlEncode(artistId) + "/albums",
                         "include_groups=album,single&limit=50"));
  EnsureOk(r, "artist albums");
  std::vector<AlbumRef> out;
  try {
    json j = json::parse(r.body);
    if (const json* items = JGet(j, "items")) {
      for (const auto& a : *items) out.push_back(ParseAlbum(a));
    }
  } catch (...) {
    throw ApiError(0, 0, "artist albums: malformed response");
  }
  return out;
}

std::vector<TrackRef> SpotifyApi::GetAlbumTracks(const std::string& albumId) {
  HttpResponse r = Authed(
      "GET", AppendQuery("/v1/albums/" + UrlEncode(albumId) + "/tracks", "limit=50"));
  EnsureOk(r, "album tracks");
  std::vector<TrackRef> out;
  try {
    json j = json::parse(r.body);
    if (const json* items = JGet(j, "items")) {
      for (const auto& t : *items) out.push_back(ParseTrack(t));
    }
  } catch (...) {
    throw ApiError(0, 0, "album tracks: malformed response");
  }
  return out;
}

std::string SpotifyApi::GetMeId() {
  HttpResponse r = Authed("GET", "/v1/me");
  EnsureOk(r, "me");
  try {
    json j = json::parse(r.body);
    return JStr(&j, "id");
  } catch (...) {
    throw ApiError(0, 0, "me: malformed response");
  }
}

std::vector<PlaylistRef> SpotifyApi::GetMyPlaylists() {
  HttpResponse r = Authed("GET", AppendQuery("/v1/me/playlists", "limit=50"));
  EnsureOk(r, "playlists");
  std::vector<PlaylistRef> out;
  try {
    json j = json::parse(r.body);
    if (const json* items = JGet(j, "items")) {
      for (const auto& p : *items) {
        PlaylistRef pl;
        pl.id = JStr(&p, "id");
        pl.uri = JStr(&p, "uri");
        pl.name = JStr(&p, "name");
        const json* owner = JGet(p, "owner");
        if (owner) pl.owner = JStr(owner, "display_name");
        const json* images = JGet(p, "images");
        if (images && images->is_array() && !images->empty())
          pl.cover_url = JStr(&images->front(), "url");
        pl.collaborative = JBool(&p, "collaborative");
        const json* tracks = JGet(p, "tracks");
        if (tracks) pl.tracks_total = JInt(tracks, "total");
        pl.snapshot_id = JStr(&p, "snapshot_id");
        out.push_back(std::move(pl));
      }
    }
  } catch (...) {
    throw ApiError(0, 0, "playlists: malformed response");
  }
  return out;
}
std::vector<TrackRef> SpotifyApi::GetPlaylistTracks(const std::string& playlistId,
                                                    std::string* snapshotIdOut) {
  HttpResponse r = Authed("GET", AppendQuery("/v1/playlists/" + UrlEncode(playlistId) + "/items",
                                             "limit=50"));
  EnsureOk(r, "playlist items");
  std::vector<TrackRef> out;
  try {
    json j = json::parse(r.body);
    if (const json* items = JGet(j, "items")) {
      for (const auto& entry : *items) {
        const json* item = JGet(entry, "item");
        if (!item) item = JGet(entry, "track");  // tolerate legacy cached responses
        if (!item || item->is_null() || JStr(item, "type", "track") != "track") continue;
        out.push_back(ParseTrack(*item));
      }
    }
  } catch (...) {
    throw ApiError(0, 0, "playlist items: malformed response");
  }
  // Fetch the playlist object for the snapshot_id (needed for edits).
  if (snapshotIdOut) {
    try {
      HttpResponse pr = Authed("GET", "/v1/playlists/" + UrlEncode(playlistId), {}, {}, 15000);
      EnsureOk(pr, "playlist details");
      json j = json::parse(pr.body);
      *snapshotIdOut = JStr(&j, "snapshot_id");
    } catch (...) {
      snapshotIdOut->clear();
    }
  }
  return out;
}

PlaylistRef SpotifyApi::CreatePlaylist(const std::string& meId, const std::string& name) {
  std::string body = json{{"name", name}, {"public", false}}.dump();
  HttpResponse r = Authed("POST", "/v1/users/" + UrlEncode(meId) + "/playlists", body,
                          "application/json");
  EnsureOk(r, "create playlist");
  PlaylistRef pl;
  try {
    json j = json::parse(r.body);
    pl.id = JStr(&j, "id");
    pl.uri = JStr(&j, "uri");
    pl.name = JStr(&j, "name");
    const json* images = JGet(j, "images");
    if (images && images->is_array() && !images->empty())
      pl.cover_url = JStr(&images->front(), "url");
    pl.snapshot_id = JStr(&j, "snapshot_id");
  } catch (...) {
    throw ApiError(0, 0, "create playlist: malformed response");
  }
  return pl;
}

void SpotifyApi::RenamePlaylist(const std::string& playlistId, const std::string& name) {
  std::string body = json{{"name", name}}.dump();
  HttpResponse r = Authed("PUT", "/v1/playlists/" + UrlEncode(playlistId), body,
                          "application/json");
  EnsureOk(r, "rename playlist");
}

void SpotifyApi::DeletePlaylist(const std::string& playlistId) {
  HttpResponse r = Authed("DELETE", "/v1/playlists/" + UrlEncode(playlistId) + "/followers");
  EnsureOk(r, "delete playlist");
}

void SpotifyApi::AddTracksToPlaylist(const std::string& playlistId,
                                     const std::vector<std::string>& uris) {
  json arr = json::array();
  for (const auto& u : uris) arr.push_back(u);
  std::string body = json{{"uris", arr}}.dump();
  HttpResponse r = Authed("POST", "/v1/playlists/" + UrlEncode(playlistId) + "/items", body,
                          "application/json");
  EnsureOk(r, "add to playlist");
}

void SpotifyApi::RemoveTracksFromPlaylist(const std::string& playlistId,
                                          const std::vector<std::string>& uris,
                                          const std::string& snapshotId) {
  json arr = json::array();
  for (const auto& u : uris) {
    arr.push_back({{"uri", u}});
  }
  json body = {{"items", arr}};
  if (!snapshotId.empty()) body["snapshot_id"] = snapshotId;
  HttpResponse r = Authed("DELETE", "/v1/playlists/" + UrlEncode(playlistId) + "/items",
                          body.dump(), "application/json");
  EnsureOk(r, "remove from playlist");
}

void SpotifyApi::ReorderPlaylistTracks(const std::string& playlistId, int rangeStart,
                                       int insertBefore, int rangeLength,
                                       const std::string& snapshotId) {
  json body = {{"range_start", rangeStart},
               {"insert_before", insertBefore},
               {"range_length", rangeLength}};
  if (!snapshotId.empty()) body["snapshot_id"] = snapshotId;
  HttpResponse r = Authed("PUT", "/v1/playlists/" + UrlEncode(playlistId) + "/items",
                          body.dump(), "application/json");
  EnsureOk(r, "reorder playlist");
}




}  // namespace sr
