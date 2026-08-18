#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest/doctest.h>

#include <atomic>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <functional>
#include <memory>
#include <stdexcept>
#include <thread>
#include <vector>

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

TEST_CASE("web_api_token message maps minted tokens without a process") {
  EngineMessage message = ParseEngineMessage(
      R"({"type":"web_api_token","request_id":"77","ok":true,"token_type":"Bearer","access_token":"tok-abc","expires_in":3540})");
  CHECK(message.kind == EngineMessage::Kind::WebApiToken);
  CHECK(message.request_id == "77");
  CHECK(message.ok);
  CHECK(message.token.token_type == "Bearer");
  CHECK(message.token.access_token == "tok-abc");
  CHECK(message.token.expires_in == 3540);
  CHECK(message.error.empty());
}

TEST_CASE("web_api_token error responses carry the failure without token fields") {
  EngineMessage message = ParseEngineMessage(
      R"({"type":"web_api_token","request_id":"78","ok":false,"error":"could not mint a Spotify Web API token: unavailable"})");
  CHECK_FALSE(message.ok);
  CHECK(message.error == "could not mint a Spotify Web API token: unavailable");
  CHECK(message.token.access_token.empty());
}

TEST_CASE("web_api_token messages reject malformed token payloads") {
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"web_api_token","request_id":"79","ok":true,"token_type":"Bearer","access_token":"","expires_in":3600})"),
                  std::invalid_argument);
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"web_api_token","request_id":"80","ok":true,"token_type":"Bearer","access_token":"tok","expires_in":0})"),
                  std::invalid_argument);
  CHECK_THROWS_AS(
      ParseEngineMessage(R"({"type":"web_api_token","ok":true})"),
      std::invalid_argument);
}

TEST_CASE("web_api_token provider reuses fresh tokens and refreshes on expiry") {
  auto now = std::make_shared<int64_t>(1'000'000);
  int mints = 0;
  WebApiTokenProvider provider(
      [&](WebApiToken* token, std::string*, int) {
        ++mints;
        token->token_type = "Bearer";
        token->access_token = "token-" + std::to_string(mints);
        token->expires_in = 3600;
        return true;
      },
      [now] { return *now; });
  CHECK(provider.GetAccessToken() == "token-1");
  CHECK(provider.GetAccessToken() == "token-1");  // cached
  CHECK(mints == 1);
  *now += 3599;  // still inside the reported lifetime
  CHECK(provider.GetAccessToken() == "token-1");
  *now += 2;  // past expiry: the next call must re-mint
  CHECK(provider.GetAccessToken() == "token-2");
  CHECK(mints == 2);
}

TEST_CASE("web_api_token provider forced refresh mints a new token") {
  int mints = 0;
  WebApiTokenProvider provider([&](WebApiToken* token, std::string*, int) {
    token->access_token = "token-" + std::to_string(++mints);
    token->expires_in = 3600;
    return true;
  });
  CHECK(provider.GetAccessToken() == "token-1");
  CHECK(provider.Refresh());
  CHECK(provider.GetAccessToken() == "token-2");
  CHECK(mints == 2);
}

