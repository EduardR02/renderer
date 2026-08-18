#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include <windows.h>
#include <commctrl.h>

#include "playback_engine_client.h"
#include "spotify_api.h"
#include "ui_rows.h"
namespace sr {

class Application;
struct ArtworkCache;

class MainWindow {
 public:
  enum class WorkspaceKind { Collection, Search, Settings };

  MainWindow() = default;
  ~MainWindow() = default;

  bool Create(HINSTANCE instance, Application* app);
  void Destroy();
  HWND hwnd() const { return hwnd_; }
  void SetSearchResults(const SearchResult& result);
  void SetPlayback(const PlaybackEngineState& playback);
  // Timer-driven position projection: updates only the seek bar and elapsed
  // label without copying the whole playback state (queue included) at 4 Hz.
  void SetPlaybackPosition(int64_t positionMs);
  void FocusSearch();
  void SetEngineStatus(const std::wstring& text);
  void SetCacheUsage(const std::wstring& text);
  void Show(bool show);
  bool Visible() const;
  void ShowWorkspace(WorkspaceKind kind);
  void SetDemo();

  std::optional<std::wstring> PromptText(HWND owner,
                                         const std::wstring& title,
                                         const std::wstring& initial);
  void SetMiddleTracks(const std::vector<TrackRef>& tracks);
  void SetArtistPage(const ArtistRef& artist, const std::vector<TrackRef>& tracks);
  void SetPlaylists(const std::vector<PlaylistRef>& playlists);
  void SetQueueTracks(const std::vector<TrackRef>& tracks);
  void SetStatus(const std::wstring& text);
  void SetCoverFile(const std::wstring& path);
  void SetTrackArtwork(const std::string& url, const std::wstring& path);
  void SetMiddleMode(int modeIndex);
  void SetMiddleLabel(const std::wstring& text);

  std::string SearchQuery() const;
  int SelectedTrackIndex() const;
  int SelectedResultIndex() const;
  int MiddleComboIndex() const;
  const std::vector<PlaylistRef>& playlists() const { return playlists_; }
  const SearchResult& search() const { return search_; }
  const std::vector<int>& resultKinds() const { return resultKinds_; }
  const std::vector<TrackRef>& middleTracks() const { return middleTracks_; }

 private:
  static LRESULT CALLBACK WndProc(HWND, UINT, WPARAM, LPARAM);
  static LRESULT CALLBACK CoverProc(HWND, UINT, WPARAM, LPARAM);

  enum class CollectionKind { Queue, Playlist, Album, Artist };

  // Entry pushed before navigating into an album/artist page so Back can
  // restore the exact prior workspace, collection, and rail selection.
  struct NavEntry {
    WorkspaceKind workspace;
    CollectionKind collection;
    int middleIndex;
    std::wstring title;
    std::string artworkUrl;
  };

  void CreateChildren();
  void ArrangeTabOrder();
  void Layout();
  void ApplyFonts();
  void SetDarkTheme();
  LRESULT OnDrawItem(WPARAM, LPARAM);
  LRESULT OnMeasureItem(WPARAM, LPARAM);
  LRESULT OnNotify(WPARAM, LPARAM);
  void ShowContextMenu(HWND control, int x, int y);
  void FillList(HWND list, const std::vector<ListRow>& rows);
  void SetListMessage(HWND list, const std::wstring& title,
                      const std::wstring& detail);
  void AddTooltip(HWND control, const wchar_t* text);
  void SetTooltipText(HWND control, const std::wstring& text);
  void RebuildPlaylistRail();
  void SelectPlaylistRow(int comboIndex, bool activate);
  void BeginNestedCollection(CollectionKind kind, const std::wstring& title,
                             const std::string& artworkUrl);
  void PopNestedCollection();
  void UpdateWorkspaceHeader();
  void UpdateWorkspaceArtwork(const std::string& url);
  void SetControlGroupVisible(const std::vector<HWND>& controls, bool visible);
  void ActivateSelection(HWND list);
  void RequestArtwork(const std::vector<ListRow>& rows);
  // Repaints every row of `list` whose uri matches, so the active-row
  // highlight (and its pause toggle) follows the engine's current track
  // without repainting the whole list.
  void InvalidateRowsForUri(HWND list, const std::vector<ListRow>& rows,
                            const std::string& uri);
  const ListRow* RowAt(HWND list, int index) const;
  COLORREF ButtonBaseColor(HWND control) const;

