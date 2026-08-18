#pragma once
#include <string>
#include <vector>

namespace sr {

// Shared Spotify model types. Browsing and editing are served entirely by the
// playback engine's spclient session (PlaybackEngineClient browse_*/edit_*
// commands); the developer Web API (api.spotify.com) is no longer used, so
// these structs only exist as the app-side shape of engine payloads.

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

}  // namespace sr
