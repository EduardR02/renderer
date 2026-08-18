#include "app.h"

#include <commctrl.h>
#include <objidl.h>
#include <gdiplus.h>
#include <shellapi.h>

#include <algorithm>
#include <stdexcept>
#include <utility>

#include "app_paths.h"
#include "log.h"
#include "util.h"

namespace sr {
namespace {

constexpr UINT kSmokeTimer = 1;
// Drives local position projection between engine state events (4 Hz).
constexpr UINT kPositionTimer = 2;


std::pair<std::string, std::string> SplitOrigin(const std::string& url) {
  const size_t scheme = url.find("://");
  if (scheme == std::string::npos) return {};
  const size_t slash = url.find('/', scheme + 3);
  return slash == std::string::npos
             ? std::make_pair(url, std::string("/"))
             : std::make_pair(url.substr(0, slash), url.substr(slash));
}

void ApplyAlbumMetadata(std::vector<TrackRef>& tracks, const AlbumRef& album) {
  for (TrackRef& track : tracks) {
    if (track.album_id.empty()) track.album_id = album.id;
    if (track.album_name.empty()) track.album_name = album.name;
    if (track.cover_url.empty()) track.cover_url = album.cover_url;
  }
}

}  // namespace

void PositionProjector::Reset(int64_t positionMs, bool playing,
                              ULONGLONG nowTick) {
  base_position_ms_ = positionMs;
  playing_ = playing;
  base_tick_ = nowTick;
}

int64_t PositionProjector::Current(ULONGLONG nowTick,
                                   int64_t durationMs) const {
  if (!playing_ || durationMs <= 0) return base_position_ms_;
  const int64_t elapsed =
      nowTick >= base_tick_ ? static_cast<int64_t>(nowTick - base_tick_) : 0;
  return std::clamp<int64_t>(base_position_ms_ + elapsed, 0, durationMs);
}

namespace {

// Engine state field names used by PlaybackStateReconciler. They match the
// engine protocol field names so both sides stay easy to map.
constexpr const char* kOverridePlaying = "playing";
constexpr const char* kOverridePosition = "position_ms";
constexpr const char* kOverrideDuration = "duration_ms";
constexpr const char* kOverrideVolume = "volume";
constexpr const char* kOverrideShuffle = "shuffle";
constexpr const char* kOverrideRepeat = "repeat";
constexpr const char* kOverrideCurrentIndex = "current_index";
constexpr const char* kOverrideCurrentUri = "current_uri";
constexpr const char* kOverrideQueue = "queue";

void CopyField(const std::string& field, const PlaybackEngineState& from,
               PlaybackEngineState* to) {
  if (field == kOverridePlaying)
    to->playing = from.playing;
  else if (field == kOverridePosition)
    to->position_ms = from.position_ms;
  else if (field == kOverrideDuration)
    to->duration_ms = from.duration_ms;
  else if (field == kOverrideVolume)
    to->volume_percent = from.volume_percent;
  else if (field == kOverrideShuffle)
    to->shuffle = from.shuffle;
  else if (field == kOverrideRepeat)
    to->repeat = from.repeat;
  else if (field == kOverrideCurrentIndex)
    to->current_index = from.current_index;
  else if (field == kOverrideCurrentUri)
    to->current_uri = from.current_uri;
  else if (field == kOverrideQueue)
    to->queue = from.queue;
}

}  // namespace

void PlaybackStateReconciler::SetOverride(const std::string& field,
                                          const std::string& requestId) {
  if (requestId.empty())
    fields_.erase(field);
  else
    fields_[field] = requestId;
}

void PlaybackStateReconciler::Confirm(const std::string& requestId) {
  for (auto it = fields_.begin(); it != fields_.end();) {
    if (it->second == requestId)
      it = fields_.erase(it);
    else
      ++it;
  }
}

bool PlaybackStateReconciler::Overridden(const std::string& field) const {
  return fields_.count(field) != 0;
}

const std::unordered_map<std::string, std::string>&
PlaybackStateReconciler::PendingFields() const {
  return fields_;
}

void PlaybackStateReconciler::Reset() { fields_.clear(); }

bool PlaybackStateReconciler::HasPending() const { return !fields_.empty(); }

PlaybackEngineState ReconcileEngineState(PlaybackEngineState incoming,
                                         const PlaybackEngineState& current,
                                         const PlaybackStateReconciler& overrides) {
  for (const auto& entry : overrides.PendingFields())
    CopyField(entry.first, current, &incoming);
  return incoming;
}

namespace {

bool SameQueue(const std::vector<TrackRef>& left,
               const std::vector<TrackRef>& right) {
  if (left.size() != right.size()) return false;
  for (size_t i = 0; i < left.size(); ++i) {
    const TrackRef& a = left[i];
    const TrackRef& b = right[i];
    if (a.id != b.id || a.uri != b.uri || a.name != b.name ||
        a.artist_names != b.artist_names || a.artist_id != b.artist_id ||
        a.album_id != b.album_id || a.album_name != b.album_name ||
        a.cover_url != b.cover_url || a.duration_ms != b.duration_ms)
      return false;
  }
  return true;
}

}  // namespace

void TaskQueue::Start() {
  Stop();
  stop_.store(false);
  thread_ = std::thread([this] {
    for (;;) {
      std::function<void()> task;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [this] { return stop_.load() || !tasks_.empty(); });
        if (stop_.load() && tasks_.empty()) return;
        task = std::move(tasks_.front());
        tasks_.pop_front();
      }
      task();
    }
  });
}

void TaskQueue::Stop() {
  stop_.store(true);
  wake_.notify_all();
  if (thread_.joinable()) thread_.join();
  std::lock_guard<std::mutex> lock(mutex_);
  tasks_.clear();
}

void TaskQueue::DiscardPending() {
  std::lock_guard<std::mutex> lock(mutex_);
  tasks_.clear();
}

void TaskQueue::Post(std::function<void()> task) {
  if (!task || stop_.load()) return;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    tasks_.push_back(std::move(task));
  }
  wake_.notify_one();
}

namespace {

constexpr std::chrono::minutes kPlaylistListTtl(10);

std::wstring FormatByteSize(uint64_t bytes) {
  constexpr uint64_t kMiB = 1024ull * 1024;
  constexpr uint64_t kGiB = 1024ull * kMiB;
  if (bytes >= kGiB) {
    const uint64_t whole = bytes / kGiB;
    const uint64_t frac = (bytes % kGiB) * 100 / kGiB;
    const std::wstring padded = frac < 10 ? L"0" + std::to_wstring(frac)
                                          : std::to_wstring(frac);
    return std::to_wstring(whole) + L"." + padded + L" GB";
  }
  return std::to_wstring(bytes / kMiB) + L" MB";
}

std::wstring CacheUsageText(uint64_t bytes) {
  return L"Audio: Ogg Vorbis 320 kbps  ·  WASAPI  ·  cache " +
         FormatByteSize(bytes) + L" / 1 GiB";
}

}  // namespace

