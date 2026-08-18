#pragma once
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

#include "http.h"
#include "util.h"

namespace sr {


struct TrackRef {
  std::string id, uri, name;
  std::vector<std::string> artist_names;
  std::string artist_id;
  std::string album_id, album_name;
  std::string cover_url;
  int duration_ms = 0;
};

struct AlbumRef {
  std::string id, uri, name;
  std::vector<std::string> artist_names;
  std::string cover_url;
};

struct ArtistRef {
  std::string id, uri, name;
  std::string cover_url;
};

struct SearchResult {
  std::vector<TrackRef> tracks;
  std::vector<AlbumRef> albums;
  std::vector<ArtistRef> artists;
};

struct PlaylistRef {
  std::string id, uri, name;
  std::string owner, owner_id, cover_url;
  bool collaborative = false;
  int tracks_total = 0;
  std::string snapshot_id;
};


struct ApiError : std::runtime_error {
  int status = 0;
  int retry_after = 0;  // seconds from Retry-After (429), 0 if absent
  ApiError(int st, int retry, const std::string& msg)
      : std::runtime_error(msg), status(st), retry_after(retry) {}
};

// Engine-minted Web API access token (login5). `expires_in` is the remaining
// lifetime in seconds with the engine's safety skew already deducted.
struct WebApiToken {
  std::string token_type;
  std::string access_token;
  int64_t expires_in = 0;
};

// Caches engine-minted Web API tokens and refreshes them by expiry. `request`
// performs one blocking engine round-trip and returns false (with `error`)
// when the engine cannot mint a token. The clock is injectable so expiry
// handling stays testable without the engine or any network.
class WebApiTokenProvider {
 public:
  using RequestFn = std::function<bool(WebApiToken*, std::string*, int)>;
  using ClockFn = std::function<int64_t()>;  // unix seconds

  explicit WebApiTokenProvider(RequestFn request, ClockFn clock = [] {
    return NowUnixSeconds();
  });

  // Returns the current access token, requesting one from the engine when the
  // cached token is missing or expired. Throws std::runtime_error when the
  // engine cannot mint a token.
  std::string GetAccessToken();
  // Discards the cached token and requests a fresh one (401 recovery path).
  // Returns false when the engine cannot mint a token.
  bool Refresh(int timeoutMs = 20000);

 private:
  RequestFn request_;
  ClockFn clock_;
  std::mutex mutex_;
  std::optional<WebApiToken> token_;
  int64_t expires_at_ = 0;  // unix seconds; valid while now < expires_at
};

// Spotify Web API client. Auth is injected: the access token comes from
// getToken(); on 401 the refreshToken() callback is invoked once and the
// request retried. Throws ApiError on failure; rate limits (429) surface as
// ApiError with retry_after set.
class SpotifyApi {
 public:
  SpotifyApi(std::shared_ptr<HttpClient> api, std::function<std::string()> getToken,
             std::function<bool(int)> refreshToken);

  // Browsing / search
  SearchResult Search(const std::string& query, int limit = 10);
  std::vector<AlbumRef> GetArtistAlbums(const std::string& artistId);
  std::vector<TrackRef> GetArtistTopTracks(const std::string& artistId);
  std::vector<TrackRef> GetAlbumTracks(const std::string& albumId);
  std::string GetMeId();

  // Playlists
  std::vector<PlaylistRef> GetMyPlaylists();
  std::vector<TrackRef> GetPlaylistTracks(const std::string& playlistId,
                                          std::string* snapshotIdOut);
  PlaylistRef CreatePlaylist(const std::string& meId, const std::string& name);
  void RenamePlaylist(const std::string& playlistId, const std::string& name);
  void DeletePlaylist(const std::string& playlistId);
  void AddTracksToPlaylist(const std::string& playlistId, const std::vector<std::string>& uris);
  void RemoveTracksFromPlaylist(const std::string& playlistId,
                                const std::vector<std::string>& uris,
                                const std::string& snapshotId);
  void ReorderPlaylistTracks(const std::string& playlistId, int rangeStart, int insertBefore,
                             int rangeLength, const std::string& snapshotId);

 private:
  HttpResponse Authed(const std::string& method, const std::string& path,
                      const std::string& body = {}, const std::string& contentType = {},
                      int timeoutMs = 20000);
  void EnsureOk(const HttpResponse& r, const std::string& what);

  std::shared_ptr<HttpClient> api_;
  std::function<std::string()> getToken_;
  std::function<bool(int)> refreshToken_;
};


}  // namespace sr
