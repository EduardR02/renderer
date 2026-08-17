#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest/doctest.h>

#include <stdexcept>

#include <nlohmann/json.hpp>

#include "playback_engine_client.h"

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

TEST_CASE("unknown protocol messages are rejected") {
  CHECK_THROWS_AS(ParseEngineMessage(R"({"type":"diagnostic","text":"x"})"),
                  std::invalid_argument);
  CHECK_THROWS(ParseEngineMessage("not json"));
  CHECK_THROWS_AS(BuildEngineRequest("", "play"), std::invalid_argument);
}

}  // namespace
}  // namespace sr