TrackListCache::TrackListCache(size_t capacity, Clock::duration ttl, NowFn now)
    : capacity_(std::max<size_t>(capacity, 1)),
      ttl_(ttl),
      now_(std::move(now)) {}

bool TrackListCache::Get(const std::string& key, CachedTrackList* out) {
  auto found = index_.find(key);
  if (found == index_.end()) return false;
  auto it = found->second;
  if (now_() - it->second.fetched >= ttl_) {
    order_.erase(it);
    index_.erase(found);
    return false;
  }
  order_.splice(order_.begin(), order_, it);
  if (out) *out = it->second.value;
  return true;
}

void TrackListCache::Put(const std::string& key, CachedTrackList value) {
  auto found = index_.find(key);
  if (found != index_.end()) order_.erase(found->second);
  order_.emplace_front(key, Entry{std::move(value), now_()});
  index_[key] = order_.begin();
  while (order_.size() > capacity_) {
    index_.erase(order_.back().first);
    order_.pop_back();
  }
}

void TrackListCache::Invalidate(const std::string& key) {
  auto found = index_.find(key);
  if (found == index_.end()) return;
  order_.erase(found->second);
  index_.erase(found);
}

void TrackListCache::Clear() {
  order_.clear();
  index_.clear();
}

size_t TrackListCache::Size() const { return order_.size(); }

bool IsDevModePlaylistRestriction(int status, const std::string& ownerId,
                                  const std::string& meId) {
  return status == 403 && !ownerId.empty() && !meId.empty() && ownerId != meId;
}

Application::~Application() { Shutdown(); }

int Application::Run(HINSTANCE instance, int show, const RunOptions& options) {
  instance_ = instance;
  options_ = options;

  INITCOMMONCONTROLSEX controls{
      sizeof(controls),
      ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES};
  ::InitCommonControlsEx(&controls);
  Gdiplus::GdiplusStartupInput input;
  if (Gdiplus::GdiplusStartup(&gdiplus_token_, &input, nullptr) != Gdiplus::Ok)
    gdiplus_token_ = 0;

  if (options_.smoke || options_.demo) {
    settings_ = Settings{};
    tokens_.reset();
    log::SetConsole(true);
  } else {
    InitCore();
  }

  if (!window_.Create(instance_, this)) {
    LOG_ERROR("failed to create main window");
    Shutdown();
    return 2;
  }
  hwnd_ = window_.hwnd();
  tray_.Create(hwnd_, L"SpotifyRenderer — local engine starting");
  window_.SetSetupMode(settings_.client_id.empty());
  if (options_.demo) {
    window_.SetSetupMode(false);
    window_.SetDemo();
  } else if (!options_.smoke) {
    StartEngine();
  }

  window_.Show(true);
  ::ShowWindow(hwnd_, options_.smoke ? SW_SHOWNOACTIVATE : show);
  ::UpdateWindow(hwnd_);
  StartTimers();
  if (IsAuthed() && !options_.smoke && !options_.demo) OnRefreshAll();
  if (options_.smoke || options_.demo)
    LOG_INFO(std::string("isolated ") +
             (options_.smoke ? "smoke" : "demo") + " launch: " +
             IsolationStatus());

  MSG message{};
  while (::GetMessageW(&message, nullptr, 0, 0) > 0) {
    if ((!accelerators_ ||
         !::TranslateAcceleratorW(hwnd_, accelerators_, &message)) &&
        !::IsDialogMessageW(hwnd_, &message)) {
      ::TranslateMessage(&message);
      ::DispatchMessageW(&message);
    }
  }
  Shutdown();
  return static_cast<int>(message.wParam);
}

void Application::InitCore() {
  if (!paths::EnsureDirs()) return;
  log::Init(paths::LogFile());
  settings_ = LoadSettings(paths::SettingsFile());
  tokens_ = LoadTokenSet(paths::TokensFile());
  api_http_ = std::make_shared<HttpClient>("https://api.spotify.com");
  accounts_http_ = std::make_shared<HttpClient>("https://accounts.spotify.com");
  api_ = std::make_unique<SpotifyApi>(
      api_http_, accounts_http_, [this] { return GetAccessToken(); },
      [this](int timeout) { return RefreshToken(timeout); });
  api_tasks_.Start();
  artwork_tasks_.Start();
}

void Application::StartEngine() {
  if (shutting_down_) return;
  std::string error;
  if (!engine_.Start(
          SiblingPlaybackEnginePath(), paths::EngineStateDir(),
          paths::EngineLogFile(),
          [this](PlaybackEngineState state) {
            PostUi([this, state = std::move(state)]() mutable {
              OnEngineState(std::move(state));
            });
          },
          [this](std::string message) {
            PostUi([this, message = std::move(message)]() mutable {
              OnEngineError(std::move(message));
            });
          },
          [this](const std::string& requestId, bool) {
            PostUi([this, requestId]() mutable {
              // The engine has acknowledged the command; the state event that
              // follows it in the stream is authoritative, so the optimistic
              // overrides for this request are released.
              playback_overrides_.Confirm(requestId);
            });
          },
          [this](std::string message) {
            PostUi([this, message = std::move(message)]() mutable {
              OnEngineCommandError(std::move(message));
            });
          },
          &error)) {
    playback_.ready = false;
    playback_.auth_state = EngineAuthState::Error;
    playback_.error = error;
    UpdatePlaybackUi();
    window_.SetEngineStatus(EngineStatusText());
    window_.SetStatus(L"Local playback engine could not start: " +
                      Utf8ToWide(error));
  } else {
    window_.SetStatus(
        L"Local playback engine is starting; complete its browser sign-in if prompted.");
    window_.SetEngineStatus(EngineStatusText());
  }
}

void Application::Shutdown() {
  if (shutting_down_) return;
  shutting_down_ = true;
  engine_restart_pending_ = false;
  StopTimers();
  oauth_listener_.Stop();
  artwork_tasks_.DiscardPending();
  api_tasks_.Stop();
  artwork_tasks_.Stop();
  engine_.Shutdown();
  tray_.Destroy();
  api_.reset();
  accounts_http_.reset();
  api_http_.reset();
  if (window_.hwnd()) window_.Destroy();
  hwnd_ = nullptr;
  if (gdiplus_token_) {
    Gdiplus::GdiplusShutdown(gdiplus_token_);
    gdiplus_token_ = 0;
  }
  log::Close();
}

void Application::StartTimers() {
  if (!hwnd_) return;
  if (options_.smoke)
    ::SetTimer(hwnd_, kSmokeTimer, std::max(1, options_.smokeSeconds) * 1000,
               nullptr);
  ::SetTimer(hwnd_, kPositionTimer, 250, nullptr);
}

void Application::StopTimers() {
  if (!hwnd_) return;
  ::KillTimer(hwnd_, kSmokeTimer);
  ::KillTimer(hwnd_, kPositionTimer);
}

bool Application::IsAuthed() const {
  std::lock_guard<std::mutex> lock(tokens_mutex_);
  return tokens_.has_value() && !tokens_->access_token.empty();
}