TEST_CASE("web_api_token provider surfaces engine mint failures") {
  int calls = 0;
  WebApiTokenProvider provider([&](WebApiToken*, std::string* error, int) {
    ++calls;
    if (error) *error = "engine says no";
    return false;
  });
  CHECK_THROWS_WITH_AS(provider.GetAccessToken(), "engine says no",
                       std::runtime_error);
  CHECK_FALSE(provider.Refresh());
  CHECK(calls == 2);
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

TEST_CASE("engine state reconcile keeps unconfirmed optimistic overrides") {
  PlaybackEngineState current;
  current.playing = false;         // optimistic pause
  current.position_ms = 42'000;    // optimistic seek target
  current.volume_percent = 65;     // optimistic volume
  current.current_index = 2;       // optimistic next
  current.current_uri = "spotify:track:next";
  current.duration_ms = 180'000;
  current.queue.push_back(TrackRef{});
  current.queue.push_back(TrackRef{});
  current.queue.push_back(TrackRef{});
  current.queue[2].uri = "spotify:track:next";

  PlaybackStateReconciler overrides;
  overrides.SetOverride("playing", "r1");
  overrides.SetOverride("position_ms", "r1");
  overrides.SetOverride("volume", "r2");
  overrides.SetOverride("current_index", "r3");
  overrides.SetOverride("current_uri", "r3");
  overrides.SetOverride("duration_ms", "r3");
  overrides.SetOverride("queue", "r3");

  // A stale pre-command event: still playing, old position, old volume and an
  // old queue/index. Only fields without a pending override may apply.
  PlaybackEngineState stale;
  stale.playing = true;
  stale.position_ms = 41'500;
  stale.volume_percent = 70;
  stale.current_index = 0;
  stale.current_uri = "spotify:track:old";
  stale.duration_ms = 200'000;
  stale.queue.push_back(TrackRef{});  // single old track

  const PlaybackEngineState applied =
      ReconcileEngineState(stale, current, overrides);
  CHECK_FALSE(applied.playing);                 // optimistic pause kept
  CHECK(applied.position_ms == 42'000);         // optimistic seek kept
  CHECK(applied.volume_percent == 65);          // optimistic volume kept
  CHECK(applied.current_index == 2);            // optimistic next kept
  CHECK(applied.current_uri == "spotify:track:next");
  CHECK(applied.duration_ms == 180'000);
  REQUIRE(applied.queue.size() == 3);           // optimistic queue kept
  CHECK(applied.queue[2].uri == "spotify:track:next");
}

TEST_CASE("engine state reconcile releases overrides on confirmation") {
  PlaybackStateReconciler overrides;
  overrides.SetOverride("playing", "r1");
  overrides.SetOverride("position_ms", "r1");

  PlaybackEngineState current;
  current.playing = false;
  current.position_ms = 42'000;

  // The command response arrives: the immediately following state event is
  // authoritative and must apply even if it differs from the optimistic UI.
  overrides.Confirm("r1");
  CHECK_FALSE(overrides.Overridden("playing"));
  CHECK_FALSE(overrides.Overridden("position_ms"));

  PlaybackEngineState authoritative;
  authoritative.playing = false;
  authoritative.position_ms = 41'900;  // decoder's actual seek result
  const PlaybackEngineState applied =
      ReconcileEngineState(authoritative, current, overrides);
  CHECK_FALSE(applied.playing);
  CHECK(applied.position_ms == 41'900);
}

TEST_CASE("engine state reconcile latest intent wins and empty id clears") {
  PlaybackStateReconciler overrides;
  overrides.SetOverride("playing", "r1");
  overrides.SetOverride("playing", "r2");  // newer command takes over
  CHECK(overrides.Overridden("playing"));

  // The older command's response must not release the newer intent.
  overrides.Confirm("r1");
  CHECK(overrides.Overridden("playing"));
  overrides.Confirm("r2");
  CHECK_FALSE(overrides.Overridden("playing"));

  // Clearing with an empty id releases the field.
  overrides.SetOverride("playing", "r3");
  CHECK(overrides.Overridden("playing"));
  overrides.SetOverride("playing", "");
  CHECK_FALSE(overrides.Overridden("playing"));
  CHECK_FALSE(overrides.HasPending());
}

TEST_CASE("engine state reconcile never drops fields without overrides") {
  PlaybackStateReconciler overrides;
  PlaybackEngineState current;
  current.volume_percent = 65;
  current.shuffle = true;

  PlaybackEngineState incoming;
  incoming.ready = true;
  incoming.volume_percent = 80;
  incoming.shuffle = false;
  incoming.repeat = "track";
  incoming.playing = true;

  const PlaybackEngineState applied =
      ReconcileEngineState(incoming, current, overrides);
  CHECK(applied.ready);
  CHECK(applied.volume_percent == 80);
  CHECK_FALSE(applied.shuffle);
  CHECK(applied.repeat == "track");
  CHECK(applied.playing);
}

TEST_CASE("ui rows: rail artwork maps rows through the filtered playlist indices") {
  PlaylistRef first;
  first.id = "p1";
  first.name = "First";
  first.cover_url = "https://i.scdn.co/image/p1";
  PlaylistRef second;
  second.id = "p2";
  second.name = "Second";
  second.cover_url = "https://i.scdn.co/image/p2";
  PlaylistRef third;
  third.id = "p3";
  third.name = "Third";  // no cover art

  // Row 0 is the Queue entry; rows 1..N are playlists in filtered order.
  const std::vector<int> filtered = {0, 3, 1};  // Queue, Third, First
  const std::vector<PlaylistRef> playlists = {first, second, third};
  CHECK(RailPlaylistForRow(playlists, filtered, 0) == nullptr);
  CHECK(RailRowArtworkUrl(playlists, filtered, 0).empty());
  REQUIRE(RailPlaylistForRow(playlists, filtered, 1) != nullptr);
  CHECK(RailPlaylistForRow(playlists, filtered, 1)->id == "p3");
  CHECK(RailRowArtworkUrl(playlists, filtered, 1).empty());  // no art
  REQUIRE(RailPlaylistForRow(playlists, filtered, 2) != nullptr);
  CHECK(RailPlaylistForRow(playlists, filtered, 2)->id == "p1");
  CHECK(RailRowArtworkUrl(playlists, filtered, 2) == first.cover_url);

  // Out-of-range rows and rows past the filtered rail map nowhere.
  CHECK(RailPlaylistForRow(playlists, filtered, 3) == nullptr);
  CHECK(RailPlaylistForRow(playlists, filtered, -1) == nullptr);
  CHECK(RailRowArtworkUrl(playlists, filtered, 3).empty());
  // An empty rail (no playlists, not even Queue) still maps safely.
  const std::vector<int> queueOnly = {0};
  CHECK(RailPlaylistForRow(playlists, queueOnly, 1) == nullptr);
  // The middle index is 1-based: row 1 with a plain filter maps to playlist 0.
  const std::vector<int> unfiltered = {0, 1, 2, 3};
  CHECK(RailPlaylistForRow(playlists, unfiltered, 1)->id == "p1");
  CHECK(RailPlaylistForRow(playlists, unfiltered, 2)->id == "p2");
  CHECK(RailPlaylistForRow(playlists, unfiltered, 3)->id == "p3");
  // Row seeds stay deterministic per playlist for the fallback tile.
  CHECK(RailPlaylistForRow(playlists, unfiltered, 3) != nullptr);
  CHECK_FALSE(RowArtworkSeed(third.cover_url, Utf8ToWide(third.name)) == 0);
}

TEST_CASE("retry-after header parses seconds and HTTP dates") {
  // Plain delta seconds (RFC 7231).
  CHECK(ParseRetryAfterSeconds("", 0) == 0);
  CHECK(ParseRetryAfterSeconds("35", 0) == 35);
  CHECK(ParseRetryAfterSeconds(" 12 ", 0) == 12);
  CHECK(ParseRetryAfterSeconds("0", 0) == 0);
  // HTTP-date: the wait is the remaining time until that moment (1994-11-06
  // 08:49:37 UTC = unix 784111777).
  CHECK(ParseRetryAfterSeconds("Sun, 06 Nov 1994 08:49:37 GMT", 784111777) == 0);
  CHECK(ParseRetryAfterSeconds("Sun, 06 Nov 1994 08:49:37 GMT", 784111757) == 20);
  // RFC 850 format with a two-digit year.
  CHECK(ParseRetryAfterSeconds("Sunday, 06-Nov-94 08:49:37 GMT", 784111757) == 20);
  // asctime format.
  CHECK(ParseRetryAfterSeconds("Sun Nov  6 08:49:37 1994", 784111757) == 20);
  CHECK(ParseRetryAfterSeconds("Sun Nov  6 08:49:37 1994", 784111777) == 0);
  // Unparseable headers never produce a wait.
  CHECK(ParseRetryAfterSeconds("garbage", 0) == 0);
  CHECK(ParseRetryAfterSeconds("Thu, 32 Feb 2020 00:00:00 GMT", 1'000'000) == 0);
}

TEST_CASE("backoff doubles exponentially within jitter bounds") {
  const auto fixed = [](double value) {
    return [value] { return value; };
  };
  // rng 0.5 lands in the middle of each band: 1, 2, 3, 6, ...
  CHECK(ComputeBackoffDelay(0, 0, fixed(0.5)) == 1);
  CHECK(ComputeBackoffDelay(1, 0, fixed(0.5)) == 2);
  CHECK(ComputeBackoffDelay(2, 0, fixed(0.5)) == 3);
  CHECK(ComputeBackoffDelay(3, 0, fixed(0.5)) == 6);
  // rng 0 and 1 pin the [base/2, base] floor and ceiling.
  CHECK(ComputeBackoffDelay(0, 0, fixed(0.0)) == 1);
  CHECK(ComputeBackoffDelay(0, 0, fixed(1.0)) == 1);
  CHECK(ComputeBackoffDelay(2, 0, fixed(0.0)) == 2);
  CHECK(ComputeBackoffDelay(2, 0, fixed(1.0)) == 4);
  CHECK(ComputeBackoffDelay(3, 0, fixed(1.0)) == 8);
}

TEST_CASE("backoff honors retry-after and caps the wait") {
  const auto fixed = [](double value) {
    return [value] { return value; };
  };
  // A Retry-After hint dominates the exponential schedule.
  CHECK(ComputeBackoffDelay(0, 35, fixed(0.5)) == 35);
  CHECK(ComputeBackoffDelay(3, 35, fixed(1.0)) == 35);  // base 8 < 35
  // The exponential term wins when it exceeds the hint; base caps at 60.
  CHECK(ComputeBackoffDelay(6, 2, fixed(1.0)) == 60);
  CHECK(ComputeBackoffDelay(6, 2, fixed(0.0)) == 30);
  CHECK(ComputeBackoffDelay(9, 0, fixed(1.0)) == 60);
  // Hard ceiling: never wait longer than 300 s per retry.
  CHECK(ComputeBackoffDelay(0, 400, fixed(0.5)) == 300);
  // A missing hint still schedules a minimum wait, never zero.
  CHECK(ComputeBackoffDelay(0, 0, fixed(0.5)) >= 1);
}

TEST_CASE("web_api_token provider shares one in-flight mint across racing callers") {
  auto now = std::make_shared<int64_t>(1'000'000);
  std::atomic<int> mints{0};
  WebApiTokenProvider provider(
      [&](WebApiToken* token, std::string*, int) {
        ++mints;
        std::this_thread::sleep_for(std::chrono::milliseconds(20));  // hold the mint open
        token->token_type = "Bearer";
        token->access_token = "token-race";
        token->expires_in = 3600;
        return true;
      },
      [now] { return *now; });
  constexpr int kCallers = 8;
  std::vector<std::string> results(kCallers);
  std::vector<std::thread> threads;
  for (int i = 0; i < kCallers; ++i) {
    threads.emplace_back([&provider, &results, i] {
      results[i] = provider.GetAccessToken();
    });
  }
  for (auto& thread : threads) thread.join();
  CHECK(mints.load() == 1);  // every waiter shared the in-flight mint
  for (const std::string& token : results) CHECK(token == "token-race");
  CHECK(provider.GetAccessToken() == "token-race");  // still cached
  CHECK(mints.load() == 1);
}

TEST_CASE("delayed api task queue runs due work in deadline order, one at a time") {
  using Clock = DelayedTaskQueue::Clock;
  auto base = std::make_shared<Clock::time_point>(Clock::now());
  DelayedTaskQueue queue([base] { return *base; });
  std::vector<std::string> ran;
  const auto dispatch = [&](std::function<void()> task) {
    task();
    ran.push_back("run");
  };
  // Scheduled out of deadline order; the queue must reorder.
  queue.Schedule(10, [&] { ran.push_back("a"); });
  queue.Schedule(2, [&] { ran.push_back("b"); });
  queue.Schedule(10, [&] { ran.push_back("c"); });
  CHECK(queue.Size() == 3);
  CHECK(queue.RunDue(*base + std::chrono::seconds(1), dispatch) == 0);
  CHECK(queue.RunDue(*base + std::chrono::seconds(2), dispatch) == 1);
  CHECK(ran == std::vector<std::string>({"b", "run"}));
  // Same-deadline tasks keep schedule order (FIFO ties).
  CHECK(queue.RunDue(*base + std::chrono::seconds(10), dispatch) == 2);
  CHECK(ran == std::vector<std::string>({"b", "run", "a", "run", "c", "run"}));
  CHECK(queue.Empty());
  queue.Schedule(5, [&] { ran.push_back("d"); });
  queue.Clear();
  CHECK(queue.Size() == 0);
  CHECK(queue.Empty());
}

TEST_CASE("ui rows: search enter routing targets only the main search edit") {
  // Enter in the main search box submits through the search button; the rail
  // filter applies live per keystroke so Enter must never be routed there.
  CHECK(EditRoleForControl(kSearchEditControlId) == EditRole::Search);
  CHECK(EditRoleForControl(kPlaylistFilterEditControlId) == EditRole::Filter);
  CHECK(EditRoleForControl(999) == EditRole::Other);
  CHECK(EditRoleForControl(kSearchEditControlId + 1) == EditRole::Other);
  CHECK_FALSE(EditRoleForControl(kPlaylistFilterEditControlId) == EditRole::Search);
}

TEST_CASE("needs_login state events map the authorize URL") {
  const json line = {
      {"type", "state"},
      {"ready", false},
      {"auth_state", "needs_login"},
      {"auth_url", "https://accounts.spotify.com/authorize?state=abc"},
      {"playing", false},
      {"position_ms", 0},
      {"duration_ms", 0},
      {"volume", 50},
      {"shuffle", false},
      {"repeat", "off"},
      {"current_index", -1},
      {"current_uri", ""},
      {"queue", json::array()},
      {"error", nullptr},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  REQUIRE(message.kind == EngineMessage::Kind::State);
  CHECK_FALSE(message.state.ready);
  CHECK(message.state.auth_state == EngineAuthState::NeedsLogin);
  CHECK(message.state.auth_url ==
        "https://accounts.spotify.com/authorize?state=abc");
}

TEST_CASE("state events without an authorize URL map to an empty one") {
  const json line = {
      {"type", "state"},
      {"ready", true},
      {"auth_state", "ready"},
      {"playing", false},
      {"position_ms", 0},
      {"duration_ms", 0},
      {"volume", 50},
      {"shuffle", false},
      {"repeat", "off"},
      {"current_index", -1},
      {"current_uri", ""},
      {"queue", json::array()},
      {"error", nullptr},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  CHECK(message.state.auth_state == EngineAuthState::Ready);
  CHECK(message.state.auth_url.empty());
  // Unknown auth states degrade to Authenticating (safe local default).
  json unknown = line;
  unknown["auth_state"] = "waiting";
  CHECK(ParseEngineMessage(unknown.dump()).state.auth_state ==
        EngineAuthState::Authenticating);
}

TEST_CASE("session button enablement maps from engine auth state") {
  PlaybackEngineState state;
  // Fresh engine: no session, no flow -> only Log in is actionable.
  state.auth_state = EngineAuthState::NeedsLogin;
  state.ready = false;
  CHECK(LoginButtonEnabled(state));
  CHECK_FALSE(LogoutButtonEnabled(state));

  // Live session -> Log out only.
  state.auth_state = EngineAuthState::Ready;
  state.ready = true;
  CHECK_FALSE(LoginButtonEnabled(state));
  CHECK(LogoutButtonEnabled(state));

  // Ready flag and state must agree: a torn-down engine is never logged out.
  state.auth_state = EngineAuthState::Ready;
  state.ready = false;
  CHECK_FALSE(LogoutButtonEnabled(state));

  // Flow in flight / degraded engine: neither action (no double-submit).
  state.auth_state = EngineAuthState::Authenticating;
  state.ready = false;
  CHECK_FALSE(LoginButtonEnabled(state));
  CHECK_FALSE(LogoutButtonEnabled(state));
  state.auth_state = EngineAuthState::Error;
  CHECK_FALSE(LoginButtonEnabled(state));
  CHECK_FALSE(LogoutButtonEnabled(state));
}

TEST_CASE("login and logout requests use the line protocol command types") {
  const json login = BuildEngineRequest("50", "login");
  CHECK((login == json{{"request_id", "50"}, {"type", "login"}}));
  const json logout = BuildEngineRequest("51", "logout");
  CHECK((logout == json{{"request_id", "51"}, {"type", "logout"}}));
}

TEST_CASE("browse requests carry the engine command arguments") {
  const json playlists = BuildEngineRequest(
      "60", "browse_playlists", {{"length", 500}});
  CHECK(playlists["request_id"] == "60");
  CHECK(playlists["type"] == "browse_playlists");
  CHECK(playlists["length"] == 500);

  const json playlist = BuildEngineRequest("61", "browse_playlist",
                                           {{"id", "playlist-123"}});
  CHECK((playlist == json{{"request_id", "61"},
                          {"type", "browse_playlist"},
                          {"id", "playlist-123"}}));

  const json album = BuildEngineRequest("62", "browse_album",
                                        {{"id", "album-456"}});
  CHECK(album["id"] == "album-456");

  const json artist = BuildEngineRequest("63", "browse_artist",
                                         {{"id", "artist-789"}});
  CHECK(artist["id"] == "artist-789");

  const json search = BuildEngineRequest(
      "64", "browse_search", {{"query", "daft punk"}, {"limit", 10}});
  CHECK(search["query"] == "daft punk");
  CHECK(search["limit"] == 10);
}

TEST_CASE("browse responses parse as data messages with the raw payload") {
  const json line = {
      {"type", "browse_playlists"},
      {"request_id", "70"},
      {"ok", true},
      {"data", json::array()},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  REQUIRE(message.kind == EngineMessage::Kind::Data);
  CHECK(message.request_id == "70");
  CHECK(message.ok);
  CHECK(message.data["data"].is_array());
}

TEST_CASE("browse_playlists payload maps to PlaylistRef models") {
  // browse_playlists is the one command whose "data" payload is the bare
  // playlist array (matching the contract "-> [...]" literally).
  const json line = {
      {"type", "browse_playlists"},
      {"request_id", "71"},
      {"ok", true},
      {"data",
       json::array({
           {{"id", "pl1"},
            {"uri", "spotify:playlist:pl1"},
            {"name", "Chill"},
            {"owner_id", "user-a"},
            {"owner_name", "Alice"},
            {"cover_url", "https://i.scdn.co/image/abc"},
            {"track_count", 42}},
           {{"id", "pl2"},
            {"name", "Workout"},
            {"owner_id", "user-b"},
            {"owner_name", "Bob"},
            {"collaborative", true}},
       })},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  const json& items = message.data["data"];
  REQUIRE(items.size() == 2);
  const PlaylistRef full = PlaylistRefFromEngineJson(items[0]);
  CHECK(full.id == "pl1");
  CHECK(full.uri == "spotify:playlist:pl1");  // the engine's own uri
  CHECK(full.name == "Chill");
  CHECK(full.owner == "Alice");
  CHECK(full.owner_id == "user-a");
  CHECK(full.cover_url == "https://i.scdn.co/image/abc");
  CHECK(full.tracks_total == 42);
  CHECK_FALSE(full.collaborative);
  const PlaylistRef minimal = PlaylistRefFromEngineJson(items[1]);
  // Absent uri is rebuilt from the id; the engine may send an empty owner
  // name (rootlist carries no display name).
  CHECK(minimal.uri == "spotify:playlist:pl2");
  CHECK(minimal.owner == "Bob");
  CHECK(minimal.cover_url.empty());
  CHECK(minimal.tracks_total == 0);
  CHECK(minimal.collaborative);
  CHECK(minimal.snapshot_id.empty());
}

TEST_CASE("browse_playlist payload maps tracks and the edit revision") {
  const json line = {
      {"type", "browse_playlist"},
      {"request_id", "72"},
      {"ok", true},
      {"data",
       json{{"id", "pl1"},
            {"uri", "spotify:playlist:pl1"},
            {"name", "Chill"},
            {"revision", "snap-9"},
            {"owner_id", "user-a"},
            {"owner_name", ""},
            {"tracks", json::array({TrackRefToEngineJson(ExampleTrack())})}}},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  const json& payload = message.data["data"];
  CHECK(payload["revision"] == "snap-9");
  const TrackRef track = TrackRefFromEngineJson(payload["tracks"][0]);
  CHECK(track.id == "track-id");
  CHECK(track.uri == "spotify:track:abc");
  CHECK(track.album_name == "Local Sessions");
  CHECK(track.duration_ms == 243001);
}

TEST_CASE("browse_album payload maps track lists") {
  const json line = {
      {"type", "browse_album"},
      {"request_id", "73"},
      {"ok", true},
      {"data",
       json{{"id", "album-1"},
            {"uri", "spotify:album:album-1"},
            {"name", "Discovery"},
            {"artist_names", json::array({"Daft Punk"})},
            {"cover_url", "https://i.scdn.co/image/discovery"},
            {"tracks", json::array({TrackRefToEngineJson(ExampleTrack())})}}},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  REQUIRE(message.data["data"]["tracks"].size() == 1);
  CHECK(TrackRefFromEngineJson(message.data["data"]["tracks"][0]).name ==
        "Signal Path");
}

TEST_CASE("browse_artist payload maps top tracks and albums") {
  const json line = {
      {"type", "browse_artist"},
      {"request_id", "74"},
      {"ok", true},
      {"data",
       json{{"id", "artist-1"},
            {"uri", "spotify:artist:artist-1"},
            {"name", "Daft Punk"},
            {"portrait_url", "https://i.scdn.co/image/portrait"},
            {"top_tracks", json::array({TrackRefToEngineJson(ExampleTrack())})},
            {"albums",
             json::array({
                 {{"id", "album-1"},
                  {"uri", "spotify:album:album-1"},
                  {"name", "Discovery"},
                  {"artist_names", json::array({"Daft Punk"})},
                  {"cover_url", "https://i.scdn.co/image/discovery"}},
                 {{"id", "album-2"}, {"name", "Homework"}},
             })}}},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  const json& albums = message.data["data"]["albums"];
  REQUIRE(albums.size() == 2);
  const AlbumRef album = AlbumRefFromEngineJson(albums[0]);
  CHECK(album.id == "album-1");
  CHECK(album.uri == "spotify:album:album-1");
  CHECK(album.name == "Discovery");
  CHECK(album.artist_names == std::vector<std::string>({"Daft Punk"}));
  CHECK(album.cover_url == "https://i.scdn.co/image/discovery");
  CHECK(AlbumRefFromEngineJson(albums[1]).artist_names.empty());
  const ArtistRef artist = ArtistRefFromEngineJson(
      json{{"id", "artist-1"},
           {"uri", "spotify:artist:artist-1"},
           {"name", "Daft Punk"},
           {"portrait_url", "https://i.scdn.co/image/portrait"}});
  CHECK(artist.id == "artist-1");
  CHECK(artist.uri == "spotify:artist:artist-1");
  CHECK(artist.name == "Daft Punk");
  CHECK(artist.cover_url == "https://i.scdn.co/image/portrait");
}

TEST_CASE("browse_search payload maps tracks, albums, and artists") {
  const json line = {
      {"type", "browse_search"},
      {"request_id", "75"},
      {"ok", true},
      {"data",
       json{{"tracks", json::array({TrackRefToEngineJson(ExampleTrack())})},
            {"albums",
             json::array({{{"id", "album-9"},
                           {"name", "Random Access Memories"}}})},
            {"artists",
             json::array({{{"id", "artist-9"}, {"name", "Daft Punk"}}})}}},
  };
  const EngineMessage message = ParseEngineMessage(line.dump());
  CHECK(message.data["data"]["tracks"].size() == 1);
  CHECK(message.data["data"]["albums"].size() == 1);
  CHECK(message.data["data"]["artists"].size() == 1);
}

TEST_CASE("browse error responses carry the failure without payload access") {
  const EngineMessage message = ParseEngineMessage(
      R"({"type":"browse_search","request_id":"76","ok":false,"error":"searchview unavailable"})");
  REQUIRE(message.kind == EngineMessage::Kind::Data);
  CHECK_FALSE(message.ok);
  CHECK(message.error == "searchview unavailable");
}

TEST_CASE("browse responses reject missing request ids and unknown types") {
  CHECK_THROWS_AS(ParseEngineMessage(
                      R"({"type":"browse_playlists","ok":true})"),
                  std::invalid_argument);
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"browse_playlists"})"),
                  std::invalid_argument);
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"browse_nope","request_id":"1","ok":true})"),
                  std::invalid_argument);
}

TEST_CASE("browse model conversion tolerates absent optional fields") {
  const PlaylistRef playlist = PlaylistRefFromEngineJson(
      json{{"id", "pl-x"}, {"name", "Untitled"}});
  CHECK(playlist.uri == "spotify:playlist:pl-x");
  CHECK(playlist.owner.empty());
  CHECK(playlist.owner_id.empty());
  CHECK(playlist.cover_url.empty());
  CHECK(playlist.tracks_total == 0);

  const AlbumRef album = AlbumRefFromEngineJson(json{{"id", "al-x"}});
  CHECK(album.uri == "spotify:album:al-x");
  CHECK(album.name.empty());

  CHECK_THROWS_AS(PlaylistRefFromEngineJson(json::array()), std::invalid_argument);
  CHECK_THROWS_AS(AlbumRefFromEngineJson(json::array()), std::invalid_argument);
  CHECK_THROWS_AS(ArtistRefFromEngineJson(json::array()), std::invalid_argument);
}

}  // namespace
}  // namespace sr
