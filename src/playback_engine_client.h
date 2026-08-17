#pragma once

#include <atomic>
#include <cstdint>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include <unordered_set>

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
  enum class Kind { Response, State } kind = Kind::Response;
  std::string request_id;
  bool ok = false;
  std::string error;
  PlaybackEngineState state;
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

  PlaybackEngineClient() = default;
  ~PlaybackEngineClient();
  PlaybackEngineClient(const PlaybackEngineClient&) = delete;
  PlaybackEngineClient& operator=(const PlaybackEngineClient&) = delete;

  bool Start(const std::wstring& executable, const std::wstring& stateDirectory,
             const std::wstring& diagnosticLog, StateCallback onState,
             ErrorCallback onError, std::string* error = nullptr);
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

 private:
  std::string Send(const std::string& type, nlohmann::json arguments = {});
  void ReaderLoop();
  void ReportError(std::string message);
  void CloseHandles();

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
  std::atomic<uint64_t> next_request_id_{1};
  std::atomic<bool> stopping_{false};
};

std::wstring SiblingPlaybackEnginePath();

}  // namespace sr
