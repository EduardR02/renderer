#pragma once

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <windows.h>

#include <nlohmann/json.hpp>

#include "spotify_api.h"

namespace sr {

enum class EngineAuthState {
  Authenticating,
  // No usable session and no flow in flight: the Settings page presents the
  // Log in button with the engine-published authorize URL.
  NeedsLogin,
  Ready,
  Error,
};

struct PlaybackEngineState {
  bool ready = false;
  EngineAuthState auth_state = EngineAuthState::Authenticating;
  // Spotify OAuth authorize URL for the current/next login attempt, present
  // while the engine is NeedsLogin (and while the flow it started runs).
  std::string auth_url;
  // The signed-in account name from the engine's session, present while the
  // session is Ready. Used as the Web API user id for playlist edits.
  std::string username;
  bool playing = false;
  int64_t position_ms = 0;
  int64_t duration_ms = 0;
  int volume_percent = 100;
  bool shuffle = false;
  std::string repeat = "off";
  int current_index = -1;
  std::string current_uri;
  std::vector<TrackRef> queue;
  std::string error;
};

// Settings-page session-control enablement, pure and testable: Log in is
// actionable exactly while the engine waits for a fresh sign-in (it holds the
// regenerate-per-attempt authorize URL); Log out while a session is live.
// Both stay disabled while a flow is in flight so the UI cannot double-submit.
inline bool LoginButtonEnabled(const PlaybackEngineState& state) {
  return state.auth_state == EngineAuthState::NeedsLogin;
}
inline bool LogoutButtonEnabled(const PlaybackEngineState& state) {
  return state.auth_state == EngineAuthState::Ready && state.ready;
}

struct EngineMessage {
  enum class Kind { Response, State, Data } kind = Kind::Response;
  std::string request_id;
  bool ok = false;
  std::string error;
  PlaybackEngineState state;
  // The full response object for browse_*/edit_* (spclient) responses; the
  // accessors below parse their payloads from it.
  nlohmann::json data;
};



TrackRef TrackRefFromEngineJson(const nlohmann::json& value);
nlohmann::json TrackRefToEngineJson(const TrackRef& track);
PlaylistRef PlaylistRefFromEngineJson(const nlohmann::json& value);
AlbumRef AlbumRefFromEngineJson(const nlohmann::json& value);
ArtistRef ArtistRefFromEngineJson(const nlohmann::json& value);
EngineMessage ParseEngineMessage(const std::string& line);
nlohmann::json BuildEngineRequest(const std::string& requestId,
                                  const std::string& type,
                                  nlohmann::json arguments = {});

class PlaybackEngineClient {
 public:
  using StateCallback = std::function<void(PlaybackEngineState)>;
  using ErrorCallback = std::function<void(std::string)>;
  // Every command that the engine accepted (request_id matched) reports back
  // here. The app uses it to release optimistic UI overrides exactly when the
  // engine acknowledges the command; the immediately following state event is
  // then authoritative.
  using ResponseCallback = std::function<void(const std::string& request_id,
                                              bool ok)>;
  // A command was rejected by the engine (response ok=false). Transport-level
  // failures keep flowing through ErrorCallback; only rejected commands use
  // this callback so the app can reconcile its optimistic UI state.
  using CommandErrorCallback = std::function<void(std::string)>;

  PlaybackEngineClient() = default;
  ~PlaybackEngineClient();
  PlaybackEngineClient(const PlaybackEngineClient&) = delete;
  PlaybackEngineClient& operator=(const PlaybackEngineClient&) = delete;

  bool Start(const std::wstring& executable, const std::wstring& stateDirectory,
             const std::wstring& diagnosticLog, StateCallback onState,
             ErrorCallback onError, ResponseCallback onResponse,
             CommandErrorCallback onCommandError,
             std::string* error = nullptr);
  void Shutdown();
  bool Running() const;

  std::string Status();
  std::string PlayQueue(const std::vector<TrackRef>& queue, int index,
                        int64_t positionMs = 0);
  std::string Play();
  std::string Pause();
  std::string Next();
  std::string Previous();
  std::string Seek(int64_t positionMs);
  std::string SetVolume(int percent);
  std::string SetShuffle(bool enabled);
  std::string SetRepeat(const std::string& mode);
  std::string AddQueue(const TrackRef& track);
  std::string RemoveQueue(int index);
  std::string MoveQueue(int from, int to);
  // Clears the cached credentials and tears the session down; the engine
  // immediately emits a needs_login state event carrying a fresh authorize
  // URL, so re-login works without a restart.
  std::string Logout();
  // Starts the OAuth flow on demand. The engine publishes the authorize URL
  // in its needs_login state first; the UI opens it, then sends this command.
  // No-op when a session is already live or a flow is in flight.
  std::string TriggerLogin();

