#pragma once

#include <atomic>
#include <condition_variable>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
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

struct RunOptions {
  bool smoke = false;
  int smokeSeconds = 6;
  bool demo = false;
  std::wstring isolationTestRoot;
};

enum class MiddleMode { Queue, Playlist, ArtistAlbums, AlbumTracks };

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
  void RefreshQueue();
  void RefreshPlaylists();
  void RequestPlaylistTracks(const std::string& id);
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
  std::wstring current_artist_name_;
  MiddleMode middle_mode_ = MiddleMode::Queue;
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
