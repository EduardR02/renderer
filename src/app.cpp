#include "app.h"

#include <commctrl.h>
#include <objidl.h>
#include <gdiplus.h>
#include <shellapi.h>

#include <algorithm>
#include <fstream>
#include <iterator>
#include <limits>
#include <stdexcept>
#include <utility>

#include <nlohmann/json.hpp>

#include "app_paths.h"
#include "log.h"
#include "util.h"

namespace sr {
using nlohmann::json;

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

// Capped exponential backoff (seconds) for retry `attempt` (0-based) of the
// playlist-library refetch: 5, 10, 20, 40, 60, 60, ... Pure so the schedule
// is unit-testable.
int PlaylistRetryDelaySeconds(int attempt) {
  if (attempt < 0) attempt = 0;
  const int64_t delay =
      static_cast<int64_t>(kPlaylistRetryBaseSeconds)
      << std::min<int64_t>(attempt, 32);
  return static_cast<int>(
      std::min<int64_t>(delay, kPlaylistRetryMaxSeconds));
}

// Stale on-disk copies serve the library only after the fresh fetch already
// failed; otherwise they are skipped so the refetch actually runs.
PlaylistCacheUse ClassifyPlaylistCache(int64_t fetchedAtUnixSeconds,
                                       int64_t nowUnixSeconds,
                                       int64_t ttlMinutes,
                                       bool fetchFailed) {
  const bool fresh =
      nowUnixSeconds - fetchedAtUnixSeconds <
      static_cast<int64_t>(ttlMinutes) * 60;
  if (fresh) return PlaylistCacheUse::Fresh;
  return fetchFailed ? PlaylistCacheUse::StaleFallback : PlaylistCacheUse::None;
}

void TrimPlaylistTracksCache(std::vector<CachedPlaylistTracks>* entries) {
  if (!entries) return;
  std::stable_sort(entries->begin(), entries->end(),
                   [](const CachedPlaylistTracks& left,
                      const CachedPlaylistTracks& right) {
                     return left.fetched_at > right.fetched_at;
                   });
  if (entries->size() > static_cast<size_t>(kPlaylistTracksCacheCapacity))
    entries->resize(kPlaylistTracksCacheCapacity);
}

json BuildPlaylistTracksCacheDoc(
    const std::vector<CachedPlaylistTracks>& entries, int64_t nowUnixSeconds) {
  json playlistEntries = json::array();
  for (const CachedPlaylistTracks& entry : entries) {
    json tracks = json::array();
    for (const TrackRef& track : entry.value.tracks)
      tracks.push_back(TrackRefToEngineJson(track));
    playlistEntries.push_back(
        {{"id", entry.id},
         {"fetched_at", entry.fetched_at},
         {"revision", entry.value.snapshot_id},
         {"tracks", std::move(tracks)}});
  }
  return {{"version", 1},
          {"saved_at", nowUnixSeconds},
          {"playlists", std::move(playlistEntries)}};
}

bool ParsePlaylistTracksCacheDoc(const json& doc,
                                 std::vector<CachedPlaylistTracks>* out) {
  if (!out || !doc.is_object()) return false;
  const auto version = doc.find("version");
  if (version == doc.end() || *version != 1) return false;
  const auto list = doc.find("playlists");
  if (list == doc.end() || !list->is_array()) return false;
  std::vector<CachedPlaylistTracks> parsed;
  for (const auto& entry : *list) {
    if (!entry.is_object()) continue;
    const auto id = entry.find("id");
    const auto fetched = entry.find("fetched_at");
    if (id == entry.end() || !id->is_string() || id->get<std::string>().empty() ||
        fetched == entry.end() || !fetched->is_number_integer())
      continue;
    const auto tracks = entry.find("tracks");
    if (tracks == entry.end() || !tracks->is_array()) continue;
    const auto revision = entry.find("revision");
    CachedPlaylistTracks cached;
    cached.id = id->get<std::string>();
    cached.fetched_at = fetched->get<int64_t>();
    if (revision != entry.end() && revision->is_string())
      cached.value.snapshot_id = revision->get<std::string>();
    cached.value.tracks.reserve(tracks->size());
    for (const auto& track : *tracks) {
      if (!track.is_object()) continue;
      try {
        cached.value.tracks.push_back(TrackRefFromEngineJson(track));
      } catch (const std::exception&) {
        // A malformed row must never fail the whole cache.
      }
    }
    parsed.push_back(std::move(cached));
  }
  TrimPlaylistTracksCache(&parsed);
  *out = std::move(parsed);
  return true;
}

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


DelayedTaskQueue::DelayedTaskQueue(NowFn now) : now_(std::move(now)) {}

void DelayedTaskQueue::Schedule(int delaySeconds, std::function<void()> task) {
  if (!task) return;
  const TimePoint due =
      now_() + std::chrono::seconds(std::max(0, delaySeconds));
  pending_.push_back({due, std::move(task)});
  // Keep deadlines ordered (stable, so same-deadline tasks stay FIFO).
  std::stable_sort(pending_.begin(), pending_.end(),
                   [](const auto& left, const auto& right) {
                     return left.first < right.first;
                   });
}

int DelayedTaskQueue::RunDue(
    TimePoint now, const std::function<void(std::function<void()>)>& dispatch) {
  int dispatched = 0;
  while (!pending_.empty() && pending_.front().first <= now) {
    std::function<void()> task = std::move(pending_.front().second);
    pending_.pop_front();
    if (dispatch) dispatch(std::move(task));
    ++dispatched;
  }
  return dispatched;
}

void DelayedTaskQueue::Clear() { pending_.clear(); }

bool DelayedTaskQueue::Empty() const { return pending_.empty(); }

size_t DelayedTaskQueue::Size() const { return pending_.size(); }


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
  // The handlers (IDC_ACC_*) already exist in WM_COMMAND; without the table
  // the accelerators were dead. Ctrl+F focuses search, Ctrl+N creates a
  // playlist, F5 refreshes, and the media key toggles play/pause (no
  // modifier, so nothing fires while typing in the edits).
  ACCEL accelerators[] = {
      {FVIRTKEY | FCONTROL, 'F', IDC_ACC_SEARCH},
      {FVIRTKEY | FCONTROL, 'N', IDC_ACC_NEW_PLAYLIST},
      {FVIRTKEY, VK_F5, IDC_ACC_REFRESH},
      {FVIRTKEY, VK_MEDIA_PLAY_PAUSE, IDC_ACC_PLAY_PAUSE},
  };
  accelerators_ = ::CreateAcceleratorTableW(
      accelerators, static_cast<int>(_countof(accelerators)));
  tray_.Create(hwnd_, L"SpotifyRenderer — local engine starting");
  if (options_.demo) {
    window_.SetDemo();
  } else {
    window_.ShowWorkspace(MainWindow::WorkspaceKind::Collection);
    if (!options_.smoke) StartEngine();
  }