Settings Application::GetSettings() const {
  std::lock_guard<std::mutex> lock(settings_mutex_);
  return settings_;
}

void Application::PostUi(std::function<void()> function) {
  HWND target = hwnd_;
  if (!target || shutting_down_) return;
  auto* heap = new std::function<void()>(std::move(function));
  if (!::PostMessageW(target, WM_SR_RUN, 0, reinterpret_cast<LPARAM>(heap)))
    delete heap;
}

void Application::OnSetupSave(const std::string& clientId,
                              const std::string& redirectUri) {
  if (options_.smoke || options_.demo) return;
  std::string host;
  uint16_t port = 0;
  if (clientId.empty() || clientId.size() > 256) {
    window_.SetSetupStatus(
        L"Enter the Client ID shown in your Spotify developer app.");
    return;
  }
  if (!ParseLoopbackUri(redirectUri, &host, &port)) {
    window_.SetSetupStatus(
        L"Use a loopback redirect such as http://127.0.0.1:4382/callback.");
    return;
  }
  bool saved = false;
  {
    std::lock_guard<std::mutex> lock(settings_mutex_);
    settings_.client_id = clientId;
    settings_.redirect_uri = redirectUri;
    saved = SaveSettings(paths::SettingsFile(), settings_);
  }
  window_.SetSetupStatus(
      saved ? L"Saved. Confirm the dashboard redirect URI, then authenticate browsing."
            : L"Could not save settings.");
}

void Application::OnAuthenticate() {
  if (options_.smoke || options_.demo) return;
  std::string host;
  uint16_t port = 0;
  if (settings_.client_id.empty() ||
      !ParseLoopbackUri(settings_.redirect_uri, &host, &port)) {
    window_.SetSetupStatus(
        L"Save the Client ID and exact loopback redirect URI first.");
    return;
  }
  oauth_listener_.Stop();
  try {
    pending_pkce_ = GeneratePkce();
    pending_state_ = RandomHex(24);
  } catch (const std::exception&) {
    pending_pkce_ = {};
    pending_state_.clear();
    window_.SetSetupStatus(
        L"Secure random or PKCE hashing failed; authorization was not started.");
    return;
  }
  std::string error;
  if (!oauth_listener_.Start(
          port, pending_state_,
          [this](std::string result) {
            HWND target = hwnd_;
            auto* value = new std::string(std::move(result));
            if (!target ||
                !::PostMessageW(target, WM_SR_OAUTH_DONE, 0,
                                reinterpret_cast<LPARAM>(value)))
              delete value;
          },
          &error)) {
    window_.SetSetupStatus(
        L"Could not start the callback listener. The redirect port may be in use.");
    return;
  }
  const std::string url =
      BuildAuthorizeUrl(settings_.client_id, settings_.redirect_uri,
                        pending_pkce_.challenge, pending_state_);
  if (reinterpret_cast<INT_PTR>(::ShellExecuteW(
          nullptr, L"open", Utf8ToWide(url).c_str(), nullptr, nullptr,
          SW_SHOWNORMAL)) <= 32) {
    oauth_listener_.Stop();
    window_.SetSetupStatus(L"Could not open the authorization page.");
    return;
  }
  window_.SetSetupStatus(L"Complete browsing authorization in your browser.");
}

void Application::OnOAuthResult(const std::string& result) {
  oauth_listener_.Stop();
  if (StartsWith(result, "error: ")) {
    pending_pkce_ = {};
    pending_state_.clear();
    window_.SetSetupStatus(L"Authorization failed: " +
                           Utf8ToWide(result.substr(7)));
    return;
  }
  const std::string verifier = std::exchange(pending_pkce_.verifier, {});
  pending_pkce_.challenge.clear();
  pending_state_.clear();
  if (verifier.empty()) {
    window_.SetSetupStatus(L"Authorization session expired. Try again.");
    return;
  }
  window_.SetSetupStatus(L"Exchanging authorization code...");
  PostTask<TokenResponse>(
      [this, code = result, verifier] {
        std::string error;
        auto token = ExchangeCode(*accounts_http_, settings_.client_id,
                                  settings_.redirect_uri, code, verifier, &error);
        if (!token) throw std::runtime_error(error);
        return *token;
      },
      [this](TokenResponse token) {
        try {
          SaveTokens(token);
        } catch (const std::exception& error) {
          window_.SetSetupStatus(L"Could not protect tokens: " +
                                 Utf8ToWide(error.what()));
          return;
        }
        window_.SetSetupMode(false);
        window_.SetStatus(L"Browsing authenticated");
        OnRefreshAll();
      },
      [this](std::string message, int, int) {
        window_.SetSetupStatus(L"Token exchange failed: " +
                               Utf8ToWide(message));
      });
}

std::string Application::GetAccessToken() const {
  std::lock_guard<std::mutex> lock(tokens_mutex_);
  return tokens_ ? tokens_->access_token : std::string();
}

bool Application::RefreshToken(int timeoutMs) {
  std::string refresh;
  {
    std::lock_guard<std::mutex> lock(tokens_mutex_);
    if (!tokens_ || !tokens_->has_refresh || tokens_->refresh_token.empty())
      return false;
    refresh = tokens_->refresh_token;
  }
  std::string error;
  auto token = RefreshAccessToken(*accounts_http_, settings_.client_id, refresh,
                                  &error, timeoutMs);
  if (!token) {
    LOG_WARN("token refresh failed: " + error);
    return false;
  }
  if (!token->has_refresh) {
    token->refresh_token = refresh;
    token->has_refresh = true;
  }
  try {
    SaveTokens(*token);
    return true;
  } catch (const std::exception& exception) {
    LOG_ERROR(std::string("token protection failed: ") + exception.what());
    return false;
  }
}

void Application::SaveTokens(const TokenResponse& token) {
  TokenSet stored;
  stored.access_token = token.access_token;
  stored.refresh_token = token.refresh_token;
  stored.expires_at = token.expires_at;
  stored.has_refresh = token.has_refresh && !token.refresh_token.empty();
  if (!SaveTokenSet(paths::TokensFile(), stored))
    throw std::runtime_error("DPAPI token save failed");
  std::lock_guard<std::mutex> lock(tokens_mutex_);
  tokens_ = std::move(stored);
}

void Application::ClearTokens() {
  std::lock_guard<std::mutex> lock(tokens_mutex_);
  tokens_.reset();
  paths::DeleteOwnedFile(paths::TokensFile());
}

void Application::HandleApiError(const std::string& message, int status,
                                 int retryAfter,
                                 const std::wstring& context) {
  LOG_ERROR(WideToUtf8(context) + ": " + message +
            (status ? " (HTTP " + std::to_string(status) + ")" : ""));
  if (status == 401) {
    ClearTokens();
    window_.SetSetupMode(true);
  }
  std::wstring text = context + L": " + Utf8ToWide(message);
  if (status == 429 && retryAfter > 0)
    text += L"; retry after " + std::to_wstring(retryAfter) + L" seconds";
  window_.SetStatus(text);
}

