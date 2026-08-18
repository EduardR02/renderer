#pragma once

// Pure row-mapping helpers shared by the workspace UI and unit tests.
// No window handles: everything here is injectable and deterministic.

#include <cstdint>
#include <string>
#include <vector>

#include <windows.h>

#include "spotify_api.h"
#include "util.h"

namespace sr {

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
  // Spotify uri of the underlying item; used to match the engine's
  // current_uri so the playing row can be highlighted and its play button
  // can toggle pause/resume.
  std::string uri;
};

// Row geometry in DIPs, shared by painting and hit testing.
constexpr int kRowArtworkSizeDip = 46;
constexpr int kRowArtworkLeftDip = 9;
constexpr int kRowArtworkTopDip = 9;
constexpr int kRowTextGapDip = 12;
constexpr int kRowRightPaddingDip = 10;
constexpr int kRowActionWidthDip = 44;
constexpr int kRowDurationWidthDip = 48;

inline uint32_t RowArtworkSeed(const std::string& url,
                               const std::wstring& title) {
  uint32_t seed = 2166136261u;
  if (!url.empty()) {
    for (unsigned char ch : url) seed = (seed ^ ch) * 16777619u;
  } else {
    for (wchar_t ch : title) {
      seed = (seed ^ static_cast<uint16_t>(ch)) * 16777619u;
    }
  }
  return seed;
}

inline std::wstring JoinArtists(const std::vector<std::string>& artists) {
  std::wstring joined;
  for (size_t i = 0; i < artists.size(); ++i) {
    if (i) joined += L", ";
    joined += Utf8ToWide(artists[i]);
  }
  return joined;
}

inline std::wstring FormatTime(int64_t ms) {
  int64_t seconds = std::max<int64_t>(0, ms) / 1000;
  int64_t minutes = seconds / 60;
  return std::to_wstring(minutes) + L":" + (seconds % 60 < 10 ? L"0" : L"") +
         std::to_wstring(seconds % 60);
}

inline ListRow MakeTrackRow(const TrackRef& track, size_t ordinal = 0) {
  ListRow row;
  row.kind = ListRowKind::Track;
  row.title = track.name.empty() ? L"Untitled track" : Utf8ToWide(track.name);
  std::wstring artists = JoinArtists(track.artist_names);
  if (artists.empty()) artists = L"Unknown artist";
  std::wstring album =
      track.album_name.empty() ? L"Unknown album" : Utf8ToWide(track.album_name);
  row.eyebrow = L"TRACK";
  if (ordinal > 0) {
    row.eyebrow = (ordinal < 10 ? L"0" : L"") + std::to_wstring(ordinal) +
                  L"  ·  TRACK";
  }
  row.detail = artists + L"  ·  " + album;
  row.duration = track.duration_ms > 0 ? FormatTime(track.duration_ms) : L"—";
  row.accessibleText = row.title + L". Artist: " + artists + L". Album: " +
                       album + L". Duration: " + row.duration;
  row.artworkUrl = track.cover_url;
  row.artworkSeed = RowArtworkSeed(row.artworkUrl, row.title);
  row.uri = track.uri;
  return row;
}

inline ListRow MakeAlbumRow(const AlbumRef& album) {
  ListRow row;
  row.kind = ListRowKind::Album;
  row.title = album.name.empty() ? L"Untitled album" : Utf8ToWide(album.name);
  std::wstring artists = JoinArtists(album.artist_names);
  if (artists.empty()) artists = L"Unknown artist";
  row.eyebrow = L"ALBUM";
  row.detail = artists;
  row.accessibleText = row.title + L". Album by " + artists;
  row.artworkUrl = album.cover_url;
  row.artworkSeed = RowArtworkSeed(row.artworkUrl, row.title);
  row.uri = album.uri;
  return row;
}

inline ListRow MakeArtistRow(const ArtistRef& artist) {
  ListRow row;
  row.kind = ListRowKind::Artist;
  row.title = artist.name.empty() ? L"Unknown artist" : Utf8ToWide(artist.name);
  row.eyebrow = L"ARTIST";
  row.detail = L"Open artist page";
  row.accessibleText = row.title + L". Artist. Open artist page.";
  row.artworkUrl = artist.cover_url;
  row.artworkSeed = RowArtworkSeed(row.artworkUrl, row.title);
  row.uri = artist.uri;
  return row;
}

// The list hover property stores index + 1 so that an absent property (0)
// means "no hover". Returns the zero-based row index or -1.
inline int DecodeHoverIndex(INT_PTR propValue) {
  return static_cast<int>(propValue) - 1;
}

// DIP-space width for a label whose right edge must end `gap` DIPs before a
// sibling control starts at `siblingLeft`. Overlapping siblings are a real
// paint hazard: with WS_CLIPSIBLINGS on both windows each one's update
// region excludes the other's, so the shared band is painted by neither and
// the covered control appears cut off until an interaction forces a repaint.
inline int LabelWidthBefore(int textLeft, int siblingLeft, int gap = 8) {
  return std::max(0, siblingLeft - gap - textLeft);
}

