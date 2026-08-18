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

enum class EngineAuthState { Authenticating, Ready, Error };

struct PlaybackEngineState {
  bool ready = false;
  EngineAuthState auth_state = EngineAuthState::Authenticating;
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

struct EngineMessage {
  enum class Kind { Response, State, WebApiToken } kind = Kind::Response;
  std::string request_id;
  bool ok = false;
  std::string error;
  PlaybackEngineState state;
  // Present only on successful web_api_token responses.
  WebApiToken token;
};



TrackRef TrackRefFromEngineJson(const nlohmann::json& value);
nlohmann::json TrackRefToEngineJson(const TrackRef& track);
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

  // Blocking engine round-trip that mints a Web API token (login5) from the
  // engine's Spotify session. Returns false with `error` set when the engine
  // is not running, the mint failed, the response was malformed, or the wait
  // timed out. `timeoutMs` bounds the whole round-trip.
  bool RequestWebApiToken(WebApiToken* out, std::string* error,
                          int timeoutMs = 20000);

 private:
  std::string Send(const std::string& type, nlohmann::json arguments = {});
  // Writes one line-protocol request under an explicit request id; throws
  // std::runtime_error when the transport is unavailable or the write fails.
  void WriteRequest(const std::string& requestId, const std::string& type,
                    nlohmann::json arguments);
  void ReaderLoop();
  void ReportError(std::string message);
  void CloseHandles();

  // Blocks RequestWebApiToken callers until the reader thread delivers the
  // matching web_api_token message.
  struct TokenWaiter {
    std::mutex mutex;
    std::condition_variable cv;
    bool done = false;
    bool ok = false;
    std::string error;
    WebApiToken token;
  };

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
  std::mutex token_wait_mutex_;
  std::unordered_map<std::string, std::shared_ptr<TokenWaiter>> token_waiters_;
  std::atomic<uint64_t> next_request_id_{1};
  std::atomic<bool> stopping_{false};
};

std::wstring SiblingPlaybackEnginePath();

}  // namespace sr