void Application::OnSearch(const std::string& query) {
  if (!IsAuthed()) {
    window_.SetSetupMode(true);
    return;
  }
  if (Trim(query).empty()) return;
  window_.SetStatus(L"Searching...");
  PostTask<SearchResult>(
      [this, query] { return api_->Search(query); },
      [this](SearchResult result) {
        window_.SetSearchResults(result);
        window_.SetStatus(L"Search complete");
      },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Search");
      });
}

void Application::OnSearchActivate(int item) {
  const auto& kinds = window_.resultKinds();
  const SearchResult& result = window_.search();
  if (item < 0 || static_cast<size_t>(item) >= kinds.size()) return;
  size_t index = 0;
  for (int i = 0; i < item; ++i)
    if (kinds[i] == kinds[item]) ++index;
  if (kinds[item] == 0 && index < result.tracks.size()) {
    PlayTracks(result.tracks, static_cast<int>(index));
  } else if (kinds[item] == 1 && index < result.albums.size()) {
    OpenAlbumTracks(result.albums[index]);
  } else if (kinds[item] == 2 && index < result.artists.size()) {
    const ArtistRef artist = result.artists[index];
    PostTask<std::vector<TrackRef>>(
        [this, id = artist.id] { return api_->GetArtistTopTracks(id); },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
        [this](std::string message, int status, int retry) {
          HandleApiError(message, status, retry, L"Artist top tracks");
        });
  }
}

void Application::OnSearchContext(UINT command, int item) {
  if (command >= IDM_CTX_ADD_PLAYLIST_BASE &&
      command < IDM_CTX_ADD_PLAYLIST_BASE + 64) {
    OnAddToPlaylist(static_cast<int>(command - IDM_CTX_ADD_PLAYLIST_BASE));
    return;
  }
  const auto& kinds = window_.resultKinds();
  const SearchResult& result = window_.search();
  if (item < 0 || static_cast<size_t>(item) >= kinds.size()) return;
  size_t index = 0;
  for (int i = 0; i < item; ++i)
    if (kinds[i] == kinds[item]) ++index;
  if (kinds[item] == 1 || kinds[item] == 2) {
    if (command == IDM_CTX_OPEN_ALBUM || command == IDM_CTX_ARTIST_ALBUMS)
      OnSearchActivate(item);
    return;
  }
  if (index >= result.tracks.size()) return;
  const TrackRef track = result.tracks[index];
  if (command == IDM_CTX_PLAY_TRACK) {
    PlayTracks(result.tracks, static_cast<int>(index));
  } else if (command == IDM_CTX_ADD_QUEUE) {
    if (!EngineReady()) return;
    playback_.queue.push_back(track);
    RefreshQueue();
    const std::string requestId = RunEngineCommand(
        [this, track] { return engine_.AddQueue(track); }, L"Add to queue");
    if (!requestId.empty())
      playback_overrides_.SetOverride(kOverrideQueue, requestId);
  } else if (command == IDM_CTX_OPEN_ALBUM && !track.album_id.empty()) {
    OpenAlbumTracks(AlbumRef{track.album_id, "spotify:album:" + track.album_id,
                             track.album_name, track.artist_names,
                             track.cover_url});
  } else if (command == IDM_CTX_ARTIST_ALBUMS && !track.artist_id.empty()) {
    ArtistRef artist{track.artist_id, "spotify:artist:" + track.artist_id,
                     track.artist_names.empty() ? std::string{}
                                                : track.artist_names.front(),
                     track.cover_url};
    PostTask<std::vector<TrackRef>>(
        [this, id = artist.id] { return api_->GetArtistTopTracks(id); },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
        [this](std::string message, int status, int retry) {
          HandleApiError(message, status, retry, L"Artist top tracks");
        });
  }
}

void Application::OnAddToPlaylist(int playlistIndex) {
  const int selected = window_.SelectedResultIndex();
  const auto& kinds = window_.resultKinds();
  if (playlistIndex < 0 ||
      static_cast<size_t>(playlistIndex) >= playlists_.size() || selected < 0 ||
      static_cast<size_t>(selected) >= kinds.size() || kinds[selected] != 0)
    return;
  size_t trackIndex = 0;
  for (int i = 0; i < selected; ++i)
    if (kinds[i] == 0) ++trackIndex;
  if (trackIndex >= window_.search().tracks.size()) return;
  const std::string playlist = playlists_[playlistIndex].id;
  const std::string uri = window_.search().tracks[trackIndex].uri;
  PostTask<bool>(
      [this, playlist, uri] {
        api_->AddTracksToPlaylist(playlist, {uri});
        return true;
      },
      [this, playlist](bool) {
        track_cache_.Invalidate("p:" + playlist);
        window_.SetStatus(L"Added to playlist");
      },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Add to playlist");
      });
}

void Application::OnMiddleCombo(int index) {
  if (index <= 0) {
    middle_mode_ = MiddleMode::Queue;
    RefreshQueue();
    return;
  }
  if (static_cast<size_t>(index - 1) >= playlists_.size()) return;
  current_playlist_id_ = playlists_[index - 1].id;
  middle_mode_ = MiddleMode::Playlist;
  RequestPlaylistTracks(current_playlist_id_);
}

void Application::OnMiddleActivate(int index) {
  const auto& tracks = window_.middleTracks();
  if (index < 0 || static_cast<size_t>(index) >= tracks.size()) return;
  PlayTracks(tracks, index);
}