// Top (== bottom) inset in pixels for centering a text line of `lineHeight`
// pixels inside a `clientHeight`-tall box. Used for tall single-line edit
// controls: native edits anchor text, caret, and cue banner to a font-sized
// format rectangle at the top of the client area (EM_SETRECT and top/bottom
// EM_SETMARGINS are no-ops for single-line controls), so the client itself
// is shrunk by this inset per side (WM_NCCALCSIZE) and everything centers
// with it.
inline int EditCenteringInset(int clientHeight, int lineHeight) {
  if (clientHeight <= 0 || lineHeight <= 0) return 0;
  return std::max(0, (clientHeight - lineHeight) / 2);
}

// True when (x, y) client coordinates land on a row's artwork tile; the tile
// is the hover-play target. itemLeft/itemTop are the row bounds' origin.
inline bool RowTileHit(int x, int y, int itemLeft, int itemTop, int dpi) {
  if (dpi <= 0) dpi = 96;
  const int size = ::MulDiv(kRowArtworkSizeDip, dpi, 96);
  const int left = itemLeft + ::MulDiv(kRowArtworkLeftDip, dpi, 96);
  const int top = itemTop + ::MulDiv(kRowArtworkTopDip, dpi, 96);
  return x >= left && x < left + size && y >= top && y < top + size;
}

// Child-control ids of the subclassed edit boxes. They live here so the
// Enter-routing contract is testable without windows; ui_main.cpp binds its
// own control-id enum to these with static_asserts.
constexpr int kSearchEditControlId = 101;
constexpr int kPlaylistFilterEditControlId = 105;

enum class EditRole { Search, Filter, Other };

// Role of a subclassed edit: the main search box routes Enter to the search
// button (identical to clicking it); the rail filter applies live per
// keystroke so Enter must fall through to the default edit behavior.
inline EditRole EditRoleForControl(int controlId) {
  if (controlId == kSearchEditControlId) return EditRole::Search;
  if (controlId == kPlaylistFilterEditControlId) return EditRole::Filter;
  return EditRole::Other;
}

// Maps a playlist-rail row to its playlist. Row 0 is the Queue entry (never a
// playlist); rows 1..N route through filteredPlaylistIndices_ (1-based middle
// index) so the mapping survives filtering. Returns nullptr for the Queue row
// and for any out-of-range row.
inline const PlaylistRef* RailPlaylistForRow(
    const std::vector<PlaylistRef>& playlists,
    const std::vector<int>& filteredPlaylistIndices, int rowIndex) {
  if (rowIndex <= 0 ||
      static_cast<size_t>(rowIndex) >= filteredPlaylistIndices.size())
    return nullptr;
  const int middleIndex =
      filteredPlaylistIndices[static_cast<size_t>(rowIndex)];
  if (middleIndex <= 0 ||
      static_cast<size_t>(middleIndex - 1) >= playlists.size())
    return nullptr;
  return &playlists[static_cast<size_t>(middleIndex - 1)];
}

// Cover artwork URL shown by a rail row: "" for the Queue row, playlists
// without art, or rows that map nowhere.
inline std::string RailRowArtworkUrl(
    const std::vector<PlaylistRef>& playlists,
    const std::vector<int>& filteredPlaylistIndices, int rowIndex) {
  const PlaylistRef* playlist =
      RailPlaylistForRow(playlists, filteredPlaylistIndices, rowIndex);
  return playlist ? playlist->cover_url : std::string();
}

// True when the row is the engine's current track: it must be a track row
// (albums/artists never highlight as playing) with a non-empty uri matching
// current_uri. This is the single decision point for the active-row
// highlight AND the row play button (toggle pause/resume instead of
// restarting the context).
inline bool RowMatchesCurrentUri(const ListRow& row,
                                 const std::string& currentUri) {
  return row.kind == ListRowKind::Track && !currentUri.empty() &&
         !row.uri.empty() && row.uri == currentUri;
}

// The main window's message loop runs every key through IsDialogMessageW for
// dialog-style navigation (Tab, arrows). IsDialogMessage consumes VK_RETURN
// even though the main window has no default pushbutton, so Enter typed in
// the search box never reaches the edit's own subclass, which is what routes
// it to the Search button (the same path as clicking it). Messages for
// which this returns true must skip dialog navigation and go straight to
// TranslateMessage/DispatchMessage.
inline bool SearchEnterBypassesDialogNavigation(const MSG& message) {
  return message.message == WM_KEYDOWN && message.wParam == VK_RETURN &&
         message.hwnd != nullptr &&
         EditRoleForControl(static_cast<int>(::GetDlgCtrlID(message.hwnd))) ==
             EditRole::Search;
}

}  // namespace sr
