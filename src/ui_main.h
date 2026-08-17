#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include <windows.h>
#include <commctrl.h>

#include "playback_engine_client.h"
#include "spotify_api.h"

namespace sr {

class Application;
struct ArtworkCache;

class MainWindow {
 public:
  MainWindow() = default;
  ~MainWindow() = default;

  bool Create(HINSTANCE instance, Application* app);
  void Destroy();
  HWND hwnd() const { return hwnd_; }
  void SetSearchResults(const SearchResult& result);
  void SetPlayback(const PlaybackEngineState& playback);
  void SetEngineStatus(const std::wstring& text);
  void Show(bool show);
  bool Visible() const;
  void SetSetupMode(bool setup);
  void SetDemo();

  std::optional<std::wstring> PromptText(HWND owner,
                                         const std::wstring& title,
                                         const std::wstring& initial);
  void SetMiddleTracks(const std::vector<TrackRef>& tracks);
  void SetMiddleAlbums(const std::vector<AlbumRef>& albums);
  void SetPlaylists(const std::vector<PlaylistRef>& playlists);
  void SetQueueTracks(const std::vector<TrackRef>& tracks);
  void SetStatus(const std::wstring& text);
  void SetSetupStatus(const std::wstring& text);
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
  const std::vector<AlbumRef>& middleAlbums() const { return middleAlbums_; }

 private:
  static LRESULT CALLBACK WndProc(HWND, UINT, WPARAM, LPARAM);
  static LRESULT CALLBACK CoverProc(HWND, UINT, WPARAM, LPARAM);

  enum class ListRowKind { Track, Album, Artist };
  struct ListRow {
    ListRowKind kind = ListRowKind::Track;
    std::wstring title;
    std::wstring eyebrow;
    std::wstring detail;
    std::wstring duration;
    std::wstring accessibleText;
    uint32_t artworkSeed = 0;
    std::string artworkUrl;
  };
  enum class WorkspaceKind { Collection, Search, Settings };
  enum class CollectionKind { Queue, Playlist, Album, Artist };

  void CreateChildren();
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
  void UpdateListActions();
  void AddTooltip(HWND control, const wchar_t* text);
  void RebuildPlaylistRail();
  void SelectPlaylistRow(int comboIndex, bool activate);
  void ShowWorkspace(WorkspaceKind kind);
  void UpdateWorkspaceHeader();
  void UpdateWorkspaceArtwork(const std::string& url);
  void SetControlGroupVisible(const std::vector<HWND>& controls, bool visible);
  void ActivateSelection(HWND list);
  void RequestArtwork(const std::vector<ListRow>& rows);
  const ListRow* RowAt(HWND list, int index) const;
  std::wstring JoinArtists(const std::vector<std::string>& artists) const;
  ListRow TrackRow(const TrackRef& track, size_t ordinal = 0) const;
  ListRow AlbumRow(const AlbumRef& album) const;
  ListRow ArtistRow(const ArtistRef& artist) const;
  std::wstring FormatTime(int64_t milliseconds) const;

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
  bool setupMode_ = true;
  bool seekDragging_ = false;
  bool demoMode_ = false;
  bool volumeDragging_ = false;
  WorkspaceKind workspaceKind_ = WorkspaceKind::Settings;
  WorkspaceKind previousWorkspaceKind_ = WorkspaceKind::Collection;
  CollectionKind collectionKind_ = CollectionKind::Queue;
  int selectedMiddleIndex_ = 0;
  std::vector<int> filteredPlaylistIndices_;
  int64_t workspaceDurationMs_ = 0;
  std::string workspaceArtworkUrl_;
  std::wstring workspaceTitle_ = L"Queue";
  PlaybackEngineState playback_;

  SearchResult search_;
  std::vector<int> resultKinds_;
  std::vector<TrackRef> middleTracks_;
  std::vector<AlbumRef> middleAlbums_;
  std::vector<PlaylistRef> playlists_;
  std::vector<ListRow> searchRows_;
  std::vector<ListRow> middleRows_;
  std::vector<TrackRef> demoTracks_;
  std::wstring resultsEmptyTitle_ = L"Search for something";
  std::wstring resultsEmptyDetail_ = L"Tracks, albums and artists appear here.";
  std::wstring middleEmptyTitle_ = L"Queue is empty";
  std::wstring middleEmptyDetail_ = L"Add tracks from search results to build your queue.";
  bool resultsLoading_ = false;
  bool middleLoading_ = false;
  ArtworkCache* artworkCache_ = nullptr;

  HWND brandLbl_ = nullptr, libraryGroupLbl_ = nullptr;
  HWND playlistFilterEdit_ = nullptr, playlistList_ = nullptr;
  HWND newPlBtn_ = nullptr, settingsBtn_ = nullptr;
  HWND searchEdit_ = nullptr, searchBtn_ = nullptr;
  HWND resultsList_ = nullptr, resultsLabel_ = nullptr, resultsPlayBtn_ = nullptr;
  HWND middleCombo_ = nullptr, tracksList_ = nullptr;
  HWND backBtn_ = nullptr, renPlBtn_ = nullptr, delPlBtn_ = nullptr,
       middlePlayBtn_ = nullptr;
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
  HWND statusLbl_ = nullptr;
  HWND setupTitle_ = nullptr, setupGuide_ = nullptr,
       setupClientIdLabel_ = nullptr;
  HWND setupClientId_ = nullptr, setupRedirectLabel_ = nullptr,
       setupRedirect_ = nullptr;
  HWND setupSaveBtn_ = nullptr, setupAuthBtn_ = nullptr,
       setupContinueBtn_ = nullptr;
  HWND setupStatus_ = nullptr;
  HWND tooltip_ = nullptr;
  HIMAGELIST rowHeightImageList_ = nullptr;
};

}  // namespace sr
