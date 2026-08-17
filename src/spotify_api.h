#pragma once
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

#include "http.h"

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
  std::string owner, cover_url;
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

// Spotify Web API client. Auth is injected: the access token comes from
// getToken(); on 401 the refreshToken() callback is invoked once and the
// request retried. Throws ApiError on failure; rate limits (429) surface as
// ApiError with retry_after set.
class SpotifyApi {
 public:
  SpotifyApi(std::shared_ptr<HttpClient> api, std::shared_ptr<HttpClient> accounts,
             std::function<std::string()> getToken,
             std::function<bool(int)> refreshToken);

  // Browsing / search
  SearchResult Search(const std::string& query, int limit = 10);
  std::vector<AlbumRef> GetArtistAlbums(const std::string& artistId);
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
  std::shared_ptr<HttpClient> accounts_;
  std::function<std::string()> getToken_;
  std::function<bool(int)> refreshToken_;
};

}  // namespace sr