void Application::OnMiddleContext(UINT command, int index) {
  const auto& tracks = window_.middleTracks();
  if (index < 0 || static_cast<size_t>(index) >= tracks.size()) return;
  const TrackRef track = tracks[index];
  if (command == IDM_CTX_PLAY_MIDDLE) {
    OnMiddleActivate(index);
  } else if (command == IDM_CTX_MIDDLE_ADD_QUEUE) {
    if (!EngineReady()) return;
    playback_.queue.push_back(track);
    RefreshQueue();
    const std::string requestId = RunEngineCommand(
        [this, track] { return engine_.AddQueue(track); }, L"Add to queue");
    if (!requestId.empty())
      playback_overrides_.SetOverride(kOverrideQueue, requestId);
  } else if (middle_mode_ == MiddleMode::Queue &&
             command == IDM_CTX_MIDDLE_REMOVE) {
    if (!EngineReady()) return;
    playback_.queue.erase(playback_.queue.begin() + index);
    if (playback_.current_index == index) {
      if (playback_.queue.empty()) {
        playback_.current_index = -1;
        playback_.current_uri.clear();
        playback_.position_ms = 0;
        playback_.duration_ms = 0;
        playback_.playing = false;
      } else {
        playback_.current_index =
            std::min(index, static_cast<int>(playback_.queue.size()) - 1);
        const TrackRef& replacement =
            playback_.queue[playback_.current_index];
        playback_.current_uri = replacement.uri;
        playback_.position_ms = 0;
        playback_.duration_ms = replacement.duration_ms;
      }
    } else if (playback_.current_index > index) {
      --playback_.current_index;
    }
    UpdatePlaybackUi();
    RefreshQueue();
    const std::string requestId = RunEngineCommand(
        [this, index] { return engine_.RemoveQueue(index); },
        L"Remove from queue");
    if (!requestId.empty())
      playback_overrides_.SetOverride(kOverrideQueue, requestId);
  } else if (middle_mode_ == MiddleMode::Queue &&
             (command == IDM_CTX_MIDDLE_UP ||
              command == IDM_CTX_MIDDLE_DOWN)) {
    if (!EngineReady()) return;
    const int destination =
        command == IDM_CTX_MIDDLE_UP ? index - 1 : index + 1;
    if (destination < 0 || destination >= static_cast<int>(tracks.size()))
      return;
    TrackRef moved = std::move(playback_.queue[index]);
    playback_.queue.erase(playback_.queue.begin() + index);
    playback_.queue.insert(playback_.queue.begin() + destination,
                           std::move(moved));
    if (playback_.current_index == index)
      playback_.current_index = destination;
    else if (index < playback_.current_index &&
             destination >= playback_.current_index)
      --playback_.current_index;
    else if (index > playback_.current_index &&
             destination <= playback_.current_index)
      ++playback_.current_index;
    RefreshQueue();
    const std::string requestId = RunEngineCommand(
        [this, index, destination] {
          return engine_.MoveQueue(index, destination);
        },
        L"Move queue item");
    if (!requestId.empty())
      playback_overrides_.SetOverride(kOverrideQueue, requestId);
  } else if (command == IDM_CTX_OPEN_ALBUM && !track.album_id.empty()) {
    OpenAlbumTracks(AlbumRef{track.album_id, "spotify:album:" + track.album_id,
                             track.album_name, track.artist_names,
                             track.cover_url});
  } else if (command == IDM_CTX_ARTIST_ALBUMS && !track.artist_id.empty()) {
    ArtistRef artist{track.artist_id, "spotify:artist:" + track.artist_id,
                     track.artist_names.empty() ? std::string{}
                                                : track.artist_names.front(),
                     track.cover_url};
    PostTask<std::vector<TrackRef>>(
        [this, id = artist.id] { return api_->GetArtistTopTracks(id); },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
        [this](std::string message, int status, int retry) {
          HandleApiError(message, status, retry, L"Artist top tracks");
        });
  } else if (middle_mode_ == MiddleMode::Playlist &&
             command == IDM_CTX_MIDDLE_REMOVE) {
    PostTask<bool>(
        [this, uri = track.uri] {
          api_->RemoveTracksFromPlaylist(current_playlist_id_, {uri},
                                         current_playlist_snapshot_);
          return true;
        },
        [this](bool) {
          track_cache_.Invalidate("p:" + current_playlist_id_);
          RequestPlaylistTracks(current_playlist_id_);
        },
        [this](std::string message, int status, int retry) {
          HandleApiError(message, status, retry, L"Remove track");
        });
  } else if (middle_mode_ == MiddleMode::Playlist &&
             (command == IDM_CTX_MIDDLE_UP ||
              command == IDM_CTX_MIDDLE_DOWN)) {
    const int destination = command == IDM_CTX_MIDDLE_UP ? index - 1 : index + 2;
    if (destination < 0 || destination > static_cast<int>(tracks.size())) return;
    PostTask<bool>(
        [this, index, destination] {
          api_->ReorderPlaylistTracks(current_playlist_id_, index, destination,
                                      1, current_playlist_snapshot_);
          return true;
        },
        [this](bool) {
          track_cache_.Invalidate("p:" + current_playlist_id_);
          RequestPlaylistTracks(current_playlist_id_);
        },
        [this](std::string message, int status, int retry) {
          HandleApiError(message, status, retry, L"Reorder playlist");
        });
  }
}

void Application::OnBack() {
  middle_mode_ = MiddleMode::Queue;
  window_.SetMiddleLabel(L"Queue");
  window_.SetMiddleMode(0);
  RefreshQueue();
}

void Application::OnNewPlaylist() {
  auto name = window_.PromptText(hwnd_, L"New playlist", L"");
  if (!name || Trim(WideToUtf8(*name)).empty()) return;
  PostTask<PlaylistRef>(
      [this, value = WideToUtf8(*name)] {
        if (me_id_.empty()) me_id_ = api_->GetMeId();
        return api_->CreatePlaylist(me_id_, value);
      },
      [this](PlaylistRef) { RefreshPlaylists(true); },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Create playlist");
      });
}

void Application::OnRenamePlaylist() {
  const int index = window_.MiddleComboIndex() - 1;
  if (index < 0 || static_cast<size_t>(index) >= playlists_.size()) return;
  auto name = window_.PromptText(hwnd_, L"Rename playlist",
                                 Utf8ToWide(playlists_[index].name));
  if (!name || Trim(WideToUtf8(*name)).empty()) return;
  PostTask<bool>(
      [this, id = playlists_[index].id, value = WideToUtf8(*name)] {
        api_->RenamePlaylist(id, value);
        return true;
      },
      [this](bool) { RefreshPlaylists(true); },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Rename playlist");
      });
}

void Application::OnDeletePlaylist() {
  const int index = window_.MiddleComboIndex() - 1;
  if (index < 0 || static_cast<size_t>(index) >= playlists_.size()) return;
  if (::MessageBoxW(hwnd_, L"Unfollow this playlist?", L"SpotifyRenderer",
                    MB_ICONWARNING | MB_OKCANCEL) != IDOK)
    return;
  PostTask<bool>(
      [this, id = playlists_[index].id] {
        api_->DeletePlaylist(id);
        return true;
      },
      [this, id = playlists_[index].id](bool) {
        track_cache_.Invalidate("p:" + id);
        middle_mode_ = MiddleMode::Queue;
        RefreshPlaylists(true);
        RefreshQueue();
      },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Delete playlist");
      });
}

void Application::PlayTracks(const std::vector<TrackRef>& tracks, int index) {
  if (tracks.empty() || index < 0 || index >= static_cast<int>(tracks.size()))
    return;
  if (!EngineReady()) return;
  playback_.queue = tracks;
  playback_.current_index = index;
  playback_.current_uri = tracks[index].uri;
  playback_.duration_ms = std::max(0, tracks[index].duration_ms);
  playback_.position_ms = 0;
  playback_.playing = true;
  ResetProjectionBase();
  UpdatePlaybackUi();
  RefreshQueue();
  const std::string requestId = RunEngineCommand(
      [this, index] { return engine_.PlayQueue(playback_.queue, index); },
      L"Start playback");
  if (!requestId.empty()) {
    playback_overrides_.SetOverride(kOverrideQueue, requestId);
    playback_overrides_.SetOverride(kOverrideCurrentIndex, requestId);
    playback_overrides_.SetOverride(kOverrideCurrentUri, requestId);
    playback_overrides_.SetOverride(kOverridePosition, requestId);
    playback_overrides_.SetOverride(kOverrideDuration, requestId);
    playback_overrides_.SetOverride(kOverridePlaying, requestId);
  }
}

