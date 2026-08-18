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
#include "playback_engine_client.h"
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


// Dispatches small follow-up API work (rate-limit retries, playlist
// pagination pacing) one task at a time, earliest deadline first. The UI
// timer drains it onto the serial API TaskQueue, so api.spotify.com never
// sees concurrent or bursting requests. The clock is injectable so ordering
// stays testable without the network.
class DelayedTaskQueue {
 public:
  using Clock = std::chrono::steady_clock;
  using TimePoint = Clock::time_point;
  using NowFn = std::function<TimePoint()>;

  explicit DelayedTaskQueue(NowFn now = [] { return Clock::now(); });

  void Schedule(int delaySeconds, std::function<void()> task);
  // Hands every task whose deadline has passed to `dispatch` (in deadline
  // order, FIFO within a deadline) and returns how many were dispatched.
  int RunDue(TimePoint now,
             const std::function<void(std::function<void()>)>& dispatch);
  void Clear();
  bool Empty() const;
  size_t Size() const;

 private:
  std::deque<std::pair<TimePoint, std::function<void()>>> pending_;
  NowFn now_;
};

// Rate-limit (429/503) auto-retry cap per API task: bounded so a persistent
// limit can never spin forever; the final failure is shown to the user.
inline constexpr int kMaxApiRetries = 3;
// Playlist-library fetch size for the engine browse_playlists round-trip.
inline constexpr int kPlaylistFetchLength = 500;

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

// Tracks which transport fields the UI has optimistically changed while their
// engine commands are still in flight. Engine state events emitted before the
// command was processed (heartbeat, player transitions) must not overwrite
// these values; the override for a field is released when the response for the
// owning request arrives, and the post-command state that immediately follows
// it is authoritative. Latest intent wins: a newer command for the same field
// simply takes over the override.
class PlaybackStateReconciler {
 public:
  // Marks `field` as optimistically owned by `requestId`. An empty requestId
  // clears the override. Field names are the engine state field names
  // ("playing", "position_ms", "duration_ms", "volume", "shuffle", "repeat",
  // "current_index", "current_uri", "queue").
  void SetOverride(const std::string& field, const std::string& requestId);
  // Releases overrides owned by `requestId`; fields taken over by a newer
  // request stay overridden.
  void Confirm(const std::string& requestId);
  bool Overridden(const std::string& field) const;
  // field -> owning request id; read-only view used by ReconcileEngineState.
  const std::unordered_map<std::string, std::string>& PendingFields() const;
  void Reset();
  bool HasPending() const;

 private:
  std::unordered_map<std::string, std::string> fields_;
};

// Returns `incoming` with every field that still has an unconfirmed optimistic
// override replaced by its value in `current`. Only apply the result.
PlaybackEngineState ReconcileEngineState(PlaybackEngineState incoming,
                                         const PlaybackEngineState& current,
                                         const PlaybackStateReconciler& overrides);

enum class MiddleMode { Queue, Playlist, ArtistTracks, AlbumTracks };

class Application {
 public:
  Application() = default;
  ~Application();
  Application(const Application&) = delete;
  Application& operator=(const Application&) = delete;

  int Run(HINSTANCE instance, int show, const RunOptions& options);

  bool IsAuthed() const;

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
  // Settings session controls: opens the engine-published Spotify authorize
  // URL in the browser and starts the engine OAuth flow on demand.
  void OnLogin();
  // Clears the cached credentials and tears the session down (engine-side);
  // the needs_login state event flips the Settings page immediately.
  void OnLogout();

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

  std::string GetAccessToken();
  bool RefreshToken(int timeoutMs = 20000);
  void HandleApiError(const std::string& message, int status, int retryAfter,
                      const std::wstring& context);
  // Retryable-error handling + startup orchestration (RateLimitFix wave):
  // bounded 429/503 auto-retry with visible status, lazy paced playlist
  // pagination, and the on-disk playlist library cache. Engine protocol,
  // token minting, and the Settings login/logout surface are owned by the
  // LoginLogout wave; do not overlap those members.
  void ScheduleDelayedApiTask(int delaySeconds, std::function<void()> task);
  bool LoadPlaylistCache();
  void SavePlaylistCache();
  // Fetches the whole playlist library in one engine browse_playlists
  // round-trip (spclient rootlist; no Web API pagination).
  void FetchPlaylists();