  // Blocking engine round-trips served by the engine's spclient session
  // (same protocol machinery as RequestData). Each returns false with
  // `error` set on transport failure, engine rejection, malformed payload, or
  // timeout. Playlist/album/artist ids are the engine's Spotify ids.
  bool BrowsePlaylists(int length, std::vector<PlaylistRef>* out,
                       std::string* error, int timeoutMs = 20000);
  bool BrowsePlaylist(const std::string& id, std::vector<TrackRef>* tracks,
                      std::string* revisionOut, std::string* error,
                      int timeoutMs = 20000);
  bool BrowseAlbum(const std::string& id, std::vector<TrackRef>* tracks,
                   std::string* error, int timeoutMs = 20000);
  bool BrowseArtist(const std::string& id, std::vector<TrackRef>* topTracks,
                    std::vector<AlbumRef>* albums, std::string* error,
                    int timeoutMs = 20000);
  bool BrowseSearch(const std::string& query, int limit, SearchResult* out,
                    std::string* error, int timeoutMs = 20000);

  // Playlist edits on the engine's spclient playlist4 session: these replace
  // the Web API edit calls entirely. Revisions/checksums are fetched fresh by
  // the engine per edit, so no snapshot id is passed. Errors are engine-side
  // (spclient-native) text with no HTTP status.
  bool EditCreatePlaylist(const std::string& name, PlaylistRef* out,
                          std::string* error, int timeoutMs = 20000);
  bool EditRenamePlaylist(const std::string& id, const std::string& name,
                          std::string* error, int timeoutMs = 20000);
  bool EditDeletePlaylist(const std::string& id, std::string* error,
                          int timeoutMs = 20000);
  bool EditAddPlaylistTracks(const std::string& id,
                             const std::vector<std::string>& uris,
                             std::string* error, int timeoutMs = 20000);
  bool EditRemovePlaylistTracks(const std::string& id,
                                const std::vector<std::string>& uris,
                                std::string* error, int timeoutMs = 20000);
  bool EditReorderPlaylistTracks(const std::string& id, int from, int to,
                                 std::string* error, int timeoutMs = 20000);

 private:
  std::string Send(const std::string& type, nlohmann::json arguments = {});
  // Writes one line-protocol request under an explicit request id; throws
  // std::runtime_error when the transport is unavailable or the write fails.
  void WriteRequest(const std::string& requestId, const std::string& type,
                    nlohmann::json arguments);
  void ReaderLoop();
  void ReportError(std::string message);
  void CloseHandles();

  // One blocking round-trip: the browse_*/edit_* methods register a waiter
  // keyed by request id; the reader thread fills the raw response object and
  // wakes it.
  struct Waiter {
    std::mutex mutex;
    std::condition_variable cv;
    bool done = false;
    bool ok = false;
    std::string error;
    nlohmann::json data;
  };

  // Writes a request and blocks until the reader delivers a matching
  // response. On success returns true with the full response object in
  // `data`; on failure returns false with `error` set. `timeoutMs` bounds
  // the whole round-trip.
  bool RequestData(const std::string& type, nlohmann::json arguments,
                   nlohmann::json* data, std::string* error, int timeoutMs);

  HANDLE process_ = nullptr;
  HANDLE thread_ = nullptr;
  HANDLE input_ = nullptr;
  HANDLE output_ = nullptr;
  std::thread reader_;
  std::mutex write_mutex_;
  std::mutex pending_mutex_;
  std::unordered_set<std::string> pending_requests_;
  std::mutex callback_mutex_;
  StateCallback on_state_;
  ErrorCallback on_error_;
  ResponseCallback on_response_;
  CommandErrorCallback on_command_error_;
  std::mutex waiter_mutex_;
  std::unordered_map<std::string, std::shared_ptr<Waiter>> waiters_;
  std::atomic<uint64_t> next_request_id_{1};
  std::atomic<bool> stopping_{false};
};

std::wstring SiblingPlaybackEnginePath();

}  // namespace sr
