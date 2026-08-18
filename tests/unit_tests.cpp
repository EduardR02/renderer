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

TEST_CASE("track list cache revisits serve the identical playlist content") {
  // The browse_playlist flow keys the cache as "p:"+id; a revisit after the
  // first fetch must resolve from the cache with the exact tracks and edit
  // revision, without any engine round-trip.
  TrackListCache cache(20, std::chrono::minutes(10));
  CachedTrackList fetched{std::vector<TrackRef>{ExampleTrack(), ExampleTrack()},
                          "revision-1"};
  cache.Put("p:0123456789ABCDEFGHIJKL", fetched);
  CachedTrackList revisit;
  REQUIRE(cache.Get("p:0123456789ABCDEFGHIJKL", &revisit));
  CHECK(revisit.snapshot_id == "revision-1");
  REQUIRE(revisit.tracks.size() == fetched.tracks.size());
  for (size_t i = 0; i < fetched.tracks.size(); ++i) {
    CHECK(revisit.tracks[i].id == fetched.tracks[i].id);
    CHECK(revisit.tracks[i].name == fetched.tracks[i].name);
    CHECK(revisit.tracks[i].uri == fetched.tracks[i].uri);
  }
  // Album revisits use their own key namespace.
  cache.Put("a:album-id", fetched);
  CachedTrackList albumRevisit;
  REQUIRE(cache.Get("a:album-id", &albumRevisit));
  CHECK(albumRevisit.tracks.size() == 2);
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

TEST_CASE("ui rows: row factories carry the item uri for playback matching") {
  // The active-row highlight and the row play button (pause toggle) match on
  // uri, so every row kind must expose the uri of the item it renders.
  TrackRef track = ExampleTrack();
  CHECK(MakeTrackRow(track).uri == track.uri);
  AlbumRef album;
  album.uri = "spotify:album:xyz";
  CHECK(MakeAlbumRow(album).uri == "spotify:album:xyz");
  ArtistRef artist;
  artist.uri = "spotify:artist:xyz";
  CHECK(MakeArtistRow(artist).uri == "spotify:artist:xyz");
}

TEST_CASE("ui rows: active row matches the engine current uri only for track rows") {
  TrackRef track = ExampleTrack();
  const ListRow trackRow = MakeTrackRow(track);
  CHECK(RowMatchesCurrentUri(trackRow, track.uri));

  // A different track or no current track never highlights the row.
  CHECK_FALSE(RowMatchesCurrentUri(trackRow, ""));
  CHECK_FALSE(RowMatchesCurrentUri(trackRow, "spotify:track:other"));
  TrackRef other = ExampleTrack();
  other.uri = "spotify:track:other";
  CHECK_FALSE(RowMatchesCurrentUri(MakeTrackRow(other), track.uri));

  // Albums and artists never highlight as playing even with a matching uri.
  AlbumRef album;
  album.uri = track.uri;
  CHECK_FALSE(RowMatchesCurrentUri(MakeAlbumRow(album), track.uri));
  ArtistRef artist;
  artist.uri = track.uri;
  CHECK_FALSE(RowMatchesCurrentUri(MakeArtistRow(artist), track.uri));

  // Rows without a uri (unresolvable track) can never be the current track.
  TrackRef missingUri = ExampleTrack();
  missingUri.uri.clear();
  CHECK_FALSE(RowMatchesCurrentUri(MakeTrackRow(missingUri), track.uri));
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
  // A stale filtered index past the end of the current library maps nowhere
  // (the library can shrink between a filter pass and a repaint; the draw
  // path must fall back to the seeded tile instead of reading out of range).
  const std::vector<int> stale = {0, 7, 2};
  CHECK(RailPlaylistForRow(playlists, stale, 1) == nullptr);
  CHECK(RailRowArtworkUrl(playlists, stale, 1).empty());
  CHECK(RailPlaylistForRow(playlists, stale, 2)->id == "p2");
  // An empty library never maps any row to a playlist.
  const std::vector<PlaylistRef> none;
  CHECK(RailPlaylistForRow(none, filtered, 1) == nullptr);
  CHECK(RailPlaylistForRow(none, unfiltered, 1) == nullptr);
  CHECK(RailRowArtworkUrl(none, unfiltered, 1).empty());
}

TEST_CASE("ui rows: label width ends before an overlapping sibling control") {
  // The workspace title/meta labels must stop 8 DIPs before the Rename
  // button (siblingLeft); overlapping siblings with WS_CLIPSIBLINGS leave
  // the shared band unpainted, cutting the covered control off.
  CHECK(LabelWidthBefore(400, 1120) == 712);   // default 8px gap
  CHECK(LabelWidthBefore(400, 1120, 0) == 720);
  CHECK(LabelWidthBefore(400, 1120, 16) == 704);
  // A sibling that starts left of the text cannot leave room: clamp to 0.
  CHECK(LabelWidthBefore(1120, 400) == 0);
  CHECK(LabelWidthBefore(500, 480) == 0);
  // Exact-fit sibling leaves exactly the gap.
  CHECK(LabelWidthBefore(400, 400, 8) == 0);
  CHECK(LabelWidthBefore(392, 400, 8) == 0);
  CHECK(LabelWidthBefore(391, 400, 8) == 1);
}

TEST_CASE("ui rows: edit centering inset recenters text and cue banner") {
  // 38px edit with a ~21px line height: inset centers the line.
  CHECK(EditCenteringInset(38, 21) == 8);
  // Odd heights bias the extra pixel to the bottom inset (integer divide).
  CHECK(EditCenteringInset(39, 21) == 9);
  // A line taller than the control leaves no room: inset clamps to 0.
  CHECK(EditCenteringInset(20, 21) == 0);
  CHECK(EditCenteringInset(21, 21) == 0);
  // Degenerate inputs never produce a negative inset.
  CHECK(EditCenteringInset(0, 21) == 0);
  CHECK(EditCenteringInset(38, 0) == 0);
  CHECK(EditCenteringInset(-4, 21) == 0);
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

TEST_CASE("ui rows: search-enter bypasses dialog navigation only for the search edit") {
  // The main window's IsDialogMessage consumes VK_RETURN even without a
  // default pushbutton; the loop must send Enter straight to the search
  // edit's subclass (which routes it to the Search button). Every other
  // message keeps the normal dialog-navigation path.
  MSG message{};
  message.message = WM_KEYDOWN;
  message.wParam = VK_RETURN;
  CHECK_FALSE(SearchEnterBypassesDialogNavigation(message));  // no hwnd

  const wchar_t* className = L"SROEnterRoutingTestWnd";
  WNDCLASSEXW wc{};
  wc.cbSize = sizeof(wc);
  wc.lpfnWndProc = ::DefWindowProcW;
  wc.hInstance = ::GetModuleHandleW(nullptr);
  wc.lpszClassName = className;
  ::RegisterClassExW(&wc);
  HWND parent = ::CreateWindowExW(0, className, L"", WS_OVERLAPPEDWINDOW,
                                  0, 0, 200, 200, nullptr, nullptr,
                                  wc.hInstance, nullptr);
  REQUIRE(parent != nullptr);
  auto makeEdit = [&](int id) {
    return ::CreateWindowExW(
        0, L"EDIT", L"",
        WS_CHILD | WS_VISIBLE | WS_BORDER | ES_AUTOHSCROLL, 0, 0, 100, 24,
        parent, reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)),
        wc.hInstance, nullptr);
  };
  HWND searchEdit = makeEdit(kSearchEditControlId);
  HWND filterEdit = makeEdit(kPlaylistFilterEditControlId);
  REQUIRE(searchEdit != nullptr);
  REQUIRE(filterEdit != nullptr);

  MSG enter{};
  enter.hwnd = searchEdit;
  enter.message = WM_KEYDOWN;
  enter.wParam = VK_RETURN;
  CHECK(SearchEnterBypassesDialogNavigation(enter));

  // The rail filter's Enter is left to the default edit behavior.
  enter.hwnd = filterEdit;
  CHECK_FALSE(SearchEnterBypassesDialogNavigation(enter));

  // Only the VK_RETURN keydown is routed; chars and other keys are not.
  enter.hwnd = searchEdit;
  enter.message = WM_CHAR;
  CHECK_FALSE(SearchEnterBypassesDialogNavigation(enter));
  enter.message = WM_KEYDOWN;
  enter.wParam = VK_SPACE;
  CHECK_FALSE(SearchEnterBypassesDialogNavigation(enter));

  ::DestroyWindow(searchEdit);
  ::DestroyWindow(filterEdit);
  ::DestroyWindow(parent);
  ::UnregisterClassW(className, wc.hInstance);
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

TEST_CASE("edit_* responses parse as data messages and errors carry text") {
  EngineMessage create = ParseEngineMessage(
      R"({"type":"edit_create_playlist","request_id":"90","ok":true,"data":{"id":"pl-new","uri":"spotify:playlist:pl-new","name":"Road Trip","owner_id":"alice","owner_name":"","track_count":0}})");
  CHECK(create.kind == EngineMessage::Kind::Data);
  CHECK(create.ok);
  const PlaylistRef playlist = PlaylistRefFromEngineJson(create.data["data"]);
  CHECK(playlist.id == "pl-new");
  CHECK(playlist.name == "Road Trip");
  CHECK(playlist.tracks_total == 0);

  EngineMessage rename = ParseEngineMessage(
      R"({"type":"edit_rename_playlist","request_id":"91","ok":false,"error":"playlist change failed: stale revision"})");
  CHECK(rename.kind == EngineMessage::Kind::Data);
  CHECK_FALSE(rename.ok);
  CHECK(rename.error == "playlist change failed: stale revision");
  CHECK(rename.data.find("data") == rename.data.end());
}

TEST_CASE("edit command request shapes match the engine line protocol") {
  const json create = BuildEngineRequest(
      "r1", "edit_create_playlist", {{"name", "Road Trip"}});
  CHECK(create["type"] == "edit_create_playlist");
  CHECK(create["name"] == "Road Trip");

  const json rename = BuildEngineRequest(
      "r2", "edit_rename_playlist", {{"id", "pl1"}, {"name", "Renamed"}});
  CHECK(rename["id"] == "pl1");
  CHECK(rename["name"] == "Renamed");

  const json remove = BuildEngineRequest(
      "r3", "edit_remove_playlist_tracks",
      {{"id", "pl1"}, {"uris", json::array({"spotify:track:abc"})}});
  CHECK(remove["type"] == "edit_remove_playlist_tracks");
  CHECK(remove["uris"][0] == "spotify:track:abc");

  const json reorder = BuildEngineRequest(
      "r4", "edit_reorder_playlist_tracks",
      {{"id", "pl1"}, {"from", 3}, {"to", 1}});
  CHECK(reorder["from"] == 3);
  CHECK(reorder["to"] == 1);
}

TEST_CASE("playlist cache serves fresh always, stale only as fetch fallback") {
  const int64_t now = 1'800'000'000;
  // Fresh (within the 10-minute TTL): usable with or without a fetch.
  CHECK(ClassifyPlaylistCache(now - 60, now, 10, false) ==
        PlaylistCacheUse::Fresh);
  CHECK(ClassifyPlaylistCache(now - 599, now, 10, false) ==
        PlaylistCacheUse::Fresh);
  CHECK(ClassifyPlaylistCache(now - 60, now, 10, true) ==
        PlaylistCacheUse::Fresh);
  // Exactly at the TTL boundary it is stale.
  CHECK(ClassifyPlaylistCache(now - 600, now, 10, false) ==
        PlaylistCacheUse::None);
  // Stale: usable only as the post-failure fallback, never to skip a fetch.
  CHECK(ClassifyPlaylistCache(now - 600, now, 10, true) ==
        PlaylistCacheUse::StaleFallback);
  CHECK(ClassifyPlaylistCache(now - 86'400, now, 10, true) ==
        PlaylistCacheUse::StaleFallback);
  CHECK(ClassifyPlaylistCache(now - 86'400, now, 10, false) ==
        PlaylistCacheUse::None);
  // Clock skew (fetched timestamp in the future) counts as fresh.
  CHECK(ClassifyPlaylistCache(now + 120, now, 10, false) ==
        PlaylistCacheUse::Fresh);
}

TEST_CASE("playlist refetch backoff doubles, caps, and stays bounded") {
  CHECK(PlaylistRetryDelaySeconds(0) == 5);
  CHECK(PlaylistRetryDelaySeconds(1) == 10);
  CHECK(PlaylistRetryDelaySeconds(2) == 20);
  CHECK(PlaylistRetryDelaySeconds(3) == 40);
  CHECK(PlaylistRetryDelaySeconds(4) == 60);
  CHECK(PlaylistRetryDelaySeconds(5) == 60);
  // Large/negative attempts stay clamped (no shift overflow, no regression).
  CHECK(PlaylistRetryDelaySeconds(40) == 60);
  CHECK(PlaylistRetryDelaySeconds(-1) == 5);
  CHECK(kPlaylistRetryMaxAttempts >= 1);
  CHECK(kPlaylistRetryBaseSeconds <= kPlaylistRetryMaxSeconds);
}

namespace {

CachedTrackList CachedTracksWithRevision(const std::string& revision) {
  CachedTrackList cached;
  cached.tracks = {ExampleTrack()};
  cached.snapshot_id = revision;
  return cached;
}

}  // namespace

TEST_CASE("playlist tracks cache document round-trips tracks and revision") {
  std::vector<CachedPlaylistTracks> entries;
  entries.push_back({"pl-a", CachedTracksWithRevision("rev-1"), 1000});
  entries.push_back({"pl-b", CachedTracksWithRevision("rev-2"), 2000});

  const json doc = BuildPlaylistTracksCacheDoc(entries, 3000);
  CHECK(doc["version"] == 1);
  CHECK(doc["saved_at"] == 3000);
  REQUIRE(doc["playlists"].is_array());
  REQUIRE(doc["playlists"].size() == 2);

  std::vector<CachedPlaylistTracks> parsed;
  REQUIRE(ParsePlaylistTracksCacheDoc(doc, &parsed));
  // The parser restores the in-memory invariant: most-recent-first.
  REQUIRE(parsed.size() == 2);
  CHECK(parsed[0].id == "pl-b");
  CHECK(parsed[0].fetched_at == 2000);
  CHECK(parsed[0].value.snapshot_id == "rev-2");
  REQUIRE(parsed[0].value.tracks.size() == 1);
  const TrackRef& track = parsed[0].value.tracks[0];
  CHECK(track.id == "track-id");
  CHECK(track.uri == "spotify:track:abc");
  CHECK(track.name == "Signal Path");
  CHECK(track.artist_names == ExampleTrack().artist_names);
  CHECK(track.artist_id == "artist-id");
  CHECK(track.album_id == "album-id");
  CHECK(track.album_name == "Local Sessions");
  CHECK(track.cover_url == "https://i.scdn.co/image/example");
  CHECK(track.duration_ms == 243001);
  CHECK(parsed[1].id == "pl-a");
  CHECK(parsed[1].fetched_at == 1000);
  CHECK(parsed[1].value.snapshot_id == "rev-1");
}

TEST_CASE("playlist tracks cache evicts the oldest beyond capacity") {
  std::vector<CachedPlaylistTracks> entries;
  // 30 distinct playlists, fetched one second apart, oldest first.
  for (int i = 0; i < 30; ++i) {
    CachedPlaylistTracks entry;
    entry.id = "pl-" + std::to_string(i);
    entry.fetched_at = 1000 + i;
    entry.value = CachedTracksWithRevision("rev");
    entries.push_back(std::move(entry));
  }
  TrimPlaylistTracksCache(&entries);
  REQUIRE(entries.size() == static_cast<size_t>(kPlaylistTracksCacheCapacity));
  CHECK(entries.front().id == "pl-29");
  CHECK(entries.back().id == "pl-5");

  // A re-fetched playlist (Application upserts with a newer fetched_at,
  // replacing the existing entry in place) jumps to the front; the window
  // keeps its oldest survivor because the upsert does not add an entry.
  for (CachedPlaylistTracks& entry : entries) {
    if (entry.id == "pl-20") entry.fetched_at = 2000 + 30;
  }
  TrimPlaylistTracksCache(&entries);
  REQUIRE(entries.size() == static_cast<size_t>(kPlaylistTracksCacheCapacity));
  CHECK(entries.front().id == "pl-20");
  CHECK(entries.back().id == "pl-5");
}

TEST_CASE("playlist tracks cache stale copy serves only as fetch fallback") {
  const int64_t now = 1'800'000'000;
  // The per-entry TTL follows the same classification as the playlist
  // library, with the disk-cache TTL (30 minutes).
  CHECK(ClassifyPlaylistCache(now - 60, now, kPlaylistTracksTtlMinutes, false) ==
        PlaylistCacheUse::Fresh);
  CHECK(ClassifyPlaylistCache(now - 1'700, now, kPlaylistTracksTtlMinutes,
                              false) == PlaylistCacheUse::Fresh);
  // At the boundary it is stale.
  CHECK(ClassifyPlaylistCache(now - 1'800, now, kPlaylistTracksTtlMinutes,
                              false) == PlaylistCacheUse::None);
  // A stale copy serves the click (shown instantly, refreshed in the
  // background); only a failed refresh keeps it as the visible fallback.
  CHECK(ClassifyPlaylistCache(now - 1'800, now, kPlaylistTracksTtlMinutes,
                              true) == PlaylistCacheUse::StaleFallback);
  CHECK(ClassifyPlaylistCache(now - 86'400, now, kPlaylistTracksTtlMinutes,
                              true) == PlaylistCacheUse::StaleFallback);
}

TEST_CASE("playlist tracks cache parsing rejects unusable documents") {
  std::vector<CachedPlaylistTracks> out;
  CHECK_FALSE(ParsePlaylistTracksCacheDoc(json::object(), &out));
  CHECK_FALSE(ParsePlaylistTracksCacheDoc(
      json{{"version", 2}, {"playlists", json::array()}}, &out));
  CHECK_FALSE(ParsePlaylistTracksCacheDoc(
      json{{"version", 1}, {"playlists", "nope"}}, &out));
  // Malformed rows are skipped, valid rows survive.
  json playlistList = json::array();
  playlistList.push_back(json{{"id", ""}, {"fetched_at", 1}});
  json okEntry = json::object();
  okEntry["id"] = "pl-ok";
  okEntry["fetched_at"] = 100;
  okEntry["revision"] = "r";
  okEntry["tracks"] = json::array({TrackRefToEngineJson(ExampleTrack())});
  playlistList.push_back(std::move(okEntry));
  const json doc = {{"version", 1}, {"playlists", std::move(playlistList)}};
  REQUIRE(ParsePlaylistTracksCacheDoc(doc, &out));
  REQUIRE(out.size() == 1);
  CHECK(out[0].id == "pl-ok");
  CHECK(out[0].fetched_at == 100);
  CHECK(out[0].value.snapshot_id == "r");
  REQUIRE(out[0].value.tracks.size() == 1);
  const TrackRef& track = out[0].value.tracks[0];
  CHECK(track.id == "track-id");
  CHECK(track.uri == "spotify:track:abc");
  CHECK(track.duration_ms == 243001);
}

}  // namespace
}  // namespace sr
