#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest/doctest.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <memory>
#include <stdexcept>

#include <nlohmann/json.hpp>

#include "app.h"
#include "app_paths.h"
#include "playback_engine_client.h"
#include "ui_rows.h"

using nlohmann::json;

namespace sr {
namespace {

TrackRef ExampleTrack() {
  TrackRef track;
  track.id = "track-id";
  track.uri = "spotify:track:abc";
  track.name = "Signal Path";
  track.artist_names = {"First Artist", "Second Artist"};
  track.artist_id = "artist-id";
  track.album_id = "album-id";
  track.album_name = "Local Sessions";
  track.cover_url = "https://i.scdn.co/image/example";
  track.duration_ms = 243001;
  return track;
}

TEST_CASE("TrackRef engine JSON preserves complete metadata") {
  const TrackRef expected = ExampleTrack();
  const json encoded = TrackRefToEngineJson(expected);
  CHECK((encoded == json{{"id", "track-id"},
                         {"uri", "spotify:track:abc"},
                         {"name", "Signal Path"},
                         {"artist_names", {"First Artist", "Second Artist"}},
                         {"artist_id", "artist-id"},
                         {"album_id", "album-id"},
                         {"album_name", "Local Sessions"},
                         {"cover_url", "https://i.scdn.co/image/example"},
                         {"duration_ms", 243001}}));
  const TrackRef decoded = TrackRefFromEngineJson(encoded);
  CHECK(decoded.id == expected.id);
  CHECK(decoded.uri == expected.uri);
  CHECK(decoded.name == expected.name);
  CHECK(decoded.artist_names == expected.artist_names);
  CHECK(decoded.artist_id == expected.artist_id);
  CHECK(decoded.album_id == expected.album_id);
  CHECK(decoded.album_name == expected.album_name);
  CHECK(decoded.cover_url == expected.cover_url);
  CHECK(decoded.duration_ms == expected.duration_ms);
}

TEST_CASE("state event maps playback and queue without a process") {
  const json line = {
      {"type", "state"},
      {"ready", true},
      {"auth_state", "ready"},
      {"playing", true},
      {"position_ms", 500000},
      {"duration_ms", 243001},
      {"volume", 120},
      {"shuffle", true},
      {"repeat", "context"},
      {"current_index", 0},
      {"current_uri", "spotify:track:abc"},
      {"queue", json::array({TrackRefToEngineJson(ExampleTrack())})},
      {"error", nullptr},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  REQUIRE(message.kind == EngineMessage::Kind::State);
  CHECK(message.state.ready);
  CHECK(message.state.auth_state == EngineAuthState::Ready);
  CHECK(message.state.playing);
  CHECK(message.state.position_ms == 243001);
  CHECK(message.state.duration_ms == 243001);
  CHECK(message.state.volume_percent == 100);
  CHECK(message.state.shuffle);
  CHECK(message.state.repeat == "context");
  CHECK(message.state.current_index == 0);
  REQUIRE(message.state.queue.size() == 1);
  CHECK(message.state.queue.front().uri == "spotify:track:abc");
  CHECK(message.state.error.empty());
}

TEST_CASE("invalid state values use safe local bounds") {
  const EngineMessage message = ParseEngineMessage(
      R"({"type":"state","ready":false,"auth_state":"error","playing":false,"position_ms":-4,"duration_ms":-1,"volume":-8,"shuffle":false,"repeat":"unexpected","current_index":8,"current_uri":"","queue":[],"error":"login failed"})");
  CHECK(message.state.auth_state == EngineAuthState::Error);
  CHECK(message.state.position_ms == 0);
  CHECK(message.state.duration_ms == 0);
  CHECK(message.state.volume_percent == 0);
  CHECK(message.state.repeat == "off");
  CHECK(message.state.current_index == -1);
  CHECK(message.state.error == "login failed");
}

TEST_CASE("responses retain request correlation and errors") {
  EngineMessage success = ParseEngineMessage(
      R"({"type":"response","request_id":"41","ok":true})");
  CHECK(success.kind == EngineMessage::Kind::Response);
  CHECK(success.request_id == "41");
  CHECK(success.ok);

  EngineMessage failure = ParseEngineMessage(
      R"({"type":"response","request_id":"42","ok":false,"error":"queue index out of range"})");
  CHECK_FALSE(failure.ok);
  CHECK(failure.error == "queue index out of range");
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"response","ok":true})"),
                  std::invalid_argument);
}

TEST_CASE("play queue request carries full queue and selected position") {
  const json request = BuildEngineRequest(
      "7", "play_queue",
      {{"queue", json::array({TrackRefToEngineJson(ExampleTrack())})},
       {"index", 0},
       {"position_ms", 1250}});
  CHECK(request["request_id"] == "7");
  CHECK(request["type"] == "play_queue");
  CHECK(request["index"] == 0);
  CHECK(request["position_ms"] == 1250);
  CHECK(request["queue"][0]["album_name"] == "Local Sessions");
  CHECK(request["queue"][0]["artist_names"].size() == 2);
}