std::string Application::RunEngineCommand(
    const std::function<std::string()>& command,
    const std::wstring& failureContext) {
  if (options_.smoke || options_.demo) return {};
  try {
    return command();
  } catch (const std::exception& error) {
    window_.SetStatus(failureContext + L": " + Utf8ToWide(error.what()));
    return {};
  }
}

bool Application::EngineReady() {
  if (options_.demo) return true;
  if (playback_.ready && engine_.Running()) return true;
  if (!engine_.Running()) {
    window_.SetStatus(
        L"Local playback engine is not running; it restarts automatically.");
    ScheduleEngineRestart();
    return false;
  }
  if (playback_.auth_state == EngineAuthState::Error) TryRecoverEngine();
  window_.SetStatus(
      engine_restart_pending_
          ? L"Local playback engine is restarting; playback resumes "
            L"automatically."
          : L"Local playback engine is not ready. Complete its browser "
            L"sign-in or check Settings.");
  return false;
}

void Application::OnTogglePlay() {
  if (!EngineReady()) return;
  const bool play = !playback_.playing;
  playback_.playing = play;
  ResetProjectionBase();
  UpdatePlaybackUi();
  const std::string requestId =
      RunEngineCommand([this, play] { return play ? engine_.Play() : engine_.Pause(); },
                       play ? L"Play" : L"Pause");
  if (!requestId.empty()) playback_overrides_.SetOverride(kOverridePlaying, requestId);
}

void Application::OnNext() {
  if (!EngineReady()) return;
  bool advanced = false;
  if (!playback_.shuffle && playback_.current_index >= 0 &&
      playback_.current_index + 1 < static_cast<int>(playback_.queue.size())) {
    ++playback_.current_index;
    playback_.current_uri = playback_.queue[playback_.current_index].uri;
    playback_.position_ms = 0;
    playback_.duration_ms =
        playback_.queue[playback_.current_index].duration_ms;
    playback_.playing = true;
    advanced = true;
    ResetProjectionBase();
    UpdatePlaybackUi();
  }
  const std::string requestId =
      RunEngineCommand([this] { return engine_.Next(); }, L"Next");
  if (!requestId.empty() && advanced) {
    playback_overrides_.SetOverride(kOverrideCurrentIndex, requestId);
    playback_overrides_.SetOverride(kOverrideCurrentUri, requestId);
    playback_overrides_.SetOverride(kOverridePosition, requestId);
    playback_overrides_.SetOverride(kOverrideDuration, requestId);
    playback_overrides_.SetOverride(kOverridePlaying, requestId);
  }
}

void Application::OnPrevious() {
  if (!EngineReady()) return;
  bool restarted = false;
  bool switched = false;
  if (playback_.position_ms > 3000) {
    playback_.position_ms = 0;
    restarted = true;
    ResetProjectionBase();
    UpdatePlaybackUi();
  } else if (!playback_.shuffle && playback_.current_index > 0) {
    --playback_.current_index;
    playback_.current_uri = playback_.queue[playback_.current_index].uri;
    playback_.position_ms = 0;
    playback_.duration_ms =
        playback_.queue[playback_.current_index].duration_ms;
    playback_.playing = true;
    switched = true;
    ResetProjectionBase();
    UpdatePlaybackUi();
  }
  const std::string requestId =
      RunEngineCommand([this] { return engine_.Previous(); }, L"Previous");
  if (!requestId.empty()) {
    if (switched) {
      playback_overrides_.SetOverride(kOverrideCurrentIndex, requestId);
      playback_overrides_.SetOverride(kOverrideCurrentUri, requestId);
      playback_overrides_.SetOverride(kOverridePosition, requestId);
      playback_overrides_.SetOverride(kOverrideDuration, requestId);
      playback_overrides_.SetOverride(kOverridePlaying, requestId);
    } else if (restarted) {
      playback_overrides_.SetOverride(kOverridePosition, requestId);
    }
  }
}

void Application::OnSeekTo(int positionMs) {
  if (!EngineReady()) return;
  playback_.position_ms =
      std::clamp<int64_t>(positionMs, 0, playback_.duration_ms);
  ResetProjectionBase();
  UpdatePlaybackUi();
  const std::string requestId =
      RunEngineCommand([this, positionMs] { return engine_.Seek(positionMs); },
                       L"Seek");
  if (!requestId.empty())
    playback_overrides_.SetOverride(kOverridePosition, requestId);
}

void Application::OnSetVolumePercent(int volumePercent) {
  if (!EngineReady()) return;
  playback_.volume_percent = std::clamp(volumePercent, 0, 100);
  UpdatePlaybackUi();
  const std::string requestId = RunEngineCommand(
      [this, volumePercent] { return engine_.SetVolume(volumePercent); },
      L"Volume");
  if (!requestId.empty())
    playback_overrides_.SetOverride(kOverrideVolume, requestId);
}

void Application::OnToggleShuffle() {
  if (!EngineReady()) return;
  playback_.shuffle = !playback_.shuffle;
  const bool enabled = playback_.shuffle;
  UpdatePlaybackUi();
  const std::string requestId =
      RunEngineCommand([this, enabled] { return engine_.SetShuffle(enabled); },
                       L"Shuffle");
  if (!requestId.empty())
    playback_overrides_.SetOverride(kOverrideShuffle, requestId);
}

void Application::OnCycleRepeat() {
  if (!EngineReady()) return;
  playback_.repeat = playback_.repeat == "off"
                         ? "context"
                         : playback_.repeat == "context" ? "track" : "off";
  const std::string mode = playback_.repeat;
  UpdatePlaybackUi();
  const std::string requestId =
      RunEngineCommand([this, mode] { return engine_.SetRepeat(mode); },
                       L"Repeat");
  if (!requestId.empty())
    playback_overrides_.SetOverride(kOverrideRepeat, requestId);
}

void Application::OnEngineState(PlaybackEngineState state) {
  // Fields with unconfirmed optimistic overrides keep their current values so
  // stale pre-command events (heartbeat ticks, player transitions) cannot
  // flicker the UI back; the post-command state after the response applies.
  PlaybackEngineState reconciled =
      ReconcileEngineState(std::move(state), playback_, playback_overrides_);
  const bool queueChanged = !SameQueue(playback_.queue, reconciled.queue);
  const bool statusChanged =
      playback_.ready != reconciled.ready ||
      playback_.auth_state != reconciled.auth_state ||
      playback_.error != reconciled.error;
  const bool becameReady = !playback_.ready && reconciled.ready;
  playback_ = std::move(reconciled);
  ResetProjectionBase();
  UpdatePlaybackUi();
  if (queueChanged) RefreshQueue();
  if (statusChanged) {
    window_.SetEngineStatus(EngineStatusText());
    tray_.SetTooltip(playback_.ready
                         ? L"SpotifyRenderer — standalone 320 kbps engine ready"
                         : L"SpotifyRenderer — engine sign-in required");
    if (!playback_.error.empty()) {
      window_.SetStatus(L"Playback engine: " +
                        Utf8ToWide(playback_.error));
      // The engine marks its own player thread death as a recoverable error;
      // retry through Status (re-authenticates with cached credentials) or
      // respawn the process if the transport is gone.
      if (!playback_.ready &&
          playback_.auth_state == EngineAuthState::Error &&
          playback_.error.find("stopped unexpectedly") != std::string::npos)
        TryRecoverEngine();
    } else if (playback_.ready) {
      window_.SetStatus(L"Standalone playback engine ready at 320 kbps.");
    }
  }
  if (becameReady) {
    engine_restart_attempts_ = 0;
    if (restore_playback_pending_) RestorePlaybackAfterRespawn();
  }
}

