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
  return row;
}

// The list hover property stores index + 1 so that an absent property (0)
// means "no hover". Returns the zero-based row index or -1.
inline int DecodeHoverIndex(INT_PTR propValue) {
  return static_cast<int>(propValue) - 1;
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

}  // namespace sr