  window_.Show(true);
  ::ShowWindow(hwnd_, options_.smoke ? SW_SHOWNOACTIVATE : show);
  ::UpdateWindow(hwnd_);
  if (!options_.smoke) window_.FocusSearch();
  StartTimers();
  if (options_.smoke || options_.demo)
    LOG_INFO(std::string("isolated ") +
             (options_.smoke ? "smoke" : "demo") + " launch: " +
             IsolationStatus());

  MSG message{};
  while (::GetMessageW(&message, nullptr, 0, 0) > 0) {
    // IsDialogMessage consumes VK_RETURN even though the main window has no
    // default pushbutton, which would swallow Enter typed in the search box
    // before the edit's subclass can submit it (the same path as clicking
    // Search). Enter on the search edit bypasses accelerator/dialog
    // navigation entirely and is dispatched straight to the edit; all other
    // keys keep the normal routing.
    const bool bypassEnter = SearchEnterBypassesDialogNavigation(message);
    if (!bypassEnter && accelerators_ &&
        ::TranslateAcceleratorW(hwnd_, accelerators_, &message))
      continue;
    if (!bypassEnter && ::IsDialogMessageW(hwnd_, &message)) continue;
    ::TranslateMessage(&message);
    ::DispatchMessageW(&message);
  }
  Shutdown();
  return static_cast<int>(message.wParam);
}

void Application::InitCore() {
  if (!paths::EnsureDirs()) return;
  log::Init(paths::LogFile());
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
        L"Local playback engine is starting; open Settings to sign in with "
        L"Spotify if needed.");
    window_.SetEngineStatus(EngineStatusText());
  }
}

void Application::Shutdown() {
  if (shutting_down_) return;
  shutting_down_ = true;
  engine_restart_pending_ = false;
  StopTimers();
  if (accelerators_) {
    ::DestroyAcceleratorTable(accelerators_);
    accelerators_ = nullptr;
  }
  delayed_api_tasks_.Clear();
  artwork_tasks_.DiscardPending();
  api_tasks_.Stop();
  artwork_tasks_.Stop();
  engine_.Shutdown();
  tray_.Destroy();
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
  // Browsing and playlist edits both ride on the playback engine's Spotify
  // session (spclient browse + login5-minted Web API tokens), so they are
  // available exactly when the engine reports ready.
  return playback_.ready;
}