void Application::OnEngineCommandError(std::string error) {
  window_.SetStatus(L"Playback command failed: " + Utf8ToWide(error));
  // Reconcile the optimistic UI state with the engine's actual state.
  try {
    engine_.Status();
  } catch (const std::exception&) {
  }
}

void Application::ResetProjectionBase() {
  projector_.Reset(playback_.position_ms, playback_.playing,
                   ::GetTickCount64());
}

void Application::OnEngineError(std::string error) {
  LOG_ERROR("playback engine: " + error);
  playback_.ready = false;
  playback_.auth_state = EngineAuthState::Error;
  playback_.error = std::move(error);
  // The pipe is gone: in-flight commands can never be confirmed, so their
  // optimistic overrides must not stick around.
  playback_overrides_.Reset();
  projector_.Reset(playback_.position_ms, false, ::GetTickCount64());
  UpdatePlaybackUi();
  window_.SetEngineStatus(EngineStatusText());
  window_.SetStatus(
      L"Playback engine stopped; restarting in a moment — playback resumes "
      L"automatically.");
  ScheduleEngineRestart();
}

void Application::TryRecoverEngine() {
  const ULONGLONG now = ::GetTickCount64();
  if (now - last_recovery_attempt_tick_ < 10000) return;
  last_recovery_attempt_tick_ = now;
  if (!engine_.Running()) {
    ScheduleEngineRestart();
    return;
  }
  try {
    engine_.Status();
  } catch (const std::exception&) {
    ScheduleEngineRestart();
  }
}

void Application::ScheduleEngineRestart() {
  if (shutting_down_ || engine_restart_pending_) return;
  // Remember what to restore once the respawned engine authenticates.
  restore_playback_pending_ =
      playback_.current_index >= 0 && !playback_.queue.empty();
  ++engine_restart_attempts_;
  const int64_t delaySeconds = std::min<int64_t>(
      10, 1LL << std::min(engine_restart_attempts_, 4));
  engine_restart_at_ =
      std::chrono::steady_clock::now() + std::chrono::seconds(delaySeconds);
  engine_restart_pending_ = true;
  window_.SetEngineStatus(EngineStatusText());
}

void Application::RestorePlaybackAfterRespawn() {
  restore_playback_pending_ = false;
  const std::string volumeId =
      RunEngineCommand([this] { return engine_.SetVolume(playback_.volume_percent); },
                       L"Restore volume");
  if (!volumeId.empty())
    playback_overrides_.SetOverride(kOverrideVolume, volumeId);
  const std::string shuffleId =
      RunEngineCommand([this] { return engine_.SetShuffle(playback_.shuffle); },
                       L"Restore shuffle");
  if (!shuffleId.empty())
    playback_overrides_.SetOverride(kOverrideShuffle, shuffleId);
  const std::string repeatId =
      RunEngineCommand([this] { return engine_.SetRepeat(playback_.repeat); },
                       L"Restore repeat");
  if (!repeatId.empty())
    playback_overrides_.SetOverride(kOverrideRepeat, repeatId);
  if (playback_.current_index >= 0 &&
      playback_.current_index < static_cast<int>(playback_.queue.size())) {
    const std::string playId = RunEngineCommand(
        [this] {
          return engine_.PlayQueue(playback_.queue, playback_.current_index,
                                   playback_.position_ms);
        },
        L"Resume playback");
    if (!playId.empty()) {
      playback_overrides_.SetOverride(kOverrideQueue, playId);
      playback_overrides_.SetOverride(kOverrideCurrentIndex, playId);
      playback_overrides_.SetOverride(kOverrideCurrentUri, playId);
      playback_overrides_.SetOverride(kOverridePosition, playId);
      playback_overrides_.SetOverride(kOverrideDuration, playId);
      playback_overrides_.SetOverride(kOverridePlaying, playId);
    }
  }
}

void Application::RefreshQueue() {
  if (middle_mode_ != MiddleMode::Queue) return;
  window_.SetMiddleLabel(L"Queue");
  window_.SetQueueTracks(playback_.queue);
}

void Application::RefreshPlaylists(bool force) {
  if (!IsAuthed()) return;
  const auto now = std::chrono::steady_clock::now();
  if (!force && playlists_fetched_at_.has_value() && !playlists_.empty() &&
      now - *playlists_fetched_at_ < kPlaylistListTtl) {
    window_.SetPlaylists(playlists_);
    return;
  }
  PostTask<std::vector<PlaylistRef>>(
      [this] {
        if (me_id_.empty()) {
          try {
            me_id_ = api_->GetMeId();
          } catch (const ApiError& error) {
            LOG_WARN(std::string("could not load account id: ") + error.what());
          }
        }
        return api_->GetMyPlaylists();
      },
      [this](std::vector<PlaylistRef> playlists) {
        playlists_ = std::move(playlists);
        playlists_fetched_at_ = std::chrono::steady_clock::now();
        window_.SetPlaylists(playlists_);
      },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Playlists");
      });
}

void Application::RequestPlaylistTracks(const std::string& id) {
  CachedTrackList cached;
  if (track_cache_.Get("p:" + id, &cached)) {
    ShowPlaylistTracks(id, cached);
    return;
  }
  PostTask<std::pair<std::vector<TrackRef>, std::string>>(
      [this, id] {
        std::string snapshot;
        auto tracks = api_->GetPlaylistTracks(id, &snapshot);
        return std::make_pair(std::move(tracks), std::move(snapshot));
      },
      [this, id](std::pair<std::vector<TrackRef>, std::string> result) {
        CachedTrackList cached{std::move(result.first),
                               std::move(result.second)};
        ShowPlaylistTracks(id, cached);
        track_cache_.Put("p:" + id, std::move(cached));
      },
      [this, id](std::string message, int status, int retry) {
        const PlaylistRef* playlist = nullptr;
        for (const auto& candidate : playlists_)
          if (candidate.id == id) playlist = &candidate;
        if (IsDevModePlaylistRestriction(
                status, playlist ? playlist->owner_id : std::string(),
                me_id_)) {
          LOG_WARN(std::string("development-mode 403 for playlist ") + id +
                   " (owner " + (playlist ? playlist->owner_id : "unknown") +
                   "): " + message);
          window_.SetStatus(
              L"Playlist tracks: Spotify development-mode restriction (HTTP "
              L"403). This playlist belongs to another account; "
              L"development-mode apps can only read playlists owned by "
              L"accounts on the app's allowlist. Add the owner in your "
              L"Spotify developer dashboard (Settings > Users and Access), or "
              L"request extended quota mode for the app.");
          return;
        }
        HandleApiError(message, status, retry, L"Playlist tracks");
      });
}

