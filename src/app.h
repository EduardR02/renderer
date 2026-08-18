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

// Playlist-library fetch size for the engine browse_playlists round-trip.
inline constexpr int kPlaylistFetchLength = 500;

// Retry schedule for a failed playlist-library fetch: the first retry waits
// `kPlaylistRetryBaseSeconds`, doubling per attempt up to
// `kPlaylistRetryMaxSeconds`; after `kPlaylistRetryMaxAttempts` failures the
// error is surfaced as final. Retries run on the UI-timer-drained
// DelayedTaskQueue, so the engine never sees a burst.
inline constexpr int kPlaylistRetryBaseSeconds = 5;
inline constexpr int kPlaylistRetryMaxSeconds = 60;
inline constexpr int kPlaylistRetryMaxAttempts = 5;

// Capped exponential backoff (seconds) for retry `attempt` (0-based) of the
// playlist-library refetch. Pure so the schedule is unit-testable.
int PlaylistRetryDelaySeconds(int attempt);

// What the on-disk playlist cache may serve:
enum class PlaylistCacheUse {
  None,           // absent/unusable, or stale with a fresh fetch pending
  Fresh,          // within TTL: usable without any fetch
  StaleFallback,  // stale, usable only because the fresh fetch already failed
};

// Classifies the on-disk playlist cache for the startup/refresh path:
// `fetchFailed` is true when the fresh engine browse already failed; only
// then may a stale cache serve the library (shown immediately, with a
// background retry scheduled behind it). Pure so the startup decision is
// unit-testable.
PlaylistCacheUse ClassifyPlaylistCache(int64_t fetchedAtUnixSeconds,
                                       int64_t nowUnixSeconds,
                                       int64_t ttlMinutes,
                                       bool fetchFailed);

// One persisted browse_playlist cache entry: the TrackRef list, the edit
// revision, and the unix-seconds time it was fetched. Per-entry timestamps
// let old playlists stay cached without going stale as a group.
struct CachedPlaylistTracks {
  std::string id;
  CachedTrackList value;
  int64_t fetched_at = 0;
};

// Persisted browse_playlist results: the last `kPlaylistTracksCacheCapacity`
// playlists fetched, each fresh for `kPlaylistTracksTtlMinutes`. Playlist
// clicks resolve from disk instantly (no engine round-trip); a stale copy is
// shown immediately with a background refresh; a failed refresh keeps the
// cached copy instead of surfacing an error.
inline constexpr int kPlaylistTracksCacheCapacity = 25;
inline constexpr int kPlaylistTracksTtlMinutes = 30;

// Sorts entries most-recent-first (stable) and drops the oldest beyond
// kPlaylistTracksCacheCapacity. Pure so eviction is unit-testable.
void TrimPlaylistTracksCache(std::vector<CachedPlaylistTracks>* entries);

// Serializes entries to the version-1 cache document (the shape
// ParsePlaylistTracksCacheDoc reads back). Pure so persistence is
// unit-testable without the Application.
nlohmann::json BuildPlaylistTracksCacheDoc(
    const std::vector<CachedPlaylistTracks>& entries, int64_t nowUnixSeconds);

// Parses a cache document into entries, skipping malformed rows. Returns
// false (leaving *out untouched) when the document is not a usable version-1
// cache. Pure so loading is unit-testable without the Application.
bool ParsePlaylistTracksCacheDoc(const nlohmann::json& doc,
                                 std::vector<CachedPlaylistTracks>* out);

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

  void HandleApiError(const std::string& message, const std::wstring& context);
  void ScheduleDelayedApiTask(int delaySeconds, std::function<void()> task);
  // Loads the on-disk playlist library cache into playlists_ and shows it.
  // `allowStale` permits serving an expired copy as a fallback after the
  // fresh fetch failed (the caller schedules the background refetch);
  // without it a stale cache is skipped so the refetch actually runs.
  bool LoadPlaylistCache(bool allowStale = false);
  void SavePlaylistCache();
  // Fetches the whole playlist library in one engine browse_playlists
  // round-trip (spclient rootlist; no Web API pagination). On failure the
  // stale on-disk cache is served immediately (never an empty library) and
  // the fetch is retried in the background with capped exponential backoff.
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
  // Backfills the cover_url of playlists the server has no artwork for from
  // the persisted tracks cache (the first track's cover — the same art the
  // playlist view shows). Mirrors the official client's client-side tile
  // for coverless playlists; the server data is never modified.
  void ApplyPlaylistCoverFallbacks();
  // Kicks off background track fetches for playlists the server still has
  // no artwork for, so the rail's mosaic tiles populate without waiting
  // for a click. Visible rail rows are fetched first (the window's
  // filtered row order) and the batch is capped so a large coverless
  // library never floods the serial API queue at startup.
  void EagerFetchCoverlessPlaylists();
  // Engine round-trip for one playlist's tracks. `refreshOnly` is true when
  // a (stale) cached copy is already on screen: a failure then keeps the
  // cached copy visible instead of surfacing an error.
  void FetchPlaylistTracksBackground(const std::string& id, bool refreshOnly);
  // Lazy load of the on-disk playlist-tracks cache (once per process).
  void EnsurePlaylistTracksCacheLoaded();
  // Upserts one fetched playlist into the disk cache and persists it
  // (bounded to kPlaylistTracksCacheCapacity, trimmed oldest-first).
  void SavePlaylistTracksCache(const std::string& id, const CachedTrackList& value);
  // Drops one playlist from the disk cache (after edits make it stale).
  void InvalidatePlaylistTracksCache(const std::string& id);
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
  // Failed playlist-library fetches retried so far (reset on success); the
  // retry backoff doubles per attempt up to the cap in FetchPlaylists.
  int playlist_retry_attempts_ = 0;
  std::string me_id_;
  std::string last_cover_url_;
  std::string current_playlist_id_;
  std::string current_playlist_snapshot_;
  MiddleMode middle_mode_ = MiddleMode::Queue;
  TrackListCache track_cache_;
  std::optional<std::chrono::steady_clock::time_point> playlists_fetched_at_;
  // On-disk browse_playlist cache mirror (persisted to
  // paths::PlaylistTracksCacheFile, most-recent-first). UI thread only.
  std::vector<CachedPlaylistTracks> playlist_tracks_cache_;
  bool playlist_tracks_cache_loaded_ = false;
  AlbumRef current_album_;
  RunOptions options_;

  template <typename T>
  void PostTask(std::function<T()> work, std::function<void(T)> onSuccess,
                std::function<void(std::string)> onError);
};

// Runs `work` on the serial API queue (engine browse/edit round-trips are
// blocking, so they never run on the UI thread) and delivers the result or
// the error text on the UI thread.
template <typename T>
void Application::PostTask(std::function<T()> work,
                           std::function<void(T)> onSuccess,
                           std::function<void(std::string)> onError) {
  if (options_.smoke || options_.demo) return;
  api_tasks_.Post([this, work = std::move(work), onSuccess = std::move(onSuccess),
                   onError = std::move(onError)]() mutable {
    try {
      T result = work();
      PostUi([onSuccess = std::move(onSuccess),
              result = std::move(result)]() mutable {
        onSuccess(std::move(result));
      });
    } catch (const std::exception& error) {
      const std::string message = error.what();
      PostUi([onError = std::move(onError), message]() mutable {
        onError(std::move(message));
      });
    }
  });
}

}  // namespace sr