void Application::PostUi(std::function<void()> function) {
  HWND target = hwnd_;
  if (!target || shutting_down_) return;
  auto* heap = new std::function<void()>(std::move(function));
  if (!::PostMessageW(target, WM_SR_RUN, 0, reinterpret_cast<LPARAM>(heap)))
    delete heap;
}

void Application::HandleApiError(const std::string& message,
                                 const std::wstring& context) {
  LOG_ERROR(WideToUtf8(context) + ": " + message);
  window_.SetStatus(context + L": " + Utf8ToWide(message));
}


void Application::OnSearch(const std::string& query) {
  if (!IsAuthed()) {
    window_.SetStatus(
        L"Search needs the local playback engine to finish signing in.");
    return;
  }
  if (Trim(query).empty()) return;
  window_.SetStatus(L"Searching...");
  PostTask<SearchResult>(
      [this, query] {
        SearchResult result;
        std::string error;
        if (!engine_.BrowseSearch(query, 10, &result, &error))
          throw std::runtime_error(error.empty() ? "search failed" : error);
        return result;
      },
      [this](SearchResult result) {
        window_.SetSearchResults(result);
        window_.SetStatus(L"Search complete");
      },
      [this](std::string message) {
        HandleApiError(message, L"Search");
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
        [this, id = artist.id] {
          std::vector<TrackRef> topTracks;
          std::vector<AlbumRef> albums;
          std::string error;
          if (!engine_.BrowseArtist(id, &topTracks, &albums, &error))
            throw std::runtime_error(error.empty() ? "artist browse failed"
                                                   : error);
          return topTracks;
        },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
      [this](std::string message) {
        HandleApiError(message, L"Artist top tracks");
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
        [this, id = artist.id] {
          std::vector<TrackRef> topTracks;
          std::vector<AlbumRef> albums;
          std::string error;
          if (!engine_.BrowseArtist(id, &topTracks, &albums, &error))
            throw std::runtime_error(error.empty() ? "artist browse failed"
                                                   : error);
          return topTracks;
        },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
      [this](std::string message) {
        HandleApiError(message, L"Artist top tracks");
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
        std::string error;
        if (!engine_.EditAddPlaylistTracks(playlist, {uri}, &error))
          throw std::runtime_error(error.empty() ? "add to playlist failed"
                                                 : error);
        return true;
      },
      [this, playlist](bool) {
        track_cache_.Invalidate("p:" + playlist);
        InvalidatePlaylistTracksCache(playlist);
        window_.SetStatus(L"Added to playlist");
      },
      [this](std::string message) {
        HandleApiError(message, L"Add to playlist");
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
        [this, id = artist.id] {
          std::vector<TrackRef> topTracks;
          std::vector<AlbumRef> albums;
          std::string error;
          if (!engine_.BrowseArtist(id, &topTracks, &albums, &error))
            throw std::runtime_error(error.empty() ? "artist browse failed"
                                                   : error);
          return topTracks;
        },
        [this, artist](std::vector<TrackRef> tracks) {
          middle_mode_ = MiddleMode::ArtistTracks;
          window_.SetArtistPage(artist, tracks);
        },
      [this](std::string message) {
        HandleApiError(message, L"Artist top tracks");
        });
  } else if (middle_mode_ == MiddleMode::Playlist &&
             command == IDM_CTX_MIDDLE_REMOVE) {
    PostTask<bool>(
        [this, uri = track.uri] {
          std::string error;
          if (!engine_.EditRemovePlaylistTracks(current_playlist_id_, {uri},
                                                &error))
            throw std::runtime_error(error.empty() ? "remove track failed"
                                                    : error);
          return true;
        },
        [this](bool) {
          track_cache_.Invalidate("p:" + current_playlist_id_);
          InvalidatePlaylistTracksCache(current_playlist_id_);
          RequestPlaylistTracks(current_playlist_id_);
        },
      [this](std::string message) {
        HandleApiError(message, L"Remove track");
        });
  } else if (middle_mode_ == MiddleMode::Playlist &&
             (command == IDM_CTX_MIDDLE_UP ||
              command == IDM_CTX_MIDDLE_DOWN)) {
    const int destination = command == IDM_CTX_MIDDLE_UP ? index - 1 : index + 2;
    if (destination < 0 || destination > static_cast<int>(tracks.size())) return;
    PostTask<bool>(
        [this, index, destination] {
          std::string error;
          if (!engine_.EditReorderPlaylistTracks(current_playlist_id_, index,
                                                 destination, &error))
            throw std::runtime_error(error.empty() ? "reorder playlist failed"
                                                   : error);
          return true;
        },
        [this](bool) {
          track_cache_.Invalidate("p:" + current_playlist_id_);
          InvalidatePlaylistTracksCache(current_playlist_id_);
          RequestPlaylistTracks(current_playlist_id_);
        },
      [this](std::string message) {
        HandleApiError(message, L"Reorder playlist");
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
        PlaylistRef playlist;
        std::string error;
        if (!engine_.EditCreatePlaylist(value, &playlist, &error))
          throw std::runtime_error(error.empty() ? "create playlist failed"
                                                 : error);
        return playlist;
      },
      [this](PlaylistRef) { RefreshPlaylists(true); },
      [this](std::string message) {
        HandleApiError(message, L"Create playlist");
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
        std::string error;
        if (!engine_.EditRenamePlaylist(id, value, &error))
          throw std::runtime_error(error.empty() ? "rename playlist failed"
                                                 : error);
        return true;
      },
      [this](bool) { RefreshPlaylists(true); },
      [this](std::string message) {
        HandleApiError(message, L"Rename playlist");
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
        std::string error;
        if (!engine_.EditDeletePlaylist(id, &error))
          throw std::runtime_error(error.empty() ? "delete playlist failed"
                                                 : error);
        return true;
      },
      [this, id = playlists_[index].id](bool) {
        track_cache_.Invalidate("p:" + id);
        InvalidatePlaylistTracksCache(id);
        middle_mode_ = MiddleMode::Queue;
        RefreshPlaylists(true);
        RefreshQueue();
      },
      [this](std::string message) {
        HandleApiError(message, L"Delete playlist");
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

void Application::OnLogin() {
  if (options_.smoke || options_.demo) return;
  // The Log in button is enabled exactly while the engine holds a fresh
  // authorize URL; anything else is a double-submit or a stale click.
  if (playback_.auth_state != EngineAuthState::NeedsLogin) return;
  if (playback_.auth_url.empty()) {
    window_.SetStatus(
        L"Spotify login is not ready yet; try again in a moment.");
    return;
  }
  // Open the authorize URL first (the user needs seconds to complete the
  // sign-in; the engine binds its loopback callback right after), then start
  // the flow. The engine regenerates the URL per attempt, so the URL shown
  // here is exactly the one its listener accepts.
  const HINSTANCE opened = ::ShellExecuteW(
      nullptr, L"open", Utf8ToWide(playback_.auth_url).c_str(), nullptr,
      nullptr, SW_SHOWNORMAL);
  if (reinterpret_cast<INT_PTR>(opened) <= 32) {
    window_.SetStatus(L"Could not open the Spotify sign-in page in your "
                      L"browser (Windows error " +
                      std::to_wstring(::GetLastError()) + L").");
    return;
  }
  window_.SetStatus(L"Waiting for Spotify login... complete the sign-in in "
                    L"the browser.");
  RunEngineCommand([this] { return engine_.TriggerLogin(); },
                   L"Spotify login");
}

void Application::OnLogout() {
  if (options_.smoke || options_.demo) return;
  if (!LogoutButtonEnabled(playback_)) return;
  // Immediate action, no confirmation dialog: the engine clears the cached
  // credentials and tears the session down; the needs_login state event it
  // emits right after flips the Settings page and re-enables Log in.
  // A stale respawn must never resurrect the session or its playback.
  restore_playback_pending_ = false;
  playback_overrides_.Reset();
  window_.SetStatus(L"Signed out; Spotify login required.");
  RunEngineCommand([this] { return engine_.Logout(); }, L"Spotify logout");
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
  // The engine's session account doubles as the Web API user id for playlist
  // edits; /v1/me is no longer consulted.
  if (playback_.auth_state == EngineAuthState::Ready &&
      !playback_.username.empty())
    me_id_ = playback_.username;
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
    } else if (playback_.auth_state == EngineAuthState::NeedsLogin) {
      window_.SetStatus(
          L"Spotify login required. Use Settings and Log in.");
    } else if (playback_.auth_state == EngineAuthState::Authenticating) {
      // A published authorize URL means the browser flow is running; the
      // cached-credentials path needs no user action.
      window_.SetStatus(
          playback_.auth_url.empty()
              ? L"Local playback engine is signing in with saved credentials."
              : L"Waiting for Spotify login... complete the sign-in in the "
                L"browser.");
    }
  }
  if (becameReady) {
    engine_restart_attempts_ = 0;
    if (restore_playback_pending_) RestorePlaybackAfterRespawn();
    // The engine session now mints browsing tokens: load the library once per
    // successful engine authentication.
    OnRefreshAll();
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
  // Fresh in-memory copy (set by this run or loaded from disk below).
  if (!force && playlists_fetched_at_.has_value() && !playlists_.empty() &&
      now - *playlists_fetched_at_ < kPlaylistListTtl) {
    window_.SetPlaylists(playlists_);
    return;
  }
  // Relaunch fast path: a fresh on-disk copy of the library skips the
  // startup engine round-trip entirely.
  if (!force && LoadPlaylistCache()) return;
  FetchPlaylists();
}

void Application::FetchPlaylists() {
  PostTask<std::vector<PlaylistRef>>(
      [this] {
        std::vector<PlaylistRef> list;
        std::string error;
        if (!engine_.BrowsePlaylists(kPlaylistFetchLength, &list, &error))
          throw std::runtime_error(error.empty() ? "playlists unavailable"
                                                 : error);
        return list;
      },
      [this](std::vector<PlaylistRef> list) {
        playlist_retry_attempts_ = 0;
        playlists_ = std::move(list);
        ApplyPlaylistCoverFallbacks();
        if (!playlists_fetched_at_.has_value())
          playlists_fetched_at_ = std::chrono::steady_clock::now();
        window_.SetPlaylists(playlists_);
        // Coverless playlists that have no cached tracks get their tracks
        // fetched in the background so the rail mosaic tiles appear without
        // a click. Runs after SetPlaylists so the rail's filtered row order
        // (visible rows first) is current; the batch is capped.
        EagerFetchCoverlessPlaylists();
        SavePlaylistCache();
      },
      [this](std::string message) {
        // A failed fresh fetch must not leave the library empty on startup:
        // serve the on-disk copy even when stale, then retry in the
        // background with capped exponential backoff so a transient
        // engine-side 502/503 heals without user action. The in-memory
        // library is never downgraded by an older disk copy.
        const bool fell_back = playlists_.empty() && LoadPlaylistCache(true);
        if (fell_back)
          LOG_INFO("playlist library loaded from cache (stale fallback, " +
                   std::to_string(playlists_.size()) + " playlists)");
        if (playlist_retry_attempts_ >= kPlaylistRetryMaxAttempts) {
          // Give up: log and surface the failure (the cached library, when
          // present, stays visible).
          HandleApiError(message, L"Playlists");
          return;
        }
        LOG_ERROR("Playlists: " + message);
        const int delay = PlaylistRetryDelaySeconds(playlist_retry_attempts_);
        ++playlist_retry_attempts_;
        window_.SetStatus(
            fell_back ? L"Playlists unavailable - showing cached copy, retrying"
                      : L"Playlists unavailable - retrying");
        ScheduleDelayedApiTask(delay, [this] { FetchPlaylists(); });
      });
}

void Application::ScheduleDelayedApiTask(int delaySeconds,
                                         std::function<void()> task) {
  delayed_api_tasks_.Schedule(std::max(1, delaySeconds), std::move(task));
}

void Application::SavePlaylistCache() {
  if (options_.smoke || options_.demo) return;
  const std::wstring file = paths::PlaylistListCacheFile();
  if (file.empty()) return;
  json entries = json::array();
  for (const PlaylistRef& playlist : playlists_) {
    entries.push_back({{"id", playlist.id},
                       {"uri", playlist.uri},
                       {"name", playlist.name},
                       {"owner", playlist.owner},
                       {"owner_id", playlist.owner_id},
                       {"cover_url", playlist.cover_url},
                       {"collaborative", playlist.collaborative},
                       {"tracks_total", playlist.tracks_total},
                       {"snapshot_id", playlist.snapshot_id}});
  }
  const json doc = {{"version", 1},
                    {"fetched_at", NowUnixSeconds()},
                    {"me_id", me_id_},
                    {"playlists", std::move(entries)}};
  paths::AtomicWriteOwnedFile(file, doc.dump());
}

bool Application::LoadPlaylistCache(bool allowStale) {
  if (options_.smoke || options_.demo) return false;
  const std::wstring file = paths::PlaylistListCacheFile();
  if (file.empty() ||
      ::GetFileAttributesW(file.c_str()) == INVALID_FILE_ATTRIBUTES ||
      !paths::IsSafeOwnedPath(file))
    return false;
  try {
    std::ifstream stream(file, std::ios::binary);
    if (!stream) return false;
    std::string text((std::istreambuf_iterator<char>(stream)),
                     std::istreambuf_iterator<char>());
    const json doc = json::parse(text);
    if (!doc.is_object()) return false;
    const auto version = doc.find("version");
    if (version == doc.end() || *version != 1) return false;
    const auto fetched = doc.find("fetched_at");
    if (fetched == doc.end() || !fetched->is_number_integer()) return false;
    // A stale cache serves the library only as a fallback once the fresh
    // fetch has failed (the caller retries in the background); otherwise it
    // is skipped so the refetch actually runs.
    if (ClassifyPlaylistCache(fetched->get<int64_t>(), NowUnixSeconds(),
                              kPlaylistListTtl.count(), allowStale) ==
        PlaylistCacheUse::None)
      return false;
    const auto me = doc.find("me_id");
    if (me != doc.end() && me->is_string())
      me_id_ = me->get<std::string>();
    const auto list = doc.find("playlists");
    if (list == doc.end() || !list->is_array()) return false;
    std::vector<PlaylistRef> loaded;
    for (const auto& entry : *list) {
      if (!entry.is_object()) continue;
      PlaylistRef playlist;
      const auto str = [&entry](const char* key) {
        const auto it = entry.find(key);
        return it != entry.end() && it->is_string() ? it->get<std::string>()
                                                    : std::string();
      };
      const auto integer = [&entry](const char* key) {
        const auto it = entry.find(key);
        return it != entry.end() && it->is_number_integer() ? it->get<int>()
                                                            : 0;
      };
      const auto boolean = [&entry](const char* key) {
        const auto it = entry.find(key);
        return it != entry.end() && it->is_boolean() ? it->get<bool>() : false;
      };
      playlist.id = str("id");
      playlist.uri = str("uri");
      playlist.name = str("name");
      playlist.owner = str("owner");
      playlist.owner_id = str("owner_id");
      playlist.cover_url = str("cover_url");
      playlist.collaborative = boolean("collaborative");
      playlist.tracks_total = integer("tracks_total");
      playlist.snapshot_id = str("snapshot_id");
      loaded.push_back(std::move(playlist));
    }
    playlists_ = std::move(loaded);
    ApplyPlaylistCoverFallbacks();
    playlists_fetched_at_ = std::chrono::steady_clock::now();
    window_.SetPlaylists(playlists_);
    // Same eager backfill as a fresh fetch: coverless playlists without
    // cached tracks fetch in the background so rail art appears on startup.
    EagerFetchCoverlessPlaylists();
    LOG_INFO("playlist library loaded from cache (" +
             std::to_string(playlists_.size()) + " playlists)");
    return true;
  } catch (const std::exception&) {
    return false;
  }
}

void Application::RequestPlaylistTracks(const std::string& id) {
  CachedTrackList cached;
  if (track_cache_.Get("p:" + id, &cached)) {
    ShowPlaylistTracks(id, cached);
    return;
  }
  if (!options_.smoke && !options_.demo) {
    EnsurePlaylistTracksCacheLoaded();
    const auto found = std::find_if(
        playlist_tracks_cache_.begin(), playlist_tracks_cache_.end(),
        [&id](const CachedPlaylistTracks& entry) { return entry.id == id; });
    if (found != playlist_tracks_cache_.end() &&
        !found->value.tracks.empty()) {
      // Within TTL: the click resolves instantly from disk with no engine
      // round-trip. Stale: still shown instantly, refreshed in the
      // background; the refresh failure keeps the cached copy on screen.
      // An empty cached list is treated as a miss: it can only be a
      // poisoned entry from a failed fetch, so it is refetched.
      const bool fresh =
          ClassifyPlaylistCache(found->fetched_at, NowUnixSeconds(),
                                kPlaylistTracksTtlMinutes, false) ==
          PlaylistCacheUse::Fresh;
      ShowPlaylistTracks(id, found->value);
      track_cache_.Put("p:" + id, found->value);
      if (!fresh) FetchPlaylistTracksBackground(id, /*refreshOnly=*/true);
      return;
    }
  }
  FetchPlaylistTracksBackground(id, /*refreshOnly=*/false);
}

void Application::FetchPlaylistTracksBackground(const std::string& id,
                                                bool refreshOnly) {
  PostTask<std::pair<std::vector<TrackRef>, std::string>>(
      [this, id] {
        std::vector<TrackRef> tracks;
        std::string revision;
        std::string error;
        if (!engine_.BrowsePlaylist(id, &tracks, &revision, &error))
          throw std::runtime_error(error.empty() ? "playlist unavailable"
                                                 : error);
        return std::make_pair(std::move(tracks), std::move(revision));
      },
      [this, id](std::pair<std::vector<TrackRef>, std::string> result) {
        CachedTrackList cached{std::move(result.first),
                               std::move(result.second)};
        ShowPlaylistTracks(id, cached);
        // An empty result is shown but never cached: a transient engine or
        // metadata failure must not persist as "empty playlist" (the cache
        // would serve it until the TTL expires).
        if (cached.tracks.empty()) return;
        SavePlaylistTracksCache(id, cached);
        // A coverless playlist's tracks now provide the rail tile: a 2x2
        // mosaic of the first up-to-four track covers. Publish the cell set
        // before the single-cover fallback below persists the first cover
        // (the mosaic is the rail's richer art; the header keeps the
        // single-cover path).
        std::vector<std::string> mosaicCovers;
        mosaicCovers.reserve(4);
        for (const TrackRef& track : cached.tracks) {
          if (track.cover_url.empty()) continue;
          mosaicCovers.push_back(track.cover_url);
          if (mosaicCovers.size() == 4) break;
        }
        if (!mosaicCovers.empty())
          window_.SetPlaylistMosaicCovers(id, std::move(mosaicCovers));
        // A coverless playlist's tracks now provide the workspace header
        // art (the first track's cover — the same art the view shows);
        // persist it with the library cache and refresh the sidebar.
        ApplyPlaylistCoverFallbacks();
        SavePlaylistCache();
        const auto playlist = std::find_if(
            playlists_.begin(), playlists_.end(),
            [&id](const PlaylistRef& entry) { return entry.id == id; });
        if (playlist != playlists_.end() && !playlist->cover_url.empty())
          window_.SetPlaylistCoverFallback(id, playlist->cover_url);
        // Last: Put takes the list by value and moves it away, so the disk
        // cache write and the fallback above must run while it is intact.
        track_cache_.Put("p:" + id, std::move(cached));
      },
      [this, refreshOnly](std::string message) {
        if (refreshOnly) {
          // A stale copy is already on screen: never replace it with an
          // error dialog, just log and keep showing the cached tracks.
          LOG_ERROR("Playlist tracks refresh: " + message);
          window_.SetStatus(
              L"Playlist refresh failed; showing the cached copy.");
          return;
        }
        HandleApiError(message, L"Playlist tracks");
      });
}

void Application::EnsurePlaylistTracksCacheLoaded() {
  if (playlist_tracks_cache_loaded_) return;
  playlist_tracks_cache_loaded_ = true;
  if (options_.smoke || options_.demo) return;
  const std::wstring file = paths::PlaylistTracksCacheFile();
  if (file.empty() ||
      ::GetFileAttributesW(file.c_str()) == INVALID_FILE_ATTRIBUTES ||
      !paths::IsSafeOwnedPath(file))
    return;
  try {
    std::ifstream stream(file, std::ios::binary);
    if (!stream) return;
    std::string text((std::istreambuf_iterator<char>(stream)),
                     std::istreambuf_iterator<char>());
    std::vector<CachedPlaylistTracks> entries;
    if (ParsePlaylistTracksCacheDoc(json::parse(text), &entries))
      playlist_tracks_cache_ = std::move(entries);
  } catch (const std::exception&) {
    // An unreadable/foreign cache file is ignored, never fatal.
  }
}

void Application::ApplyPlaylistCoverFallbacks() {
  if (playlists_.empty()) return;
  EnsurePlaylistTracksCacheLoaded();
  for (PlaylistRef& playlist : playlists_) {
    if (!playlist.cover_url.empty()) continue;
    const auto found = std::find_if(
        playlist_tracks_cache_.begin(), playlist_tracks_cache_.end(),
        [&playlist](const CachedPlaylistTracks& entry) {
          return entry.id == playlist.id;
        });
    if (found == playlist_tracks_cache_.end()) continue;
    const std::vector<TrackRef>& tracks = found->value.tracks;
    if (tracks.empty() || tracks.front().cover_url.empty()) continue;
    playlist.cover_url = tracks.front().cover_url;
  }
}

void Application::EagerFetchCoverlessPlaylists() {
  if (options_.smoke || options_.demo || playlists_.empty()) return;
  std::vector<const PlaylistRef*> coverless;
  coverless.reserve(playlists_.size());
  for (const PlaylistRef& playlist : playlists_)
    if (playlist.cover_url.empty()) coverless.push_back(&playlist);
  if (coverless.empty()) return;
  // Order by rail visibility so the visible band fills first: the window's
  // filteredPlaylistIndices_ holds middle indices (0 = Queue), so a
  // playlist's rail rank is the position of (vector index + 1) in that
  // list; playlists filtered out rank last.
  const std::vector<int>& filtered = window_.filteredPlaylistIndices();
  std::stable_sort(coverless.begin(), coverless.end(),
                   [&filtered, this](const PlaylistRef* a,
                                     const PlaylistRef* b) {
                     auto rank = [&filtered, this](const PlaylistRef* playlist) {
                       const int middleIndex =
                           static_cast<int>(playlist - playlists_.data()) + 1;
                       for (size_t row = 1; row < filtered.size(); ++row)
                         if (filtered[row] == middleIndex)
                           return static_cast<int>(row);
                       return std::numeric_limits<int>::max();
                     };
                     return rank(a) < rank(b);
                   });
  // Cap the eager batch: the track lists of the first coverless playlists
  // are enough to fill the visible rail, and the serial API queue must not
  // be flooded at startup (the rest still fetch on click).
  constexpr size_t kEagerCoverlessFetchCap = 20;
  const size_t count = std::min(coverless.size(), kEagerCoverlessFetchCap);
  for (size_t i = 0; i < count; ++i)
    FetchPlaylistTracksBackground(coverless[i]->id, /*refreshOnly=*/false);
}

void Application::SavePlaylistTracksCache(const std::string& id,
                                          const CachedTrackList& value) {
  if (options_.smoke || options_.demo) return;
  EnsurePlaylistTracksCacheLoaded();
  CachedPlaylistTracks entry{id, value, NowUnixSeconds()};
  const auto found = std::find_if(
      playlist_tracks_cache_.begin(), playlist_tracks_cache_.end(),
      [&id](const CachedPlaylistTracks& cached) { return cached.id == id; });
  if (found == playlist_tracks_cache_.end()) {
    playlist_tracks_cache_.push_back(std::move(entry));
  } else {
    *found = std::move(entry);
  }
  TrimPlaylistTracksCache(&playlist_tracks_cache_);
  const std::wstring file = paths::PlaylistTracksCacheFile();
  if (file.empty()) return;
  paths::AtomicWriteOwnedFile(
      file, BuildPlaylistTracksCacheDoc(playlist_tracks_cache_,
                                        NowUnixSeconds())
                .dump());
}

void Application::InvalidatePlaylistTracksCache(const std::string& id) {
  if (options_.smoke || options_.demo) return;
  EnsurePlaylistTracksCacheLoaded();
  const auto found = std::find_if(
      playlist_tracks_cache_.begin(), playlist_tracks_cache_.end(),
      [&id](const CachedPlaylistTracks& cached) { return cached.id == id; });
  if (found == playlist_tracks_cache_.end()) return;
  playlist_tracks_cache_.erase(found);
  const std::wstring file = paths::PlaylistTracksCacheFile();
  if (file.empty()) return;
  paths::AtomicWriteOwnedFile(
      file, BuildPlaylistTracksCacheDoc(playlist_tracks_cache_,
                                        NowUnixSeconds())
                .dump());
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
      [this, id = album.id] {
        std::vector<TrackRef> tracks;
        std::string error;
        if (!engine_.BrowseAlbum(id, &tracks, &error))
          throw std::runtime_error(error.empty() ? "album unavailable" : error);
        return tracks;
      },
      [this, album](std::vector<TrackRef> tracks) {
        ApplyAlbumMetadata(tracks, album);
        CachedTrackList cached{std::move(tracks), {}};
        current_album_ = album;
        middle_mode_ = MiddleMode::AlbumTracks;
        window_.SetMiddleLabel(Utf8ToWide(album.name));
        window_.SetMiddleTracks(cached.tracks);
        track_cache_.Put("a:" + album.id, std::move(cached));
      },
      [this](std::string message) {
        HandleApiError(message, L"Album tracks");
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
  if (playback_.auth_state == EngineAuthState::NeedsLogin) auth = L"needs login";
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
  // Rate-limit retries and lazy playlist pagination dispatch here, one task
  // at a time onto the serial API queue.
  delayed_api_tasks_.RunDue(std::chrono::steady_clock::now(),
                            [this](std::function<void()> task) {
                              api_tasks_.Post(std::move(task));
                            });
  const int64_t projected =
      projector_.Current(::GetTickCount64(), playback_.duration_ms);
  if (projected != playback_.position_ms) {
    playback_.position_ms = projected;
    // Position-only path: no full playback-state copy (the queue alone is
    // hundreds of strings) on every 250 ms tick.
    window_.SetPlaybackPosition(projected);
  }
}

void Application::OnExit() {
  if (!shutting_down_) ::DestroyWindow(hwnd_);
}

}  // namespace sr
