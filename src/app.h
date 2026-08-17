#pragma once

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <functional>
#include <list>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

#include <windows.h>

#include "http.h"
#include "messages.h"
#include "oauth.h"
#include "playback_engine_client.h"
#include "secure_store.h"
#include "settings.h"
#include "spotify_api.h"
#include "tray.h"
#include "ui_main.h"

namespace sr {

class TaskQueue {
 public:
  void Start();
  void Stop();
  void DiscardPending();
  void Post(std::function<void()> fn);

 private:
  std::thread thread_;
  std::deque<std::function<void()>> tasks_;
  std::mutex mutex_;
  std::condition_variable wake_;
  std::atomic<bool> stop_{false};
};

// A fetched track list (playlist or album) plus the playlist snapshot id used
// for edits; albums leave snapshot_id empty.
struct CachedTrackList {
  std::vector<TrackRef> tracks;
  std::string snapshot_id;
};

// Bounded LRU cache of fetched track lists keyed by resource id ("p:"+id for
// playlists, "a:"+id for albums). Entries stay fresh for ttl after insertion;
// stale entries are dropped on access. UI thread only.
class TrackListCache {
 public:
  using Clock = std::chrono::steady_clock;
  using TimePoint = Clock::time_point;
  using NowFn = std::function<TimePoint()>;

  explicit TrackListCache(size_t capacity = 20,
                          Clock::duration ttl = std::chrono::minutes(10),
                          NowFn now = [] { return Clock::now(); });

  // Copies a fresh entry into out and marks it most recently used. Returns
  // false when the key is absent or stale (a stale entry is evicted).
  bool Get(const std::string& key, CachedTrackList* out);
  void Put(const std::string& key, CachedTrackList value);
  void Invalidate(const std::string& key);
  void Clear();
  size_t Size() const;

 private:
  struct Entry {
    CachedTrackList value;
    TimePoint fetched;
  };

  size_t capacity_;
  Clock::duration ttl_;
  NowFn now_;
  // Front is the most recently used entry.
  std::list<std::pair<std::string, Entry>> order_;
  std::unordered_map<std::string,
                     std::list<std::pair<std::string, Entry>>::iterator>
      index_;
};

// True when an HTTP error on playlist content is the known Spotify
// development-mode restriction: development-mode apps receive 403 for
// playlists owned by accounts that are not on the app's dashboard allowlist
// (Settings > Users and Access); extended quota mode lifts the restriction.
bool IsDevModePlaylistRestriction(int status, const std::string& ownerId,
                                  const std::string& meId);


struct RunOptions {
  bool smoke = false;
  int smokeSeconds = 6;
  bool demo = false;
  std::wstring isolationTestRoot;
};

// Projects a playback position forward between engine state events. The engine
// emits state on transitions and a 2-second heartbeat while playing; the UI
// timer renders the projected position so the seek bar moves smoothly without
// polling. While paused (or with an unknown duration) the position is static.
class PositionProjector {
 public:
  // Records a new base: an authoritative engine event, a control intent, or a
  // seek release. While playing, Current() advances from this base.
  void Reset(int64_t positionMs, bool playing, ULONGLONG nowTick);
  // Projected position in milliseconds, clamped to [0, durationMs].
  int64_t Current(ULONGLONG nowTick, int64_t durationMs) const;

 private:
  int64_t base_position_ms_ = 0;
  ULONGLONG base_tick_ = 0;
  bool playing_ = false;
};

enum class MiddleMode { Queue, Playlist, ArtistTracks, AlbumTracks };

class Application {
 public:
  Application() = default;
  ~Application();
  Application(const Application&) = delete;
  Application& operator=(const Application&) = delete;

  int Run(HINSTANCE instance, int show, const RunOptions& options);

  bool IsAuthed() const;
  Settings GetSettings() const;
  void OnSetupSave(const std::string& clientId, const std::string& redirectUri);
  void OnAuthenticate();
  void OnOAuthResult(const std::string& result);

  void OnSearch(const std::string& query);
  void OnSearchActivate(int item);
  void OnSearchContext(UINT command, int item);
  void OnAddToPlaylist(int playlistIndex);
  void OnTrackArtworkNeeded(const std::string& url);

  void OnMiddleCombo(int index);
  void OnMiddleActivate(int index);
  void OnMiddleContext(UINT command, int index);
  void OnBack();
  void OnNewPlaylist();
  void OnRenamePlaylist();
  void OnDeletePlaylist();

