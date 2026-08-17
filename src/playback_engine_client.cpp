#include "playback_engine_client.h"

#include <algorithm>
#include <limits>
#include <stdexcept>
#include <utility>

#include "util.h"

namespace sr {
namespace {

std::string StringField(const nlohmann::json& value, const char* name) {
  auto it = value.find(name);
  return it != value.end() && it->is_string() ? it->get<std::string>()
                                               : std::string();
}

int64_t IntegerField(const nlohmann::json& value, const char* name,
                     int64_t fallback = 0) {
  auto it = value.find(name);
  return it != value.end() && it->is_number_integer()
             ? it->get<int64_t>()
             : fallback;
}

bool BooleanField(const nlohmann::json& value, const char* name,
                  bool fallback = false) {
  auto it = value.find(name);
  return it != value.end() && it->is_boolean() ? it->get<bool>() : fallback;
}

std::wstring QuoteArgument(const std::wstring& argument) {
  std::wstring result = L"\"";
  size_t slashes = 0;
  for (wchar_t c : argument) {
    if (c == L'\\') {
      ++slashes;
      continue;
    }
    if (c == L'\"') {
      result.append(slashes * 2 + 1, L'\\');
      result.push_back(L'\"');
      slashes = 0;
      continue;
    }
    result.append(slashes, L'\\');
    slashes = 0;
    result.push_back(c);
  }
  result.append(slashes * 2, L'\\');
  result.push_back(L'\"');
  return result;
}

std::wstring ParentDirectory(const std::wstring& path) {
  const size_t separator = path.find_last_of(L"\\/");
  return separator == std::wstring::npos ? std::wstring()
                                         : path.substr(0, separator);
}

std::string Win32Error(const char* operation) {
  return std::string(operation) + " failed (Windows error " +
         std::to_string(::GetLastError()) + ")";
}

void Close(HANDLE* handle) {
  if (*handle && *handle != INVALID_HANDLE_VALUE) ::CloseHandle(*handle);
  *handle = nullptr;
}

}  // namespace

TrackRef TrackRefFromEngineJson(const nlohmann::json& value) {
  if (!value.is_object()) throw std::invalid_argument("track must be an object");
  TrackRef track;
  track.id = StringField(value, "id");
  track.uri = StringField(value, "uri");
  track.name = StringField(value, "name");
  track.artist_id = StringField(value, "artist_id");
  track.album_id = StringField(value, "album_id");
  track.album_name = StringField(value, "album_name");
  track.cover_url = StringField(value, "cover_url");
  const int64_t duration = IntegerField(value, "duration_ms");
  track.duration_ms = static_cast<int>(std::clamp<int64_t>(
      duration, 0, std::numeric_limits<int>::max()));
  auto artists = value.find("artist_names");
  if (artists != value.end() && artists->is_array()) {
    track.artist_names.reserve(artists->size());
    for (const auto& artist : *artists)
      if (artist.is_string()) track.artist_names.push_back(artist.get<std::string>());
  }
  return track;
}

nlohmann::json TrackRefToEngineJson(const TrackRef& track) {
  return {
      {"id", track.id},
      {"uri", track.uri},
      {"name", track.name},
      {"artist_names", track.artist_names},
      {"artist_id", track.artist_id},
      {"album_id", track.album_id},
      {"album_name", track.album_name},
      {"cover_url", track.cover_url},
      {"duration_ms", std::max(0, track.duration_ms)},
  };
}

EngineMessage ParseEngineMessage(const std::string& line) {
  nlohmann::json value = nlohmann::json::parse(line);
  if (!value.is_object()) throw std::invalid_argument("engine message must be an object");
  const std::string type = StringField(value, "type");
  EngineMessage message;
  if (type == "response") {
    message.kind = EngineMessage::Kind::Response;
    message.request_id = StringField(value, "request_id");
    if (message.request_id.empty())
      throw std::invalid_argument("engine response has no request_id");
    message.ok = BooleanField(value, "ok");
    message.error = StringField(value, "error");
    return message;
  }
  if (type != "state") throw std::invalid_argument("unknown engine message type");

  message.kind = EngineMessage::Kind::State;
  PlaybackEngineState& state = message.state;
  state.ready = BooleanField(value, "ready");
  const std::string auth = StringField(value, "auth_state");
  if (auth == "ready")
    state.auth_state = EngineAuthState::Ready;
  else if (auth == "error")
    state.auth_state = EngineAuthState::Error;
  else
    state.auth_state = EngineAuthState::Authenticating;
  state.playing = BooleanField(value, "playing");
  state.position_ms = std::max<int64_t>(0, IntegerField(value, "position_ms"));
  state.duration_ms = std::max<int64_t>(0, IntegerField(value, "duration_ms"));
  state.position_ms = std::min(state.position_ms, state.duration_ms);
  state.volume_percent = static_cast<int>(std::clamp<int64_t>(
      IntegerField(value, "volume", 100), 0, 100));
  state.shuffle = BooleanField(value, "shuffle");
  state.repeat = StringField(value, "repeat");
  if (state.repeat != "off" && state.repeat != "context" &&
      state.repeat != "track")
    state.repeat = "off";
  state.current_index = static_cast<int>(std::clamp<int64_t>(
      IntegerField(value, "current_index", -1), -1,
      std::numeric_limits<int>::max()));
  state.current_uri = StringField(value, "current_uri");
  state.error = StringField(value, "error");
  auto queue = value.find("queue");
  if (queue != value.end() && queue->is_array()) {
    state.queue.reserve(queue->size());
    for (const auto& track : *queue) state.queue.push_back(TrackRefFromEngineJson(track));
  }
  if (state.current_index >= static_cast<int>(state.queue.size()))
    state.current_index = -1;
  return message;
}

nlohmann::json BuildEngineRequest(const std::string& requestId,
                                  const std::string& type,
                                  nlohmann::json arguments) {
  if (requestId.empty() || type.empty())
    throw std::invalid_argument("engine request id and type are required");
  if (arguments.is_null()) arguments = nlohmann::json::object();
  if (!arguments.is_object())
    throw std::invalid_argument("engine request arguments must be an object");
  arguments["request_id"] = requestId;
  arguments["type"] = type;
  return arguments;
}

PlaybackEngineClient::~PlaybackEngineClient() { Shutdown(); }

bool PlaybackEngineClient::Start(const std::wstring& executable,
                                 const std::wstring& stateDirectory,
                                 const std::wstring& diagnosticLog,
                                 StateCallback onState, ErrorCallback onError,
                                 CommandErrorCallback onCommandError,
                                 std::string* error) {
  Shutdown();
  if (executable.empty() || stateDirectory.empty() || diagnosticLog.empty()) {
    if (error) *error = "engine paths are incomplete";
    return false;
  }

  SECURITY_ATTRIBUTES security{sizeof(security), nullptr, TRUE};
  HANDLE childInputRead = nullptr;
  HANDLE childOutputWrite = nullptr;
  if (!::CreatePipe(&childInputRead, &input_, &security, 0) ||
      !::SetHandleInformation(input_, HANDLE_FLAG_INHERIT, 0) ||
      !::CreatePipe(&output_, &childOutputWrite, &security, 0) ||
      !::SetHandleInformation(output_, HANDLE_FLAG_INHERIT, 0)) {
    if (error) *error = Win32Error("creating engine pipes");
    Close(&childInputRead);
    Close(&childOutputWrite);
    CloseHandles();
    return false;
  }

  HANDLE diagnostics = ::CreateFileW(
      diagnosticLog.c_str(), FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE,
      &security, OPEN_ALWAYS,
      FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
  FILE_ATTRIBUTE_TAG_INFO diagnosticAttributes{};
  if (diagnostics == INVALID_HANDLE_VALUE ||
      !::GetFileInformationByHandleEx(diagnostics, FileAttributeTagInfo,
                                      &diagnosticAttributes,
                                      sizeof(diagnosticAttributes)) ||
      (diagnosticAttributes.FileAttributes &
       (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT))) {
    if (diagnostics != INVALID_HANDLE_VALUE) ::CloseHandle(diagnostics);
    if (error) *error = Win32Error("opening engine diagnostic log");
    Close(&childInputRead);
    Close(&childOutputWrite);
    CloseHandles();
    return false;
  }

  STARTUPINFOEXW startup{};
  startup.StartupInfo.cb = sizeof(startup);
  startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
  startup.StartupInfo.wShowWindow = SW_HIDE;
  startup.StartupInfo.hStdInput = childInputRead;
  startup.StartupInfo.hStdOutput = childOutputWrite;
  startup.StartupInfo.hStdError = diagnostics;
  SIZE_T attributeBytes = 0;
  ::InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeBytes);
  std::vector<unsigned char> attributeStorage(attributeBytes);
  startup.lpAttributeList = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(
      attributeStorage.data());
  HANDLE inheritedHandles[] = {childInputRead, childOutputWrite, diagnostics};
  const bool attributesInitialized =
      attributeBytes != 0 &&
      ::InitializeProcThreadAttributeList(startup.lpAttributeList, 1, 0,
                                          &attributeBytes);
  const bool handleListSet =
      attributesInitialized &&
      ::UpdateProcThreadAttribute(
          startup.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
          inheritedHandles, sizeof(inheritedHandles), nullptr, nullptr);
  if (!handleListSet) {
    if (attributesInitialized)
      ::DeleteProcThreadAttributeList(startup.lpAttributeList);
    ::CloseHandle(diagnostics);
    Close(&childInputRead);
    Close(&childOutputWrite);
    if (error) *error = Win32Error("preparing playback engine handles");
    CloseHandles();
    return false;
  }

  PROCESS_INFORMATION process{};
  std::wstring command = QuoteArgument(executable) + L" --state-dir " +
                         QuoteArgument(stateDirectory);
  std::vector<wchar_t> mutableCommand(command.begin(), command.end());
  mutableCommand.push_back(L'\0');
  std::wstring workingDirectory = ParentDirectory(executable);
  const BOOL created = ::CreateProcessW(
      executable.c_str(), mutableCommand.data(), nullptr, nullptr, TRUE,
      CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT, nullptr,
      workingDirectory.empty() ? nullptr : workingDirectory.c_str(),
      &startup.StartupInfo, &process);
  const DWORD createError = ::GetLastError();
  ::DeleteProcThreadAttributeList(startup.lpAttributeList);
  ::CloseHandle(diagnostics);
  Close(&childInputRead);
  Close(&childOutputWrite);
  if (!created) {
    ::SetLastError(createError);
    if (error) *error = Win32Error("starting playback engine");
    CloseHandles();
    return false;
  }

  process_ = process.hProcess;
  thread_ = process.hThread;
  {
    std::lock_guard<std::mutex> lock(callback_mutex_);
    on_state_ = std::move(onState);
    on_error_ = std::move(onError);
    on_command_error_ = std::move(onCommandError);
  }
  stopping_.store(false);
  reader_ = std::thread([this] { ReaderLoop(); });
  try {
    Status();
  } catch (const std::exception& exception) {
    if (error) *error = exception.what();
    Shutdown();
    return false;
  }
  if (error) error->clear();
  return true;
}

void PlaybackEngineClient::Shutdown() {
  if (!process_ && !reader_.joinable()) return;
  stopping_.store(true);
  if (Running()) {
    try {
      Send("shutdown");
    } catch (...) {
    }
  }
  Close(&input_);
  if (process_) {
    DWORD wait = ::WaitForSingleObject(process_, 3000);
    if (wait == WAIT_TIMEOUT) {
      ::TerminateProcess(process_, 1);
      ::WaitForSingleObject(process_, 1000);
    }
  }
  if (reader_.joinable()) reader_.join();
  CloseHandles();
  std::lock_guard<std::mutex> lock(callback_mutex_);
  on_state_ = {};
  on_error_ = {};
  on_command_error_ = {};
}

bool PlaybackEngineClient::Running() const {
  if (!process_) return false;
  DWORD exitCode = 0;
  return ::GetExitCodeProcess(process_, &exitCode) && exitCode == STILL_ACTIVE;
}

std::string PlaybackEngineClient::Status() { return Send("status"); }

std::string PlaybackEngineClient::PlayQueue(const std::vector<TrackRef>& queue,
                                             int index,
                                             int64_t positionMs) {
  if (queue.empty() || index < 0 || index >= static_cast<int>(queue.size()))
    throw std::invalid_argument("play_queue requires a valid selected track");
  if (positionMs < 0 ||
      positionMs > std::numeric_limits<uint32_t>::max())
    throw std::invalid_argument("play_queue position is out of range");
  nlohmann::json tracks = nlohmann::json::array();
  for (const TrackRef& track : queue) {
    if (track.uri.empty())
      throw std::invalid_argument("every queued track requires a URI");
    tracks.push_back(TrackRefToEngineJson(track));
  }
  return Send("play_queue", {{"queue", std::move(tracks)},
                             {"index", index},
                             {"position_ms", positionMs}});
}

std::string PlaybackEngineClient::Play() { return Send("play"); }
std::string PlaybackEngineClient::Pause() { return Send("pause"); }
std::string PlaybackEngineClient::Next() { return Send("next"); }
std::string PlaybackEngineClient::Previous() { return Send("previous"); }
std::string PlaybackEngineClient::Seek(int64_t positionMs) {
  if (positionMs < 0 ||
      positionMs > std::numeric_limits<uint32_t>::max())
    throw std::invalid_argument("seek position is out of range");
  return Send("seek", {{"position_ms", positionMs}});
}
std::string PlaybackEngineClient::SetVolume(int percent) {
  return Send("set_volume", {{"percent", std::clamp(percent, 0, 100)}});
}
std::string PlaybackEngineClient::SetShuffle(bool enabled) {
  return Send("set_shuffle", {{"enabled", enabled}});
}
std::string PlaybackEngineClient::SetRepeat(const std::string& mode) {
  if (mode != "off" && mode != "context" && mode != "track")
    throw std::invalid_argument("invalid repeat mode");
  return Send("set_repeat", {{"mode", mode}});
}
std::string PlaybackEngineClient::AddQueue(const TrackRef& track) {
  if (track.uri.empty()) throw std::invalid_argument("queued track requires a URI");
  return Send("add_queue", {{"track", TrackRefToEngineJson(track)}});
}
std::string PlaybackEngineClient::RemoveQueue(int index) {
  if (index < 0) throw std::invalid_argument("queue index cannot be negative");
  return Send("remove_queue", {{"index", index}});
}
std::string PlaybackEngineClient::MoveQueue(int from, int to) {
  if (from < 0 || to < 0) throw std::invalid_argument("queue indices cannot be negative");
  return Send("move_queue", {{"from", from}, {"to", to}});
}

std::string PlaybackEngineClient::Send(const std::string& type,
                                       nlohmann::json arguments) {
  const std::string requestId = std::to_string(next_request_id_.fetch_add(1));
  std::string line = BuildEngineRequest(requestId, type, std::move(arguments)).dump();
  line.push_back('\n');
  std::lock_guard<std::mutex> lock(write_mutex_);
  if (!input_ || !Running())
    throw std::runtime_error("playback engine is not running");
  {
    std::lock_guard<std::mutex> pendingLock(pending_mutex_);
    pending_requests_.insert(requestId);
  }
  size_t offset = 0;
  while (offset < line.size()) {
    DWORD written = 0;
    const DWORD chunk = static_cast<DWORD>(std::min<size_t>(
        line.size() - offset, std::numeric_limits<DWORD>::max()));
    if (!::WriteFile(input_, line.data() + offset, chunk, &written, nullptr) ||
        written == 0) {
      {
        std::lock_guard<std::mutex> pendingLock(pending_mutex_);
        pending_requests_.erase(requestId);
      }
      const std::string message = Win32Error("writing playback command");
      ReportError(message);
      throw std::runtime_error(message);
    }
    offset += written;
  }
  return requestId;
}

void PlaybackEngineClient::ReaderLoop() {
  std::string pending;
  char buffer[4096];
  for (;;) {
    DWORD count = 0;
    if (!output_ || !::ReadFile(output_, buffer, sizeof(buffer), &count, nullptr) ||
        count == 0)
      break;
    pending.append(buffer, count);
    if (pending.size() > 4 * 1024 * 1024) {
      ReportError("playback engine emitted an oversized protocol line");
      pending.clear();
      continue;
    }
    size_t newline = 0;
    while ((newline = pending.find('\n')) != std::string::npos) {
      std::string line = pending.substr(0, newline);
      pending.erase(0, newline + 1);
      if (!line.empty() && line.back() == '\r') line.pop_back();
      if (line.empty()) continue;
      try {
        EngineMessage message = ParseEngineMessage(line);
        if (message.kind == EngineMessage::Kind::State) {
          StateCallback callback;
          {
            std::lock_guard<std::mutex> lock(callback_mutex_);
            callback = on_state_;
          }
          if (callback) callback(std::move(message.state));
        } else {
          bool expected = false;
          {
            std::lock_guard<std::mutex> lock(pending_mutex_);
            expected = pending_requests_.erase(message.request_id) == 1;
          }
          if (!expected) {
            ReportError("playback engine returned an unknown request_id");
          } else if (!message.ok) {
            CommandErrorCallback callback;
            {
              std::lock_guard<std::mutex> lock(callback_mutex_);
              callback = on_command_error_;
            }
            if (callback)
              callback(message.error.empty() ? "playback command failed"
                                             : std::move(message.error));
          }
        }
      } catch (const std::exception& exception) {
        ReportError(std::string("invalid playback engine message: ") +
                    exception.what());
      }
    }
  }

  if (!stopping_.load()) {
    DWORD exitCode = 0;
    std::string message = "playback engine stopped unexpectedly";
    if (process_ && ::GetExitCodeProcess(process_, &exitCode) &&
        exitCode != STILL_ACTIVE)
      message += " (exit code " + std::to_string(exitCode) + ")";
    ReportError(std::move(message));
  }
}

void PlaybackEngineClient::ReportError(std::string message) {
  ErrorCallback callback;
  {
    std::lock_guard<std::mutex> lock(callback_mutex_);
    callback = on_error_;
  }
  if (callback) callback(std::move(message));
}

void PlaybackEngineClient::CloseHandles() {
  Close(&input_);
  Close(&output_);
  Close(&thread_);
  Close(&process_);
  std::lock_guard<std::mutex> lock(pending_mutex_);
  pending_requests_.clear();
}

std::wstring SiblingPlaybackEnginePath() {
  std::wstring module(32768, L'\0');
  const DWORD length = ::GetModuleFileNameW(nullptr, module.data(),
                                            static_cast<DWORD>(module.size()));
  if (length == 0 || length >= module.size()) return {};
  module.resize(length);
  const std::wstring directory = ParentDirectory(module);
  return directory.empty() ? L"SpotifyPlaybackEngine.exe"
                           : directory + L"\\SpotifyPlaybackEngine.exe";
}

}  // namespace sr