  Application* app_ = nullptr;
  HINSTANCE hinst_ = nullptr;
  HWND hwnd_ = nullptr;
  HFONT fontUi_ = nullptr, fontList_ = nullptr, fontRowTitle_ = nullptr,
        fontTitle_ = nullptr, fontDisplay_ = nullptr, fontSmall_ = nullptr,
        fontIcon16_ = nullptr, fontIcon20_ = nullptr, fontIcon24_ = nullptr,
        fontIcon40_ = nullptr;
  HBRUSH brushBg_ = nullptr, brushSidebar_ = nullptr, brushPanel_ = nullptr,
         brushEdit_ = nullptr, brushControl_ = nullptr, brushPlayer_ = nullptr;
  UINT dpi_ = 96;
  bool seekDragging_ = false;
  bool demoMode_ = false;
  bool volumeDragging_ = false;
  WorkspaceKind workspaceKind_ = WorkspaceKind::Settings;
  WorkspaceKind previousWorkspaceKind_ = WorkspaceKind::Collection;
  CollectionKind collectionKind_ = CollectionKind::Queue;
  int selectedMiddleIndex_ = 0;
  bool suppressNextDoubleActivate_ = false;
  std::vector<NavEntry> navStack_;
  std::vector<int> filteredPlaylistIndices_;
  int64_t workspaceDurationMs_ = 0;
  std::string workspaceArtworkUrl_;
  std::wstring workspaceTitle_ = L"Queue";
  PlaybackEngineState playback_;

  SearchResult search_;
  std::vector<int> resultKinds_;
  std::vector<TrackRef> middleTracks_;
  std::vector<PlaylistRef> playlists_;
  std::vector<ListRow> searchRows_;
  std::vector<ListRow> middleRows_;
  std::vector<TrackRef> demoTracks_;
  bool resultsLoading_ = false;
  bool middleLoading_ = false;
  ArtworkCache* artworkCache_ = nullptr;
  std::wstring resultsEmptyTitle_ = L"Search for something";
  std::wstring resultsEmptyDetail_ = L"Tracks, albums and artists appear here.";
  std::wstring middleEmptyTitle_ = L"Queue is empty";
  std::wstring middleEmptyDetail_ = L"Add tracks from search results to build your queue.";
  HWND brandLbl_ = nullptr, libraryGroupLbl_ = nullptr;
  HWND playlistFilterEdit_ = nullptr, playlistList_ = nullptr;
  HWND newPlBtn_ = nullptr, settingsBtn_ = nullptr;
  HWND searchEdit_ = nullptr, searchBtn_ = nullptr;
  HWND resultsList_ = nullptr, resultsLabel_ = nullptr;
  HWND middleCombo_ = nullptr, tracksList_ = nullptr;
  HWND backBtn_ = nullptr, renPlBtn_ = nullptr, delPlBtn_ = nullptr;
  HWND middleLabel_ = nullptr, workspaceTypeLbl_ = nullptr,
       workspaceMetaLbl_ = nullptr, workspaceColumnsLbl_ = nullptr,
       workspaceTimeColumnLbl_ = nullptr,
       workspaceActionColumnLbl_ = nullptr;
  HWND workspaceCover_ = nullptr;
  HWND coverArea_ = nullptr, nowPlayingLbl_ = nullptr, titleLbl_ = nullptr;
  HWND artistLbl_ = nullptr, albumLbl_ = nullptr;
  HWND elapsedLbl_ = nullptr, durationLbl_ = nullptr, seekBar_ = nullptr;
  HWND prevBtn_ = nullptr, playBtn_ = nullptr, nextBtn_ = nullptr;
  HWND shuffleBtn_ = nullptr, repeatBtn_ = nullptr;
  HWND volumeLbl_ = nullptr, volumeBar_ = nullptr, localControlsLbl_ = nullptr;
  HWND engineGroupLbl_ = nullptr, engineGuideLbl_ = nullptr,
       engineStatusLbl_ = nullptr, cacheStatusLbl_ = nullptr;
  HWND loginBtn_ = nullptr, logoutBtn_ = nullptr;
  HWND statusLbl_ = nullptr;
  HWND settingsTitle_ = nullptr, settingsGuide_ = nullptr;
  HWND tooltip_ = nullptr;
  HIMAGELIST rowHeightImageList_ = nullptr;
};

}  // namespace sr