  void OnTogglePlay();
  void OnNext();
  void OnPrevious();
  void OnSeekTo(int positionMs);
  void OnSetVolumePercent(int volumePercent);
  void OnToggleShuffle();
  void OnCycleRepeat();

  void OnRefreshAll();
  // Recomputes engine audio-cache usage and publishes it to the Settings page.
  void OnSettingsShown();
  void OnTrayShow();
  void OnTrayCommand(UINT id);
  void OnTimer(UINT id);
  void OnExit();
  void PostUi(std::function<void()> fn);

 private:
  void InitCore();
  void StartEngine();
  void Shutdown();
  void StartTimers();
  void StopTimers();

  std::string GetAccessToken() const;
  bool RefreshToken(int timeoutMs = 20000);
  void SaveTokens(const TokenResponse& token);
  void ClearTokens();
  void HandleApiError(const std::string& message, int status, int retryAfter,
                      const std::wstring& context);

  void PlayTracks(const std::vector<TrackRef>& tracks, int index);
  void RunEngineCommand(const std::function<void()>& command,
                        const std::wstring& failureContext);
  bool EngineReady();
  void OnEngineState(PlaybackEngineState state);
  void OnEngineError(std::string error);
  void OnEngineCommandError(std::string error);
  void ResetProjectionBase();
  void RefreshQueue();
  void RefreshPlaylists(bool force = false);
  void RequestPlaylistTracks(const std::string& id);
  void ShowPlaylistTracks(const std::string& id, const CachedTrackList& cached);
  void OpenAlbumTracks(const AlbumRef& album);
  void EnsureCover(const std::string& url);
  void RequestCoverFile(const std::string& url, bool nowPlaying);
  void UpdatePlaybackUi();
  std::wstring EngineStatusText() const;
  std::string IsolationStatus() const;

  HINSTANCE instance_ = nullptr;
  HWND hwnd_ = nullptr;
  HACCEL accelerators_ = nullptr;
  Settings settings_;
  mutable std::mutex settings_mutex_;
  std::optional<TokenSet> tokens_;
  mutable std::mutex tokens_mutex_;

  std::shared_ptr<HttpClient> api_http_;
  std::shared_ptr<HttpClient> accounts_http_;
  std::unique_ptr<SpotifyApi> api_;
  PlaybackEngineClient engine_;
  PlaybackEngineState playback_;
  PositionProjector projector_;
  MainWindow window_;
  TrayIcon tray_;
  TaskQueue api_tasks_;
  TaskQueue artwork_tasks_;
  LoopbackListener oauth_listener_;
  Pkce pending_pkce_;
  std::string pending_state_;

  bool shutting_down_ = false;
  ULONG_PTR gdiplus_token_ = 0;
  std::vector<PlaylistRef> playlists_;
  std::string me_id_;
  std::string last_cover_url_;
  std::string current_playlist_id_;
  std::string current_playlist_snapshot_;
  MiddleMode middle_mode_ = MiddleMode::Queue;
  TrackListCache track_cache_;
  std::optional<std::chrono::steady_clock::time_point> playlists_fetched_at_;
  AlbumRef current_album_;
  RunOptions options_;

  template <typename T>
  void PostTask(std::function<T()> work, std::function<void(T)> onSuccess,
                std::function<void(std::string, int, int)> onError);
};

template <typename T>
void Application::PostTask(std::function<T()> work,
                           std::function<void(T)> onSuccess,
                           std::function<void(std::string, int, int)> onError) {
  if (options_.smoke || options_.demo || !api_) return;
  api_tasks_.Post([this, work = std::move(work),
                   onSuccess = std::move(onSuccess),
                   onError = std::move(onError)]() mutable {
    try {
      T result = work();
      PostUi([onSuccess = std::move(onSuccess),
              result = std::move(result)]() mutable {
        onSuccess(std::move(result));
      });
    } catch (const ApiError& error) {
      const std::string message = error.what();
      PostUi([onError = std::move(onError), message, status = error.status,
              retry = error.retry_after]() mutable {
        onError(std::move(message), status, retry);
      });
    } catch (const std::exception& error) {
      const std::string message = error.what();
      PostUi([onError = std::move(onError), message]() mutable {
        onError(std::move(message), 0, 0);
      });
    }
  });
}

}  // namespace sr