void Application::ShowPlaylistTracks(const std::string& id,
                                     const CachedTrackList& cached) {
  current_playlist_id_ = id;
  current_playlist_snapshot_ = cached.snapshot_id;
  window_.SetMiddleTracks(cached.tracks);
  const auto found = std::find_if(
      playlists_.begin(), playlists_.end(),
      [&id](const PlaylistRef& playlist) { return playlist.id == id; });
  window_.SetMiddleLabel(found == playlists_.end()
                             ? L"Playlist"
                             : Utf8ToWide(found->name));
}

void Application::OpenAlbumTracks(const AlbumRef& album) {
  CachedTrackList cached;
  if (track_cache_.Get("a:" + album.id, &cached)) {
    current_album_ = album;
    middle_mode_ = MiddleMode::AlbumTracks;
    window_.SetMiddleLabel(Utf8ToWide(album.name));
    window_.SetMiddleTracks(cached.tracks);
    return;
  }
  PostTask<std::vector<TrackRef>>(
      [this, id = album.id] { return api_->GetAlbumTracks(id); },
      [this, album](std::vector<TrackRef> tracks) {
        ApplyAlbumMetadata(tracks, album);
        CachedTrackList cached{std::move(tracks), {}};
        current_album_ = album;
        middle_mode_ = MiddleMode::AlbumTracks;
        window_.SetMiddleLabel(Utf8ToWide(album.name));
        window_.SetMiddleTracks(cached.tracks);
        track_cache_.Put("a:" + album.id, std::move(cached));
      },
      [this](std::string message, int status, int retry) {
        HandleApiError(message, status, retry, L"Album tracks");
      });
}

void Application::EnsureCover(const std::string& url) {
  if (url.empty() || url == last_cover_url_) return;
  last_cover_url_ = url;
  RequestCoverFile(url, true);
}

void Application::OnTrackArtworkNeeded(const std::string& url) {
  RequestCoverFile(url, false);
}

void Application::RequestCoverFile(const std::string& url, bool nowPlaying) {
  if (options_.smoke || options_.demo || url.empty()) return;
  auto [origin, path] = SplitOrigin(url);
  if (!StartsWith(origin, "https://") || path.empty()) return;
  const std::wstring file = paths::CoverFile(Sha1Hex(url) + ".img");
  if (file.empty()) return;
  auto publish = [this, url, file, nowPlaying] {
    window_.SetTrackArtwork(url, file);
    if (nowPlaying) window_.SetCoverFile(file);
  };
  if (::GetFileAttributesW(file.c_str()) != INVALID_FILE_ATTRIBUTES &&
      paths::IsSafeOwnedPath(file)) {
    publish();
    return;
  }
  TaskQueue* target = nowPlaying ? &api_tasks_ : &artwork_tasks_;
  target->Post([this, url, origin, path, file, nowPlaying] {
    HttpClient image(origin);
    HttpResponse response =
        image.Send("GET", path, {}, {{"Accept", "image/*"}}, 15000);
    if (!response.succeeded || response.status != 200 ||
        response.body.empty() || response.body.size() > 10 * 1024 * 1024)
      return;
    if (paths::AtomicWriteOwnedFile(file, response.body)) {
      PostUi([this, url, file, nowPlaying] {
        window_.SetTrackArtwork(url, file);
        if (nowPlaying) window_.SetCoverFile(file);
      });
    }
  });
}

void Application::OnRefreshAll() {
  // The playlist list is served from cache while fresh (< 10 min); explicit
  // refresh refetches it only once stale. Track lists always refetch.
  RefreshPlaylists();
  track_cache_.Clear();
  if (middle_mode_ == MiddleMode::Playlist && !current_playlist_id_.empty())
    RequestPlaylistTracks(current_playlist_id_);
  else if (middle_mode_ == MiddleMode::AlbumTracks && !current_album_.id.empty())
    OpenAlbumTracks(current_album_);
  RefreshQueue();
  if (engine_.Running())
    RunEngineCommand([this] { return engine_.Status(); },
                     L"Refresh engine status");
}

void Application::OnSettingsShown() {
  if (options_.smoke || options_.demo) return;
  const std::wstring dir = paths::EngineAudioCacheDir();
  window_.SetCacheUsage(L"Audio: measuring cache usage...");
  api_tasks_.Post([this, dir] {
    const uint64_t bytes = paths::SumFileBytesUnderDir(dir);
    PostUi([this, bytes] { window_.SetCacheUsage(CacheUsageText(bytes)); });
  });
}

void Application::UpdatePlaybackUi() {
  window_.SetPlayback(playback_);
  if (playback_.current_index >= 0 &&
      playback_.current_index < static_cast<int>(playback_.queue.size()))
    EnsureCover(playback_.queue[playback_.current_index].cover_url);
}

std::wstring Application::EngineStatusText() const {
  std::wstring auth = L"signing in";
  if (playback_.auth_state == EngineAuthState::Ready) auth = L"authenticated";
  if (playback_.auth_state == EngineAuthState::Error) auth = L"error";
  std::wstring result = L"Standalone engine: " + auth +
                        L" · Ogg Vorbis 320 kbps · cache limit 1 GiB";
  if (!playback_.error.empty()) result += L" · " + Utf8ToWide(playback_.error);
  return result;
}


std::string Application::IsolationStatus() const {
  return "engine=unused, api=unused, production_state=unused";
}

void Application::OnTrayShow() {
  window_.Show(true);
  ::ShowWindow(hwnd_, SW_RESTORE);
  ::SetForegroundWindow(hwnd_);
}

void Application::OnTrayCommand(UINT id) {
  if (id != 0) {
    ::SendMessageW(hwnd_, WM_COMMAND, id, 0);
    return;
  }
  tray_.ShowMenu([](HMENU menu) {
    ::AppendMenuW(menu, MF_STRING, IDM_TRAY_SHOW, L"Show SpotifyRenderer");
    ::AppendMenuW(menu, MF_STRING, IDM_TRAY_SETTINGS, L"Settings");
    ::AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
    ::AppendMenuW(menu, MF_STRING, IDM_TRAY_EXIT, L"Exit");
  });
}

void Application::OnTimer(UINT id) {
  if (id == kSmokeTimer) {
    ::DestroyWindow(hwnd_);
    return;
  }
  if (id != kPositionTimer) return;
  if (engine_restart_pending_ &&
      std::chrono::steady_clock::now() >= engine_restart_at_) {
    engine_restart_pending_ = false;
    StartEngine();
  }
  const int64_t projected =
      projector_.Current(::GetTickCount64(), playback_.duration_ms);
  if (projected != playback_.position_ms) {
    playback_.position_ms = projected;
    UpdatePlaybackUi();
  }
}

void Application::OnExit() {
  if (!shutting_down_) ::DestroyWindow(hwnd_);
}

}  // namespace sr
