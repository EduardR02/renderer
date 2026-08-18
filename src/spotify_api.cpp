#include "spotify_api.h"

#include <windows.h>
#include <algorithm>
#include <cmath>
#include <chrono>
#include <nlohmann/json.hpp>
#include <random>
#include <vector>

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

constexpr const char* kMonthNames[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                       "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};

int MonthFromName(const std::string& name) {
  for (int m = 0; m < 12; ++m)
    if (name == kMonthNames[m]) return m + 1;
  return 0;
}

// Howard Hinnant's days-from-civil algorithm: days since 1970-01-01.
int64_t DaysFromCivil(int64_t year, unsigned month, unsigned day) {
  year -= month <= 2;
  const int64_t era = (year >= 0 ? year : year - 399) / 400;
  const unsigned yoe = static_cast<unsigned>(year - era * 400);
  const unsigned doy =
      (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1;
  const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  return era * 146097 + static_cast<int64_t>(doe) - 719468;
}

// Tokenizes an HTTP-date into whitespace/comma/hyphen-separated fields and
// converts the recognized RFC 7231 shapes (IMF-fixdate, RFC 850, asctime) to
// unix seconds. Returns 0 when nothing matches.
int64_t HttpDateToUnixSeconds(const std::string& text) {
  std::string normalized;
  for (char c : text)
    normalized.push_back((c == ',' || c == '-' || c == ' ') ? ' ' : c);
  std::vector<std::string> tokens;
  std::string current;
  for (char c : normalized) {
    if (c == ' ') {
      if (!current.empty()) {
        tokens.push_back(current);
        current.clear();
      }
    } else {
      current.push_back(c);
    }
  }
  if (!current.empty()) tokens.push_back(current);
  if (tokens.size() < 5) return 0;

  // Shapes: IMF-fixdate "Sun, 06 Nov 1994 08:49:37 GMT"; RFC 850
  // "Sunday, 06-Nov-94 08:49:37 GMT" (both: weekday + 5 fields); asctime
  // "Sun Nov  6 08:49:37 1994" (weekday + 4 fields, month before day).
  std::string day, month, year, time;
  if (MonthFromName(tokens[1]) != 0) {
    month = tokens[1];
    day = tokens[2];
    time = tokens[3];
    year = tokens[4];
  } else {
    if (tokens.size() < 6) return 0;
    day = tokens[1];
    month = tokens[2];
    year = tokens[3];
    time = tokens[4];
  }
  if (MonthFromName(month) == 0 || day.empty() || time.empty() ||
      year.empty() || day.find_first_not_of("0123456789") != std::string::npos ||
      year.find_first_not_of("0123456789") != std::string::npos)
    return 0;

  int64_t parsedYear = atoll(year.c_str());
  if (parsedYear < 100) parsedYear += parsedYear >= 70 ? 1900 : 2000;  // RFC 850
  if (parsedYear < 1900 || parsedYear > 2100) return 0;

  const int dom = atoi(day.c_str());
  if (dom < 1 || dom > 31) return 0;

  size_t firstColon = time.find(':');
  size_t secondColon = time.find(':', firstColon + 1);
  if (firstColon == std::string::npos || secondColon == std::string::npos)
    return 0;
  const int hour = atoi(time.substr(0, firstColon).c_str());
  const int minute =
      atoi(time.substr(firstColon + 1, secondColon - firstColon - 1).c_str());
  const int second = atoi(time.substr(secondColon + 1).c_str());
  if (hour < 0 || hour > 23 || minute < 0 || minute > 59 || second < 0 ||
      second > 60)
    return 0;

  return DaysFromCivil(parsedYear, static_cast<unsigned>(MonthFromName(month)),
                       static_cast<unsigned>(dom)) *
             86400 +
         hour * 3600 + minute * 60 + second;
}

}  // namespace

int64_t ParseRetryAfterSeconds(const std::string& header, int64_t nowUnixSeconds) {
  std::string value = Trim(header);
  if (value.empty()) return 0;
  bool numeric = true;
  for (char c : value) {
    if (c < '0' || c > '9') {
      numeric = false;
      break;
    }
  }
  if (numeric) return std::max<int64_t>(0, atoll(value.c_str()));
  // Absolute HTTP-date: the wait is the remaining time until that moment.
  const int64_t when = HttpDateToUnixSeconds(value);
  if (when <= 0) return 0;
  return std::max<int64_t>(0, when - nowUnixSeconds);
}

double RandomUnit() {
  static thread_local std::mt19937_64 generator(std::random_device{}());
  std::uniform_real_distribution<double> distribution(0.0, 1.0);
  return distribution(generator);
}

int64_t ComputeBackoffDelay(int attempt, int retryAfterSeconds,
                            const std::function<double()>& rng) {
  const int clampedAttempt = std::max(0, attempt);
  const double base = static_cast<double>(
      std::min<int64_t>(int64_t{1} << std::min(clampedAttempt, 6), 60));
  const double jitter = (rng ? rng() : RandomUnit()) * base / 2.0;
  const double jittered = std::ceil(base / 2.0 + jitter);  // [base/2, base]
  const double wait = std::max(jittered, static_cast<double>(std::max(0, retryAfterSeconds)));
  return static_cast<int64_t>(std::min(wait, 300.0));
}

WebApiTokenProvider::WebApiTokenProvider(RequestFn request, ClockFn clock)
    : request_(std::move(request)), clock_(std::move(clock)) {}

std::string WebApiTokenProvider::GetAccessToken() {
  // The mutex is held across the whole engine round-trip, so a caller that
  // races an in-flight mint blocks here and then reuses the minted token
  // instead of minting again: one mint serves every concurrent caller.
  std::lock_guard<std::mutex> lock(mutex_);
  const int64_t now = clock_();
  if (!token_ || now >= expires_at_) {
    WebApiToken fresh;
    std::string error;
    if (!request_(&fresh, &error, 20000) || fresh.access_token.empty() ||
        fresh.expires_in <= 0)
      throw std::runtime_error(error.empty()
                                   ? "could not mint a Spotify Web API token"
                                   : error);
    token_ = std::move(fresh);
    // expires_in already includes the engine's safety skew; the token is
    // reused until it actually expires (no per-request minting).
    expires_at_ = now + token_->expires_in;
  }
  return token_->access_token;
}

bool WebApiTokenProvider::Refresh(int timeoutMs) {
  std::lock_guard<std::mutex> lock(mutex_);
  token_.reset();
  WebApiToken fresh;
  std::string error;
  if (!request_(&fresh, &error, timeoutMs) || fresh.access_token.empty() ||
      fresh.expires_in <= 0)
    return false;
  expires_at_ = clock_() + fresh.expires_in;
  token_ = std::move(fresh);
  return true;
}

SpotifyApi::SpotifyApi(std::shared_ptr<HttpClient> api,
                       std::function<std::string()> getToken,
                       std::function<bool(int)> refreshToken)
    : api_(std::move(api)),
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
  if (r.status == 429 || r.status == 503) {
    const int retryAfter = static_cast<int>(std::clamp<int64_t>(
        ParseRetryAfterSeconds(r.retry_after, NowUnixSeconds()), 0, 3600));
    const std::string detail =
        r.status == 429 ? "rate limited" : "service unavailable";
    throw ApiError(r.status, retryAfter,
                   what + ": " + detail +
                       (r.retry_after.empty() ? "" : " (Retry-After " + r.retry_after + ")"));
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

std::vector<TrackRef> SpotifyApi::GetArtistTopTracks(const std::string& artistId) {
  HttpResponse r = Authed(
      "GET", AppendQuery("/v1/artists/" + UrlEncode(artistId) + "/top-tracks",
                         "market=from_token"));
  EnsureOk(r, "artist top tracks");
  std::vector<TrackRef> out;
  try {
    json j = json::parse(r.body);
    if (const json* tracks = JGet(j, "tracks")) {
      for (const auto& t : *tracks) out.push_back(ParseTrack(t));
    }
  } catch (...) {
    throw ApiError(0, 0, "artist top tracks: malformed response");
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

PlaylistsPage SpotifyApi::GetMyPlaylistsPage(const std::string& nextPath) {
  // Exactly one page per call: pagination is owned by the caller so the
  // startup fetch never bursts multiple /me/playlists requests back-to-back.
  const std::string path =
      nextPath.empty() ? AppendQuery("/v1/me/playlists", "limit=50") : nextPath;
  HttpResponse r = Authed("GET", path);
  EnsureOk(r, "playlists");
  PlaylistsPage out;
  try {
    json j = json::parse(r.body);
    out.total = JInt(&j, "total");
    if (const json* items = JGet(j, "items")) {
      for (const auto& p : *items) {
        PlaylistRef pl;
        pl.id = JStr(&p, "id");
        pl.uri = JStr(&p, "uri");
        pl.name = JStr(&p, "name");
        const json* owner = JGet(p, "owner");
        if (owner) {
          pl.owner = JStr(owner, "display_name");
          pl.owner_id = JStr(owner, "id");
        }
        const json* images = JGet(p, "images");
        if (images && images->is_array() && !images->empty())
          pl.cover_url = JStr(&images->front(), "url");
        pl.collaborative = JBool(&p, "collaborative");
        const json* tracks = JGet(p, "tracks");
        if (tracks) pl.tracks_total = JInt(tracks, "total");
        pl.snapshot_id = JStr(&p, "snapshot_id");
        out.items.push_back(std::move(pl));
      }
    }
    const json* next = JGet(j, "next");
    if (next && next->is_string() && !next->get<std::string>().empty()) {
      const std::string nextUrl = next->get<std::string>();
      const size_t apiPos = nextUrl.find("/v1/");
      if (apiPos != std::string::npos) out.next_path = nextUrl.substr(apiPos);
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