  void PlayTracks(const std::vector<TrackRef>& tracks, int index);
  // Runs an engine command; returns the request id the transport assigned (or
  // empty when the command could not be sent). Callers register optimistic
  // overrides keyed by that id.
  std::string RunEngineCommand(const std::function<std::string()>& command,
                               const std::wstring& failureContext);
  bool EngineReady();
  void OnEngineState(PlaybackEngineState state);
  void OnEngineError(std::string error);
  void OnEngineCommandError(std::string error);
  void ResetProjectionBase();
  // Retries through the engine's own Status path (the engine re-authenticates
  // with cached credentials after its player thread died). Cooldown-guarded;
  // falls back to a process restart when the engine is gone.
  void TryRecoverEngine();
  // Schedules a respawn of the engine subprocess with exponential backoff.
  // The next OnTimer tick past the deadline performs the actual restart.
  void ScheduleEngineRestart();
  // Re-sends queue/volume/shuffle/repeat after a respawned engine reports
  // ready, so a transient engine failure does not lose playback.
  void RestorePlaybackAfterRespawn();
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
  std::optional<WebApiTokenProvider> web_token_;

  std::shared_ptr<HttpClient> api_http_;
  std::unique_ptr<SpotifyApi> api_;
  PlaybackEngineClient engine_;
  PlaybackEngineState playback_;
  PositionProjector projector_;
  PlaybackStateReconciler playback_overrides_;
  MainWindow window_;
  TrayIcon tray_;
  TaskQueue api_tasks_;
  TaskQueue artwork_tasks_;
  // RateLimitFix wave: delayed follow-up API work (see DelayedTaskQueue).
  DelayedTaskQueue delayed_api_tasks_;

  bool shutting_down_ = false;
  bool engine_restart_pending_ = false;
  std::chrono::steady_clock::time_point engine_restart_at_{};
  int engine_restart_attempts_ = 0;
  bool restore_playback_pending_ = false;
  ULONGLONG last_recovery_attempt_tick_ = 0;
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
  // Bounded 429/503 auto-retry variant; see definition below.
  template <typename T>
  void PostTaskWithRetries(std::function<T()> work,
                           std::function<void(T)> onSuccess,
                           std::function<void(std::string, int, int)> onError,
                           int retriesLeft);
};

template <typename T>
void Application::PostTask(std::function<T()> work,
                           std::function<void(T)> onSuccess,
                           std::function<void(std::string, int, int)> onError) {
  if (options_.smoke || options_.demo || !api_) return;
  PostTaskWithRetries(std::move(work), std::move(onSuccess), std::move(onError),
                      kMaxApiRetries);
}

// Runs `work` on the serial API queue. Retryable rate limits (429/503, the
// response was rejected so re-running is safe) are retried up to `retriesLeft`
// times with exponential backoff honoring Retry-After; each wait is visible
// as a status line and does not block the queue. After the cap, or for any
// other failure, the error is delivered to `onError` on the UI thread.
template <typename T>
void Application::PostTaskWithRetries(
    std::function<T()> work, std::function<void(T)> onSuccess,
    std::function<void(std::string, int, int)> onError, int retriesLeft) {
  api_tasks_.Post([this, work = std::move(work), onSuccess = std::move(onSuccess),
                   onError = std::move(onError), retriesLeft]() mutable {
    try {
      T result = work();
      PostUi([onSuccess = std::move(onSuccess),
              result = std::move(result)]() mutable {
        onSuccess(std::move(result));
      });
    } catch (const ApiError& error) {
      const std::string message = error.what();
      const int status = error.status;
      const int retryAfter = error.retry_after;
      if ((status == 429 || status == 503) && retriesLeft > 0 &&
          !shutting_down_) {
        const int wait = static_cast<int>(
            ComputeBackoffDelay(kMaxApiRetries - retriesLeft, retryAfter));
        PostUi([this, work = std::move(work), onSuccess = std::move(onSuccess),
                onError = std::move(onError), message, wait,
                retriesLeft]() mutable {
          window_.SetStatus(
              L"Spotify is rate-limiting requests — retrying automatically "
              L"in " +
              std::to_wstring(wait) + L" s.");
          ScheduleDelayedApiTask(
              wait, [this, work = std::move(work),
                     onSuccess = std::move(onSuccess),
                     onError = std::move(onError), retriesLeft]() mutable {
                PostTaskWithRetries(std::move(work), std::move(onSuccess),
                                    std::move(onError), retriesLeft - 1);
              });
        });
        return;
      }
      PostUi([onError = std::move(onError), message, status,
              retryAfter]() mutable {
        onError(std::move(message), status, retryAfter);
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