TEST_CASE("queue mutation requests use stable indices and metadata") {
  const json add = BuildEngineRequest(
      "8", "add_queue", {{"track", TrackRefToEngineJson(ExampleTrack())}});
  const json remove =
      BuildEngineRequest("9", "remove_queue", {{"index", 3}});
  const json move =
      BuildEngineRequest("10", "move_queue", {{"from", 4}, {"to", 1}});
  CHECK(add["track"]["uri"] == "spotify:track:abc");
  CHECK((remove == json{{"index", 3},
                        {"request_id", "9"},
                        {"type", "remove_queue"}}));
  CHECK(move["from"] == 4);
  CHECK(move["to"] == 1);
  CHECK(move["request_id"] == "10");
}

TEST_CASE("playback projection advances while playing and stays static when paused") {
  PositionProjector projector;
  projector.Reset(5'000, true, 1'000);
  CHECK(projector.Current(1'000, 200'000) == 5'000);
  CHECK(projector.Current(3'500, 200'000) == 7'500);
  // Engine pause event: position is pinned to the event value.
  projector.Reset(7'500, false, 3'500);
  CHECK(projector.Current(60'000, 200'000) == 7'500);
  // Resume intent: projection restarts from the frozen position.
  projector.Reset(7'500, true, 60'000);
  CHECK(projector.Current(62'000, 200'000) == 9'500);
}

TEST_CASE("playback projection clamps at track end and holds on unknown duration") {
  PositionProjector projector;
  projector.Reset(199'000, true, 0);
  CHECK(projector.Current(30'000, 200'000) == 200'000);
  // Unknown duration (no track loaded): no projection at all.
  projector.Reset(4'000, true, 0);
  CHECK(projector.Current(10'000, 0) == 4'000);
  // Clock skew must never move the position backwards.
  projector.Reset(4'000, true, 5'000);
  CHECK(projector.Current(4'999, 200'000) == 4'000);
}

TEST_CASE("optimistic control intent reconciles with engine state events") {
  PositionProjector projector;
  // Play intent optimistically starts projection from the paused position.
  projector.Reset(30'000, true, 1'000);
  CHECK(projector.Current(4'000, 300'000) == 33'000);
  // Engine ack event lands slightly later with the authoritative position.
  projector.Reset(31'200, true, 4'000);
  CHECK(projector.Current(4'000, 300'000) == 31'200);
  CHECK(projector.Current(5'000, 300'000) == 32'200);
}

TEST_CASE("seek release re-anchors projection at the committed value") {
  PositionProjector projector;
  projector.Reset(10'000, true, 0);
  // While dragging, the slider shows only the local value; the release commits
  // and re-anchors so projection continues from the dragged position.
  projector.Reset(120'000, true, 9'000);
  CHECK(projector.Current(9'000, 300'000) == 120'000);
  CHECK(projector.Current(11'000, 300'000) == 122'000);
  // A release past the end commits clamped at the track duration.
  projector.Reset(400'000, true, 12'000);
  CHECK(projector.Current(13'000, 240'000) == 240'000);
}

TEST_CASE("unknown protocol messages are rejected") {
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"diagnostic","text":"x"})"),
                  std::invalid_argument);
  CHECK_THROWS(ParseEngineMessage("not json"));
  CHECK_THROWS_AS(BuildEngineRequest("", "play"), std::invalid_argument);
}

TEST_CASE("track list LRU cache evicts least recently used entry at capacity") {
  TrackListCache cache(2, std::chrono::minutes(10));
  CachedTrackList list{std::vector<TrackRef>{ExampleTrack()}, "snap"};
  cache.Put("p:a", list);
  cache.Put("p:b", list);
  cache.Put("p:c", list);  // evicts a (least recently used)
  CHECK(cache.Size() == 2);
  CachedTrackList out;
  CHECK_FALSE(cache.Get("p:a", &out));
  REQUIRE(cache.Get("p:b", &out));
  CHECK(out.snapshot_id == "snap");
  CHECK(out.tracks.size() == 1);
  CHECK(out.tracks.front().id == "track-id");
  // Touch c so b becomes the least recently used entry.
  CHECK(cache.Get("p:c", &out));
  cache.Put("p:d", list);  // evicts b
  CHECK_FALSE(cache.Get("p:b", &out));
  CHECK(cache.Get("p:c", &out));
  CHECK(cache.Get("p:d", &out));
}

TEST_CASE("track list cache entries go stale after their ttl") {
  using Clock = std::chrono::steady_clock;
  auto base = std::make_shared<Clock::time_point>(Clock::now());
  const auto ttl = std::chrono::minutes(10);
  TrackListCache cache(4, ttl, [base] { return *base; });
  CachedTrackList list{std::vector<TrackRef>{ExampleTrack()}, ""};
  cache.Put("p:stale", list);
  CachedTrackList out;
  CHECK(cache.Get("p:stale", &out));  // fresh immediately after fetch
  *base += ttl - std::chrono::seconds(1);
  CHECK(cache.Get("p:stale", &out));  // still fresh just before the ttl
  *base += std::chrono::seconds(2);
  CHECK_FALSE(cache.Get("p:stale", &out));  // stale past the ttl
  CHECK(cache.Size() == 0);  // stale entry evicted on access
}

TEST_CASE("track list cache invalidate and clear drop entries") {
  TrackListCache cache(4, std::chrono::minutes(10));
  CachedTrackList list{std::vector<TrackRef>{ExampleTrack()}, ""};
  cache.Put("p:a", list);
  cache.Put("a:b", list);
  cache.Invalidate("p:a");
  CachedTrackList out;
  CHECK_FALSE(cache.Get("p:a", &out));
  CHECK(cache.Get("a:b", &out));
  cache.Clear();
  CHECK(cache.Size() == 0);
  CHECK_FALSE(cache.Get("a:b", &out));
}

TEST_CASE("playlist 403 error classification detects development-mode restriction") {
  CHECK(IsDevModePlaylistRestriction(403, "owner-1", "me-1"));
  CHECK_FALSE(IsDevModePlaylistRestriction(403, "me-1", "me-1"));  // own playlist
  CHECK_FALSE(IsDevModePlaylistRestriction(403, "", "me-1"));  // unknown owner
  CHECK_FALSE(IsDevModePlaylistRestriction(403, "owner-1", ""));  // unknown me
  CHECK_FALSE(IsDevModePlaylistRestriction(404, "owner-1", "me-1"));
  CHECK_FALSE(IsDevModePlaylistRestriction(0, "owner-1", "me-1"));
}

TEST_CASE("audio cache usage sums files under engine audio cache directory") {
  const auto temp =
      std::filesystem::temp_directory_path() /
      (L"sr_cache_usage_test_" + std::to_wstring(::GetCurrentProcessId()));
  std::error_code ec;
  std::filesystem::remove_all(temp, ec);
  const auto audioDir = temp / L"engine" / L"audio";
  std::filesystem::create_directories(audioDir / L"sub", ec);
  REQUIRE(!ec);
  std::filesystem::create_directories(temp / L"engine" / L"tmp", ec);
  REQUIRE(!ec);
  auto writeFile = [&](const std::filesystem::path& file, size_t size) {
    std::ofstream stream(file, std::ios::binary);
    const std::string bytes(size, '\0');
    stream.write(bytes.data(), static_cast<std::streamsize>(size));
  };
  writeFile(audioDir / L"a.bin", 1234);
  writeFile(audioDir / L"sub" / L"b.bin", 56);
  writeFile(temp / L"engine" / L"tmp" / L"other.bin", 100);  // outside audio cache

  sr::paths::SetTestRootForCurrentProcess(temp.wstring());
  REQUIRE(paths::EngineAudioCacheDir() == paths::Root() + L"\\engine\\audio");
  CHECK(paths::SumFileBytesUnderDir(paths::EngineAudioCacheDir()) == 1290);
  CHECK(paths::SumFileBytesUnderDir(paths::EngineAudioCacheDir() + L"\\missing") == 0);
  CHECK(paths::SumFileBytesUnderDir(paths::EngineStateDir() + L"\\tmp") == 100);

  std::filesystem::remove_all(temp, ec);
}

TEST_CASE("ui rows: track rows carry ordinal eyebrow and formatted duration") {
  TrackRef track = ExampleTrack();
  const ListRow first = MakeTrackRow(track);
  CHECK(first.kind == ListRowKind::Track);
  CHECK(first.eyebrow == L"TRACK");
  CHECK(first.duration == L"4:03");  // 243001 ms
  CHECK(first.title == L"Signal Path");
  CHECK(first.detail == L"First Artist, Second Artist  ·  Local Sessions");
  CHECK(first.artworkUrl == track.cover_url);
  CHECK_FALSE(first.artworkSeed == 0);

  const ListRow second = MakeTrackRow(track, 7);
  CHECK(second.eyebrow == L"07  ·  TRACK");
  const ListRow tenth = MakeTrackRow(track, 10);
  CHECK(tenth.eyebrow == L"10  ·  TRACK");

  TrackRef unknown;
  const ListRow empty = MakeTrackRow(unknown, 1);
  CHECK(empty.title == L"Untitled track");
  CHECK(empty.duration == L"—");
  CHECK(empty.detail == L"Unknown artist  ·  Unknown album");
}

TEST_CASE("ui rows: album and artist rows keep stable kinds and artwork") {
  AlbumRef album;
  album.name = "Local Sessions";
  album.artist_names = {"First Artist"};
  album.cover_url = "https://i.scdn.co/image/album";
  const ListRow albumRow = MakeAlbumRow(album);
  CHECK(albumRow.kind == ListRowKind::Album);
  CHECK(albumRow.eyebrow == L"ALBUM");
  CHECK(albumRow.detail == L"First Artist");
  CHECK(albumRow.duration.empty());
  CHECK(albumRow.artworkUrl == album.cover_url);

  ArtistRef artist;
  artist.name = "First Artist";
  const ListRow artistRow = MakeArtistRow(artist);
  CHECK(artistRow.kind == ListRowKind::Artist);
  CHECK(artistRow.eyebrow == L"ARTIST");
  CHECK(artistRow.detail == L"Open artist page");
}

TEST_CASE("artist page state: rows derive artwork and seed deterministically for placeholder tiles") {
  ArtistRef artist;
  artist.name = "First Artist";
  artist.cover_url = "https://i.scdn.co/image/artist-cover";
  const ListRow row = MakeArtistRow(artist);
  CHECK(row.kind == ListRowKind::Artist);
  CHECK(row.title == L"First Artist");
  CHECK(row.artworkUrl == artist.cover_url);
  // The seed feeds the placeholder tile color until artwork loads; it must be
  // stable per artwork URL and differ across artists.
  CHECK(row.artworkSeed == RowArtworkSeed(artist.cover_url, row.title));
  CHECK(RowArtworkSeed("https://i.scdn.co/image/a", L"a") ==
        RowArtworkSeed("https://i.scdn.co/image/a", L"a"));
  CHECK_FALSE(RowArtworkSeed("https://i.scdn.co/image/a", L"a") ==
              RowArtworkSeed("https://i.scdn.co/image/b", L"a"));
  // Missing artwork falls back to a title-derived seed so the tile is still
  // stable and distinct from other artists.
  ArtistRef unnamed;
  unnamed.id = "artist-2";
  const ListRow fallback = MakeArtistRow(unnamed);
  CHECK(fallback.title == L"Unknown artist");
  CHECK(fallback.artworkUrl.empty());
  CHECK(fallback.artworkSeed == RowArtworkSeed("", fallback.title));
  CHECK_FALSE(fallback.artworkSeed == row.artworkSeed);
}

TEST_CASE("artist page state: top-track rows number from one like the page header") {
  // SetArtistPage numbers top tracks 1..N; the first row must show "01 · TRACK"
  // so the header's "N top tracks" count matches the visible ordinals.
  TrackRef track = ExampleTrack();
  const ListRow first = MakeTrackRow(track, 1);
  CHECK(first.eyebrow == L"01  ·  TRACK");
  CHECK(first.accessibleText.find(L"Signal Path") != std::wstring::npos);
  CHECK(first.accessibleText.find(L"First Artist") != std::wstring::npos);
}

TEST_CASE("ui rows: hover property decodes to zero-based row index") {
  CHECK(DecodeHoverIndex(0) == -1);  // absent property: no hover
  CHECK(DecodeHoverIndex(1) == 0);
  CHECK(DecodeHoverIndex(7) == 6);
  CHECK(DecodeHoverIndex(-1) == -2);
}

TEST_CASE("ui rows: artwork tile hit test follows scaled row geometry") {
  // Row origin at (20, 40) on a 96 DPI screen: the 46x46 tile starts at
  // (+9, +9) and covers exactly the kRowArtwork* extents.
  CHECK_FALSE(RowTileHit(28, 48, 20, 40, 96));
  CHECK(RowTileHit(29, 49, 20, 40, 96));
  CHECK(RowTileHit(74, 94, 20, 40, 96));
  CHECK_FALSE(RowTileHit(75, 95, 20, 40, 96));
  CHECK_FALSE(RowTileHit(40, 40, 20, 40, 96));  // above the tile
  // 192 DPI doubles every extent: tile spans (38, 58)..(130, 150).
  CHECK(RowTileHit(38, 58, 20, 40, 192));
  CHECK_FALSE(RowTileHit(37, 58, 20, 40, 192));
  CHECK_FALSE(RowTileHit(130, 150, 20, 40, 192));
  // A zero DPI falls back to 96.
  CHECK(RowTileHit(29, 49, 20, 40, 0));
}

TEST_CASE("ui rows: time formatting clamps negatives and pads seconds") {
  CHECK(FormatTime(0) == L"0:00");
  CHECK(FormatTime(-500) == L"0:00");
  CHECK(FormatTime(59'000) == L"0:59");
  CHECK(FormatTime(60'000) == L"1:00");
  CHECK(FormatTime(4 * 60'000 + 3'000) == L"4:03");
}

}  // namespace
}  // namespace sr
