#include "ui_main.h"

#include <windowsx.h>
#include <commctrl.h>
#include <dwmapi.h>
#include <uxtheme.h>
#include <objidl.h>
#include <gdiplus.h>

#include <algorithm>
#include <cstdint>
#include <functional>
#include <limits>
#include <memory>
#include <unordered_map>
#include <unordered_set>

#include "app.h"
#include "messages.h"
#include "util.h"

#pragma comment(linker,                                                    \
                "\"/manifestdependency:type='win32' name='Microsoft.Windows.Common-Controls' " \
                "version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' " \
                "language='*'\"")

namespace sr {
struct ArtworkCache {
  std::unordered_map<std::string, std::unique_ptr<Gdiplus::Image>> images;
  std::unordered_set<std::string> requested;
};

namespace {
constexpr COLORREF kBg = RGB(0x0B, 0x0C, 0x0D);
constexpr COLORREF kSidebar = RGB(0x10, 0x11, 0x12);
constexpr COLORREF kPanel = RGB(0x15, 0x16, 0x18);
constexpr COLORREF kPlayer = RGB(0x12, 0x13, 0x15);
constexpr COLORREF kControl = RGB(0x20, 0x22, 0x25);
constexpr COLORREF kControlHot = RGB(0x2A, 0x2D, 0x31);
constexpr COLORREF kEdit = RGB(0x1C, 0x1E, 0x21);
constexpr COLORREF kBorder = RGB(0x35, 0x38, 0x3D);
constexpr COLORREF kBorderSoft = RGB(0x27, 0x29, 0x2D);
constexpr COLORREF kText = RGB(0xF2, 0xF2, 0xF0);
constexpr COLORREF kDim = RGB(0xA6, 0xA8, 0xAD);
constexpr COLORREF kDisabled = RGB(0x68, 0x6B, 0x70);
constexpr COLORREF kAccent = RGB(0x4D, 0xC9, 0x73);
constexpr COLORREF kAccentHot = RGB(0x65, 0xD8, 0x87);
constexpr COLORREF kAccentText = RGB(0x08, 0x17, 0x0D);
constexpr COLORREF kSelect = RGB(0x25, 0x28, 0x2C);
constexpr COLORREF kTrack = RGB(0x43, 0x46, 0x4B);
constexpr wchar_t kHotProp[] = L"SRO.ControlHot";

// Antialiased GDI+ replacements for GDI region/pen primitives. GDI+ draws
// with real antialiasing, so rounded fills and 1px borders stay smooth at
// every DPI instead of the chunky GDI RoundRect/region look.
Gdiplus::Color GpColor(COLORREF color) {
  return Gdiplus::Color(GetRValue(color), GetGValue(color), GetBValue(color));
}

// Builds a rounded-rectangle path into an existing GraphicsPath. GDI+ paths
// are not copyable from outside the class (copy ctor is protected), so the
// caller supplies its own path.
void BuildRoundedPath(Gdiplus::GraphicsPath& path, const RECT& rect,
                      int radius) {
  Gdiplus::RectF bounds(static_cast<float>(rect.left),
                        static_cast<float>(rect.top),
                        static_cast<float>(rect.right - rect.left),
                        static_cast<float>(rect.bottom - rect.top));
  if (bounds.Width <= 0 || bounds.Height <= 0) return;
  float diameter = 2.0f * static_cast<float>(radius);
  diameter = std::min(diameter, bounds.Width);
  diameter = std::min(diameter, bounds.Height);
  if (diameter <= 0) {
    path.AddRectangle(bounds);
    return;
  }
  path.AddArc(bounds.X, bounds.Y, diameter, diameter, 180, 90);
  path.AddArc(bounds.X + bounds.Width - diameter, bounds.Y, diameter, diameter,
              270, 90);
  path.AddArc(bounds.X + bounds.Width - diameter,
              bounds.Y + bounds.Height - diameter, diameter, diameter, 0, 90);
  path.AddArc(bounds.X, bounds.Y + bounds.Height - diameter, diameter,
              diameter, 90, 90);
  path.CloseFigure();
}

void FillRoundedRectGp(Gdiplus::Graphics& graphics, const RECT& rect,
                       int radius, COLORREF color) {
  Gdiplus::SolidBrush brush(GpColor(color));
  Gdiplus::GraphicsPath path(Gdiplus::FillModeAlternate);
  BuildRoundedPath(path, rect, radius);
  graphics.FillPath(&brush, &path);
}

void StrokeRoundedRectGp(Gdiplus::Graphics& graphics, const RECT& rect,
                         int radius, COLORREF color, float width) {
  Gdiplus::Pen pen(GpColor(color), width);
  Gdiplus::GraphicsPath path(Gdiplus::FillModeAlternate);
  BuildRoundedPath(path, rect, radius);
  graphics.DrawPath(&pen, &path);
}

void FillEllipseGp(Gdiplus::Graphics& graphics, const RECT& bounds,
                   COLORREF color) {
  Gdiplus::SolidBrush brush(GpColor(color));
  graphics.FillEllipse(&brush, static_cast<float>(bounds.left),
                       static_cast<float>(bounds.top),
                       static_cast<float>(bounds.right - bounds.left),
                       static_cast<float>(bounds.bottom - bounds.top));
}

constexpr wchar_t kListHoverProp[] = L"SRO.ListHover";

// Child control ids. The subclassed edit ids are shared constants so the
// Enter-routing contract stays testable; the asserts pin the sequential enum
// to them.
enum {
  CID_SEARCH_EDIT = kSearchEditControlId,
  CID_SEARCH_BTN,
  CID_BRAND,
  CID_LIBRARY_GROUP,
  CID_PLAYLIST_FILTER = kPlaylistFilterEditControlId,
  CID_PLAYLIST_LIST,
  CID_SETTINGS_BTN,
  CID_RESULTS_LABEL,
  CID_RESULTS_LIST,
  CID_MIDDLE_COMBO,
  CID_MIDDLE_LABEL,
  CID_TRACKS_LIST,
  CID_BACK_BTN,
  CID_NEWPL_BTN,
  CID_RENPL_BTN,
  CID_DELPL_BTN,
  CID_COVER,
  CID_WORKSPACE_COVER,
  CID_WORKSPACE_TYPE,
  CID_WORKSPACE_META,
  CID_WORKSPACE_COLUMNS,
  CID_TITLE,
  CID_ARTIST,
  CID_ALBUM,
  CID_ELAPSED,
  CID_DURATION,
  CID_SEEK,
  CID_PREV_BTN,
  CID_PLAY_BTN,
  CID_NEXT_BTN,
  CID_SHUFFLE_BTN,
  CID_REPEAT_BTN,
  CID_VOLUME_LABEL,
  CID_VOLUME,
  CID_LOCAL_CONTROLS_LABEL,
  CID_ENGINE_GROUP,
  CID_ENGINE_GUIDE,
  CID_ENGINE_STATUS,
  CID_LOGIN_BTN,
  CID_LOGOUT_BTN,
  CID_CACHE_STATUS,
  CID_STATUS,
  CID_SETTINGS_TITLE,
  CID_NOW_PLAYING_LABEL,
};
static_assert(CID_SEARCH_EDIT == kSearchEditControlId);
static_assert(CID_PLAYLIST_FILTER == kPlaylistFilterEditControlId);
struct CoverCtx {
  Gdiplus::Image* img = nullptr;
  MainWindow* owner = nullptr;
};

// Search and playlist-filter edits use native cue banners so the hint is
// rendered by the edit control itself, exactly where typed text and the caret
// go (same margins, same vertical centering). Enter in the main search box
// submits through the search button — the same path as clicking it. The rail
// filter already applies per keystroke, so Enter there falls through to the
// default edit behavior.
LRESULT CALLBACK EditSubclass(HWND control, UINT message, WPARAM wparam,
                              LPARAM lparam, UINT_PTR, DWORD_PTR) {
  if (message == WM_KEYDOWN && wparam == VK_RETURN &&
      EditRoleForControl(::GetDlgCtrlID(control)) == EditRole::Search) {
    HWND parent = ::GetParent(control);
    ::SendMessageW(parent, WM_COMMAND,
                   MAKEWPARAM(CID_SEARCH_BTN, BN_CLICKED), 0);
    return 0;
  }
  if (message == WM_NCDESTROY)
    ::RemoveWindowSubclass(control, EditSubclass, 1);
  return ::DefSubclassProc(control, message, wparam, lparam);
}

void TryDarkTheme(HWND control, const wchar_t* theme) {
  HMODULE ux = ::LoadLibraryW(L"uxtheme.dll");
  if (!ux) return;
  auto setTheme = reinterpret_cast<decltype(&SetWindowTheme)>(
      ::GetProcAddress(ux, "SetWindowTheme"));
  if (setTheme) setTheme(control, theme, nullptr);
}

void EnableDarkTitleBar(HWND h) {
  HMODULE dwm = ::LoadLibraryW(L"dwmapi.dll");
  if (!dwm) return;
  auto fn20 = (decltype(&DwmSetWindowAttribute))::GetProcAddress(dwm, "DwmSetWindowAttribute");
  if (!fn20) return;
  BOOL on = TRUE;
  if (fn20(h, 20, &on, sizeof(on)) != S_OK) fn20(h, 19, &on, sizeof(on));
}


enum class FluentIcon : wchar_t {
  Add = L'\xE710',
  More = L'\xE712',
  Settings = L'\xE713',
  Search = L'\xE721',
  Back = L'\xE72B',
  Edit = L'\xE70F',
  Delete = L'\xE74D',
  Play = L'\xE768',
  Pause = L'\xE769',
  Previous = L'\xE892',
  Next = L'\xE893',
  Shuffle = L'\xE8B1',
  Repeat = L'\xE8EE',
  Playlist = L'\xE8F1',
  Album = L'\xE93C',
  Queue = L'\xEC4F',
};

void DrawFluentIcon(HDC dc, const RECT& bounds, HFONT font, COLORREF color,
                    FluentIcon icon) {
  const wchar_t glyph = static_cast<wchar_t>(icon);
  HGDIOBJ oldFont = ::SelectObject(dc, font);
  const int oldMode = ::SetBkMode(dc, TRANSPARENT);
  const COLORREF oldColor = ::SetTextColor(dc, color);
  RECT rect = bounds;
  ::DrawTextW(dc, &glyph, 1, &rect,
              DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
  ::SetTextColor(dc, oldColor);
  ::SetBkMode(dc, oldMode);
  ::SelectObject(dc, oldFont);
}

// Sets a control's text only when it actually differs, so timer-driven
// updates (4 Hz position projection, engine heartbeats) never invalidate or
// repaint unchanged labels/buttons.
void SetTextIfChanged(HWND control, const wchar_t* text) {
  if (!control) return;
  wchar_t existing[512] = {};
  ::GetWindowTextW(control, existing, 511);
  if (::lstrcmpW(existing, text) == 0) return;
  ::SetWindowTextW(control, text);
}

bool IsIconButton(UINT id) {
  switch (id) {
    case CID_SEARCH_BTN:
    case CID_SETTINGS_BTN:
    case CID_BACK_BTN:
    case CID_NEWPL_BTN:
    case CID_RENPL_BTN:
    case CID_DELPL_BTN:
    case CID_PREV_BTN:
    case CID_PLAY_BTN:
    case CID_NEXT_BTN:
    case CID_SHUFFLE_BTN:
    case CID_REPEAT_BTN:
      return true;
    default:
      return false;
  }
}


LRESULT CALLBACK ButtonSubclass(HWND control, UINT message, WPARAM wparam,
                                LPARAM lparam, UINT_PTR, DWORD_PTR) {
  switch (message) {
    case WM_ERASEBKGND:
      // The owner draw covers the entire item rect (base fill + rounded
      // shape), so the class-gray erase would flash before WM_DRAWITEM and
      // leave stale rectangles. Skip it entirely.
      return 1;
    case WM_MOUSEMOVE:
      if (!::GetPropW(control, kHotProp)) {
        ::SetPropW(control, kHotProp, reinterpret_cast<HANDLE>(1));
        TRACKMOUSEEVENT track{sizeof(track), TME_LEAVE, control, 0};
        ::TrackMouseEvent(&track);
        ::InvalidateRect(control, nullptr, FALSE);
      }
      break;
    case WM_MOUSELEAVE:
      ::RemovePropW(control, kHotProp);
      ::InvalidateRect(control, nullptr, FALSE);
      break;
    case WM_NCDESTROY:
      ::RemovePropW(control, kHotProp);
      ::RemoveWindowSubclass(control, ButtonSubclass, 1);
      break;
  }
  return ::DefSubclassProc(control, message, wparam, lparam);
}

LRESULT CALLBACK ListSubclass(HWND control, UINT message, WPARAM wparam, LPARAM lparam,
                              UINT_PTR, DWORD_PTR) {
  if (message == WM_MOUSEMOVE) {
    LVHITTESTINFO hit{};
    hit.pt = {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
    int index = static_cast<int>(::SendMessageW(control, LVM_HITTEST, 0,
                                                reinterpret_cast<LPARAM>(&hit)));
    int previous = static_cast<int>(
        reinterpret_cast<INT_PTR>(::GetPropW(control, kListHoverProp))) - 1;
    if (index != previous) {
      ::SetPropW(control, kListHoverProp,
                 reinterpret_cast<HANDLE>(static_cast<INT_PTR>(index + 1)));
      if (previous >= 0) {
        RECT row{};
        ListView_GetItemRect(control, previous, &row, LVIR_BOUNDS);
        ::InvalidateRect(control, &row, FALSE);
      }
      if (index >= 0) {
        RECT row{};
        ListView_GetItemRect(control, index, &row, LVIR_BOUNDS);
        ::InvalidateRect(control, &row, FALSE);
      }
    }
    TRACKMOUSEEVENT track{sizeof(track), TME_LEAVE, control, 0};
    ::TrackMouseEvent(&track);
  } else if (message == WM_MOUSELEAVE) {
    ::RemovePropW(control, kListHoverProp);
    ::InvalidateRect(control, nullptr, FALSE);
  } else if (message == WM_NCDESTROY) {
    ::RemovePropW(control, kListHoverProp);
    ::RemoveWindowSubclass(control, ListSubclass, 1);
  }
  return ::DefSubclassProc(control, message, wparam, lparam);
}

struct SliderContext {
  int minimum = 0;
  int maximum = 100;
  int position = 0;
  bool dragging = false;
};

int SliderPositionFromX(HWND control, const SliderContext& context, int x) {
  RECT rect{};
  ::GetClientRect(control, &rect);
  int inset = ::MulDiv(9, ::GetDpiForWindow(control), 96);
  int width = std::max(1, static_cast<int>(rect.right) - 2 * inset);
  int clampedX =
      std::clamp(x, inset, static_cast<int>(rect.right) - inset);
  if (context.maximum <= context.minimum) return context.minimum;
  int64_t value = static_cast<int64_t>(clampedX - inset) *
                      (context.maximum - context.minimum) / width +
                  context.minimum;
  return static_cast<int>(value);
}

void SetSliderPosition(HWND control, SliderContext* context, int position,
                       bool redraw) {
  if (!context) return;
  const int clamped = std::clamp(position, context->minimum, context->maximum);
  if (clamped == context->position) return;  // repaint only on real change
  context->position = clamped;
  if (redraw) ::InvalidateRect(control, nullptr, FALSE);
}

LRESULT CALLBACK SliderProc(HWND control, UINT message, WPARAM wparam, LPARAM lparam) {
  auto* context =
      reinterpret_cast<SliderContext*>(::GetWindowLongPtrW(control, GWLP_USERDATA));
  if (message == WM_NCCREATE) {
    context = new SliderContext();
    ::SetWindowLongPtrW(control, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(context));
  }
  if (!context) return ::DefWindowProcW(control, message, wparam, lparam);

  switch (message) {
    case TBM_SETRANGEMIN:
      if (context->minimum != static_cast<int>(lparam)) {
        context->minimum = static_cast<int>(lparam);
        if (context->maximum < context->minimum)
          context->maximum = context->minimum;
        SetSliderPosition(control, context, context->position, wparam != FALSE);
      }
      return 0;
    case TBM_SETRANGEMAX:
      if (context->maximum != static_cast<int>(lparam)) {
        context->maximum = static_cast<int>(lparam);
        if (context->minimum > context->maximum)
          context->minimum = context->maximum;
        SetSliderPosition(control, context, context->position, wparam != FALSE);
      }
      return 0;
    case TBM_SETPOS:
      SetSliderPosition(control, context, static_cast<int>(lparam), wparam != FALSE);
      return 0;
    case TBM_GETPOS:
      return context->position;
    case WM_GETDLGCODE:
      return DLGC_WANTARROWS;
    case WM_LBUTTONDOWN: {
      if (!::IsWindowEnabled(control)) return 0;
      ::SetFocus(control);
      ::SetCapture(control);
      context->dragging = true;
      SetSliderPosition(control, context,
                        SliderPositionFromX(control, *context, GET_X_LPARAM(lparam)), true);
      ::SendMessageW(::GetParent(control), WM_HSCROLL,
                     MAKEWPARAM(TB_THUMBTRACK, context->position),
                     reinterpret_cast<LPARAM>(control));
      return 0;
    }
    case WM_MOUSEMOVE:
      if (context->dragging && (wparam & MK_LBUTTON)) {
        SetSliderPosition(control, context,
                          SliderPositionFromX(control, *context, GET_X_LPARAM(lparam)), true);
        ::SendMessageW(::GetParent(control), WM_HSCROLL,
                       MAKEWPARAM(TB_THUMBTRACK, context->position),
                       reinterpret_cast<LPARAM>(control));
      }
      return 0;
    case WM_LBUTTONUP:
      if (context->dragging) {
        SetSliderPosition(control, context,
                          SliderPositionFromX(control, *context, GET_X_LPARAM(lparam)), true);
        context->dragging = false;
        ::ReleaseCapture();
        ::SendMessageW(::GetParent(control), WM_HSCROLL,
                       MAKEWPARAM(TB_ENDTRACK, context->position),
                       reinterpret_cast<LPARAM>(control));
      }
      return 0;
    case WM_CAPTURECHANGED:
      context->dragging = false;
      return 0;
    case WM_KEYDOWN: {
      if (!::IsWindowEnabled(control)) return 0;
      int span = std::max(1, context->maximum - context->minimum);
      int step = std::max(1, span / 100);
      int next = context->position;
      if (wparam == VK_LEFT || wparam == VK_DOWN) next -= step;
      else if (wparam == VK_RIGHT || wparam == VK_UP) next += step;
      else if (wparam == VK_PRIOR) next += std::max(1, span / 10);
      else if (wparam == VK_NEXT) next -= std::max(1, span / 10);
      else if (wparam == VK_HOME) next = context->minimum;
      else if (wparam == VK_END) next = context->maximum;
      else return ::DefWindowProcW(control, message, wparam, lparam);
      SetSliderPosition(control, context, next, true);
      ::SendMessageW(::GetParent(control), WM_HSCROLL,
                     MAKEWPARAM(TB_ENDTRACK, context->position),
                     reinterpret_cast<LPARAM>(control));
      return 0;
    }
    case WM_SETFOCUS:
    case WM_KILLFOCUS:
      // Focus changes nothing visually on sliders (no outline by design);
      // fall through so focus state itself is still processed.
      return ::DefWindowProcW(control, message, wparam, lparam);
    case WM_ENABLE:
      ::InvalidateRect(control, nullptr, FALSE);
      return 0;
    case WM_ERASEBKGND:
      return 1;
    case WM_PAINT: {
      PAINTSTRUCT paint{};
      HDC dc = ::BeginPaint(control, &paint);
      RECT rect{};
      ::GetClientRect(control, &rect);
      HBRUSH background = ::CreateSolidBrush(kPlayer);
      ::FillRect(dc, &rect, background);
      ::DeleteObject(background);

      int dpi = static_cast<int>(::GetDpiForWindow(control));
      int inset = ::MulDiv(9, dpi, 96);
      int channelHeight = std::max(3, ::MulDiv(4, dpi, 96));
      int centerY = rect.bottom / 2;
      RECT channel{inset, centerY - channelHeight / 2, rect.right - inset,
                   centerY + (channelHeight + 1) / 2};
      int span = std::max(1, context->maximum - context->minimum);
      int thumbX = channel.left + static_cast<int>(
          static_cast<int64_t>(channel.right - channel.left) *
          (context->position - context->minimum) / span);
      int thumbRadius = std::max(5, ::MulDiv(7, dpi, 96));
      {
        Gdiplus::Graphics graphics(dc);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillRoundedRectGp(graphics, channel, channelHeight, kTrack);
        RECT progress = channel;
        progress.right = std::max(progress.left, static_cast<LONG>(thumbX));
        FillRoundedRectGp(graphics, progress, channelHeight,
                          ::IsWindowEnabled(control) ? kAccent : kDisabled);
        RECT thumb{thumbX - thumbRadius, centerY - thumbRadius,
                   thumbX + thumbRadius + 1, centerY + thumbRadius + 1};
        FillEllipseGp(graphics, thumb,
                      ::IsWindowEnabled(control) ? kText : kDisabled);
      }
      ::EndPaint(control, &paint);
      return 0;
    }
    case WM_NCDESTROY:
      delete context;
      ::SetWindowLongPtrW(control, GWLP_USERDATA, 0);
      return 0;
  }
  return ::DefWindowProcW(control, message, wparam, lparam);
}

struct PromptContext {
  HWND edit = nullptr;
  HWND accept = nullptr;
  HWND cancel = nullptr;
  HBRUSH background = nullptr;
  HBRUSH editBackground = nullptr;
  HFONT font = nullptr;
  UINT dpi = 96;
  bool done = false;
  bool accepted = false;
  std::wstring value;
};

LRESULT CALLBACK PromptProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
  auto* context =
      reinterpret_cast<PromptContext*>(::GetWindowLongPtrW(window, GWLP_USERDATA));
  if (message == WM_NCCREATE) {
    context = reinterpret_cast<PromptContext*>(
        reinterpret_cast<CREATESTRUCTW*>(lparam)->lpCreateParams);
    ::SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(context));
  }
  if (!context) return ::DefWindowProcW(window, message, wparam, lparam);
  switch (message) {
    case WM_ERASEBKGND:
      return 1;
    case WM_PAINT: {
      PAINTSTRUCT paint{};
      HDC dc = ::BeginPaint(window, &paint);
      RECT client{};
      ::GetClientRect(window, &client);
      ::FillRect(dc, &client, context->background);
      RECT label{::MulDiv(22, context->dpi, 96), ::MulDiv(18, context->dpi, 96),
                 client.right - ::MulDiv(22, context->dpi, 96),
                 ::MulDiv(40, context->dpi, 96)};
      ::SelectObject(dc, context->font);
      ::SetBkMode(dc, TRANSPARENT);
      ::SetTextColor(dc, kAccent);
      ::DrawTextW(dc, L"PLAYLIST NAME", -1, &label,
                  DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
      ::EndPaint(window, &paint);
      return 0;
    }
    case WM_CTLCOLOREDIT: {
      HDC dc = reinterpret_cast<HDC>(wparam);
      ::SetTextColor(dc, kText);
      ::SetBkColor(dc, kEdit);
      return reinterpret_cast<LRESULT>(context->editBackground);
    }
    case WM_DRAWITEM: {
      auto* draw = reinterpret_cast<DRAWITEMSTRUCT*>(lparam);
      bool primary = draw->CtlID == IDOK;
      bool pushed = (draw->itemState & ODS_SELECTED) != 0;
      bool hot = ::GetPropW(draw->hwndItem, kHotProp) != nullptr;
      // Clear the full item rect first so the rounded shape never leaves
      // stale corner pixels behind.
      HBRUSH base = ::CreateSolidBrush(kPanel);
      ::FillRect(draw->hDC, &draw->rcItem, base);
      ::DeleteObject(base);
      COLORREF background = primary ? (hot ? kAccentHot : kAccent)
                                    : (hot ? kControlHot : kControl);
      if (pushed) background = primary ? RGB(0x49, 0xB9, 0x69) : kPanel;
      {
        Gdiplus::Graphics graphics(draw->hDC);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillRoundedRectGp(graphics, draw->rcItem, ::MulDiv(9, context->dpi, 96),
                          background);
      }
      wchar_t text[32] = {};
      ::GetWindowTextW(draw->hwndItem, text, 31);
      ::SelectObject(draw->hDC, context->font);
      ::SetBkMode(draw->hDC, TRANSPARENT);
      ::SetTextColor(draw->hDC, primary ? kAccentText : kText);
      ::DrawTextW(draw->hDC, text, -1, &draw->rcItem,
                  DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
      return TRUE;
    }
    case WM_COMMAND:
      if (LOWORD(wparam) == IDOK || LOWORD(wparam) == IDCANCEL) {
        if (LOWORD(wparam) == IDOK) {
          int length = ::GetWindowTextLengthW(context->edit);
          context->value.resize(static_cast<size_t>(length) + 1);
          ::GetWindowTextW(context->edit, context->value.data(), length + 1);
          context->value.resize(static_cast<size_t>(length));
        }
        context->accepted = LOWORD(wparam) == IDOK;
        context->done = true;
        ::DestroyWindow(window);
        return 0;
      }
      break;
    case WM_CLOSE:
      context->done = true;
      ::DestroyWindow(window);
      return 0;
  }
  return ::DefWindowProcW(window, message, wparam, lparam);
}

}  // namespace

std::optional<std::wstring> MainWindow::PromptText(HWND owner, const std::wstring& title,
                                                   const std::wstring& initial) {
  const wchar_t* className = L"SROPromptWindow";
  WNDCLASSEXW windowClass{};
  windowClass.cbSize = sizeof(windowClass);
  windowClass.lpfnWndProc = PromptProc;
  windowClass.hInstance = hinst_;
  windowClass.hCursor = ::LoadCursorW(nullptr, IDC_ARROW);
  windowClass.hbrBackground = reinterpret_cast<HBRUSH>(::GetStockObject(BLACK_BRUSH));
  windowClass.lpszClassName = className;
  ::RegisterClassExW(&windowClass);

  PromptContext context;
  context.dpi = dpi_;
  context.background = ::CreateSolidBrush(kPanel);
  context.editBackground = ::CreateSolidBrush(kEdit);
  context.font = ::CreateFontW(
      -::MulDiv(10, context.dpi, 72), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
      DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
      DEFAULT_PITCH | FF_DONTCARE, L"Segoe UI Variable Text");
  auto scale = [&context](int value) { return ::MulDiv(value, context.dpi, 96); };
  RECT ownerRect{};
  ::GetWindowRect(owner, &ownerRect);
  int width = scale(460);
  int height = scale(188);
  int x = ownerRect.left + (ownerRect.right - ownerRect.left - width) / 2;
  int y = ownerRect.top + (ownerRect.bottom - ownerRect.top - height) / 2;
  HWND dialog = ::CreateWindowExW(
      WS_EX_DLGMODALFRAME, className, title.c_str(),
      WS_CAPTION | WS_SYSMENU | WS_POPUP, x, y, width, height, owner, nullptr, hinst_,
      &context);
  if (!dialog) {
    ::DeleteObject(context.font);
    ::DeleteObject(context.background);
    ::DeleteObject(context.editBackground);
    return std::nullopt;
  }
  EnableDarkTitleBar(dialog);

  context.edit = ::CreateWindowExW(
      0, L"EDIT", initial.c_str(),
      WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
      scale(22), scale(46), width - scale(60), scale(40), dialog, nullptr, hinst_,
      nullptr);
  ::SendMessageW(context.edit, EM_SETMARGINS, EC_LEFTMARGIN | EC_RIGHTMARGIN,
                 MAKELPARAM(scale(10), scale(10)));
  context.accept = ::CreateWindowExW(
      0, L"BUTTON", L"SAVE",
      WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW | BS_DEFPUSHBUTTON,
      width - scale(214), scale(104), scale(88), scale(38), dialog,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(IDOK)), hinst_, nullptr);
  context.cancel = ::CreateWindowExW(
      0, L"BUTTON", L"CANCEL",
      WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW,
      width - scale(116), scale(104), scale(88), scale(38), dialog,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(IDCANCEL)), hinst_, nullptr);
  ::SetWindowSubclass(context.accept, ButtonSubclass, 1, 0);
  ::SetWindowSubclass(context.cancel, ButtonSubclass, 1, 0);
  ::SendMessageW(dialog, DM_SETDEFID, IDOK, 0);
  ::SendMessageW(context.edit, WM_SETFONT, reinterpret_cast<WPARAM>(context.font), TRUE);
  ::SendMessageW(context.accept, WM_SETFONT, reinterpret_cast<WPARAM>(context.font), TRUE);
  ::SendMessageW(context.cancel, WM_SETFONT, reinterpret_cast<WPARAM>(context.font), TRUE);
  ::SendMessageW(context.edit, EM_SETSEL, 0, -1);
  ::EnableWindow(owner, FALSE);
  ::ShowWindow(dialog, SW_SHOW);
  ::SetFocus(context.edit);

  MSG message{};
  while (!context.done && ::GetMessageW(&message, nullptr, 0, 0) > 0) {
    if (!::IsDialogMessageW(dialog, &message)) {
      ::TranslateMessage(&message);
      ::DispatchMessageW(&message);
    }
  }
  ::EnableWindow(owner, TRUE);
  ::SetForegroundWindow(owner);
  ::DeleteObject(context.font);
  ::DeleteObject(context.background);
  ::DeleteObject(context.editBackground);
  if (!context.accepted) return std::nullopt;
  return context.value;
}

bool MainWindow::Create(HINSTANCE hinst, Application* app) {
  app_ = app;
  hinst_ = hinst;
  dpi_ = ::GetDpiForSystem();
  if (dpi_ == 0) dpi_ = 96;

  WNDCLASSEXW wc{};
  wc.cbSize = sizeof(wc);
  wc.lpfnWndProc = &MainWindow::WndProc;
  wc.hInstance = hinst_;
  wc.hCursor = ::LoadCursorW(nullptr, IDC_ARROW);
  wc.hIcon = ::LoadIconW(nullptr, IDI_APPLICATION);
  wc.lpszClassName = L"SROMainWnd";
  wc.hbrBackground = ::CreateSolidBrush(kBg);
  ::RegisterClassExW(&wc);

  WNDCLASSEXW cc{};
  cc.cbSize = sizeof(cc);
  cc.lpfnWndProc = &MainWindow::CoverProc;
  cc.hInstance = hinst_;
  cc.lpszClassName = L"SROCoverArea";
  cc.hbrBackground = ::CreateSolidBrush(kPanel);
  ::RegisterClassExW(&cc);

  WNDCLASSEXW sliderClass{};
  sliderClass.cbSize = sizeof(sliderClass);
  sliderClass.lpfnWndProc = SliderProc;
  sliderClass.hInstance = hinst_;
  sliderClass.hCursor = ::LoadCursorW(nullptr, IDC_HAND);
  sliderClass.lpszClassName = L"SROSlider";
  sliderClass.hbrBackground = nullptr;
  ::RegisterClassExW(&sliderClass);

  int w = ::MulDiv(1240, dpi_, 96), h = ::MulDiv(780, dpi_, 96);
  hwnd_ = ::CreateWindowExW(0, L"SROMainWnd", L"Spotify Renderer",
                            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT,
                            w, h, nullptr, nullptr, hinst_, this);
  if (!hwnd_) return false;
  return true;
}

void MainWindow::Destroy() {
  if (hwnd_) ::DestroyWindow(hwnd_);
  hwnd_ = nullptr;
}

void MainWindow::Show(bool show) {
  if (hwnd_) ::ShowWindow(hwnd_, show ? SW_SHOW : SW_HIDE);
}

bool MainWindow::Visible() const {
  return hwnd_ && ::IsWindowVisible(hwnd_);
}



void MainWindow::ApplyFonts() {
  auto makeFont = [this](int points, int weight, const wchar_t* face) {
    return ::CreateFontW(
        -::MulDiv(points, dpi_, 72), 0, 0, 0, weight, FALSE, FALSE, FALSE,
        DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE, face);
  };
  auto makeIconFont = [this](int pixels) {
    return ::CreateFontW(
        -::MulDiv(pixels, dpi_, 96), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
        DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        CLEARTYPE_NATURAL_QUALITY, DEFAULT_PITCH | FF_DONTCARE,
        L"Segoe Fluent Icons");
  };
  auto setFont = [](HWND control, HFONT font) {
    if (control && font)
      ::SendMessageW(control, WM_SETFONT, reinterpret_cast<WPARAM>(font), TRUE);
  };
  if (fontUi_) ::DeleteObject(fontUi_);
  if (fontList_) ::DeleteObject(fontList_);
  if (fontRowTitle_) ::DeleteObject(fontRowTitle_);
  if (fontTitle_) ::DeleteObject(fontTitle_);
  if (fontDisplay_) ::DeleteObject(fontDisplay_);
  if (fontSmall_) ::DeleteObject(fontSmall_);
  if (fontIcon16_) ::DeleteObject(fontIcon16_);
  if (fontIcon20_) ::DeleteObject(fontIcon20_);
  if (fontIcon24_) ::DeleteObject(fontIcon24_);
  if (fontIcon40_) ::DeleteObject(fontIcon40_);
  fontUi_ = makeFont(10, FW_NORMAL, L"Segoe UI Variable Text");
  fontList_ = makeFont(9, FW_NORMAL, L"Segoe UI Variable Text");
  fontRowTitle_ = makeFont(10, FW_SEMIBOLD, L"Segoe UI Variable Display");
  fontTitle_ = makeFont(18, FW_SEMIBOLD, L"Segoe UI Variable Display");
  fontDisplay_ = makeFont(24, FW_BOLD, L"Bahnschrift SemiBold");
  fontSmall_ = makeFont(8, FW_SEMIBOLD, L"Segoe UI Variable Text");
  fontIcon16_ = makeIconFont(16);
  fontIcon20_ = makeIconFont(20);
  fontIcon24_ = makeIconFont(24);
  fontIcon40_ = makeIconFont(40);

  HWND controls[] = {
      brandLbl_,           libraryGroupLbl_,     playlistFilterEdit_,
      playlistList_,       newPlBtn_,            settingsBtn_,
      searchEdit_,         searchBtn_,           resultsList_,
      resultsLabel_,       middleCombo_,         tracksList_,
      backBtn_,            renPlBtn_,            delPlBtn_,
      middleLabel_,        workspaceTypeLbl_,    workspaceMetaLbl_,
      workspaceColumnsLbl_, workspaceTimeColumnLbl_,
      workspaceActionColumnLbl_, nowPlayingLbl_, artistLbl_,
      albumLbl_,           elapsedLbl_,          durationLbl_,
      prevBtn_,            playBtn_,             nextBtn_,
      shuffleBtn_,         repeatBtn_,           volumeLbl_,
      localControlsLbl_,   engineGroupLbl_,      engineGuideLbl_,
      engineStatusLbl_,    loginBtn_,            logoutBtn_,
      cacheStatusLbl_,     statusLbl_,
      settingsTitle_,      settingsGuide_};
  for (HWND control : controls) setFont(control, fontUi_);
  setFont(resultsList_, fontList_);
  setFont(tracksList_, fontList_);
  setFont(playlistList_, fontList_);
  setFont(titleLbl_, fontRowTitle_);
  setFont(middleLabel_, fontDisplay_);
  setFont(settingsTitle_, fontTitle_);
  setFont(brandLbl_, fontTitle_);
  setFont(libraryGroupLbl_, fontSmall_);
  setFont(resultsLabel_, fontSmall_);
  setFont(workspaceTypeLbl_, fontSmall_);
  setFont(workspaceColumnsLbl_, fontSmall_);
  setFont(workspaceTimeColumnLbl_, fontSmall_);
  setFont(workspaceActionColumnLbl_, fontIcon16_);
  setFont(nowPlayingLbl_, fontSmall_);
  setFont(volumeLbl_, fontSmall_);
  setFont(engineGroupLbl_, fontSmall_);
  setFont(statusLbl_, fontSmall_);
}

void MainWindow::SetDarkTheme() {
  TryDarkTheme(middleCombo_, L"DarkMode_CFD");
  TryDarkTheme(resultsList_, L"DarkMode_Explorer");
  TryDarkTheme(tracksList_, L"DarkMode_Explorer");
  TryDarkTheme(playlistList_, L"DarkMode_Explorer");
  TryDarkTheme(searchEdit_, L"DarkMode_CFD");
  TryDarkTheme(playlistFilterEdit_, L"DarkMode_CFD");
  EnableDarkTitleBar(hwnd_);
}
void MainWindow::AddTooltip(HWND control, const wchar_t* text) {
  if (!tooltip_ || !control || !text) return;
  TOOLINFOW info{};
  info.cbSize = sizeof(info);
  info.uFlags = TTF_IDISHWND | TTF_SUBCLASS;

  info.hwnd = hwnd_;
  info.uId = reinterpret_cast<UINT_PTR>(control);
  ::SendMessageW(tooltip_, TTM_ADDTOOLW, 0,
                 reinterpret_cast<LPARAM>(&info));
}

void MainWindow::SetTooltipText(HWND control, const std::wstring& text) {
  if (!tooltip_ || !control) return;
  TOOLINFOW info{};
  info.cbSize = sizeof(info);
  info.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
  info.hwnd = hwnd_;
  info.uId = reinterpret_cast<UINT_PTR>(control);
  info.lpszText = const_cast<LPWSTR>(text.c_str());
  ::SendMessageW(tooltip_, TTM_UPDATETIPTEXTW, 0,
                 reinterpret_cast<LPARAM>(&info));
}

void MainWindow::SetControlGroupVisible(const std::vector<HWND>& controls,
                                        bool visible) {
  for (HWND control : controls)
    if (control) ::ShowWindow(control, visible ? SW_SHOW : SW_HIDE);
}

void MainWindow::RebuildPlaylistRail() {
  if (!playlistList_) return;
  wchar_t filterBuffer[256] = {};
  ::GetWindowTextW(playlistFilterEdit_, filterBuffer, 255);
  std::wstring filter = filterBuffer;
  ::CharLowerBuffW(filter.data(), static_cast<DWORD>(filter.size()));

  ::SendMessageW(playlistList_, WM_SETREDRAW, FALSE, 0);
  ::SendMessageW(playlistList_, LB_RESETCONTENT, 0, 0);
  filteredPlaylistIndices_.clear();
  filteredPlaylistIndices_.push_back(0);
  ::SendMessageW(playlistList_, LB_ADDSTRING, 0,
                 reinterpret_cast<LPARAM>(L"Queue"));
  for (size_t i = 0; i < playlists_.size(); ++i) {
    std::wstring searchable = Utf8ToWide(playlists_[i].name + " " +
                                         playlists_[i].owner);
    ::CharLowerBuffW(searchable.data(),
                     static_cast<DWORD>(searchable.size()));
    if (!filter.empty() && searchable.find(filter) == std::wstring::npos)
      continue;
    filteredPlaylistIndices_.push_back(static_cast<int>(i) + 1);
    ::SendMessageW(
        playlistList_, LB_ADDSTRING, 0,
        reinterpret_cast<LPARAM>(Utf8ToWide(playlists_[i].name).c_str()));
  }
  int selectedRow = -1;
  for (size_t i = 0; i < filteredPlaylistIndices_.size(); ++i)
    if (filteredPlaylistIndices_[i] == selectedMiddleIndex_)
      selectedRow = static_cast<int>(i);
  ::SendMessageW(playlistList_, LB_SETCURSEL, selectedRow, 0);
  ::SendMessageW(playlistList_, WM_SETREDRAW, TRUE, 0);
  ::InvalidateRect(playlistList_, nullptr, TRUE);
  // Fetch covers for the visible playlists through the existing artwork
  // machinery; rows fall back to seeded tiles until art arrives. Dedup via
  // the same requested-set as the track lists.
  if (app_ && artworkCache_) {
    if (artworkCache_->requested.size() >= 512)
      artworkCache_->requested.clear();
    size_t issued = 0;
    for (size_t row = 1; row < filteredPlaylistIndices_.size() && issued < 12;
         ++row) {
      const PlaylistRef* playlist = RailPlaylistForRow(
          playlists_, filteredPlaylistIndices_, static_cast<int>(row));
      if (!playlist || playlist->cover_url.empty()) continue;
      const std::string& url = playlist->cover_url;
      if (artworkCache_->images.find(url) != artworkCache_->images.end())
        continue;
      if (artworkCache_->requested.insert(url).second) {
        app_->OnTrackArtworkNeeded(url);
        ++issued;
      }
    }
  }
}

void MainWindow::SelectPlaylistRow(int comboIndex, bool activate) {
  comboIndex = std::clamp(comboIndex, 0, static_cast<int>(playlists_.size()));
  selectedMiddleIndex_ = comboIndex;
  ::SendMessageW(middleCombo_, CB_SETCURSEL, comboIndex, 0);
  for (size_t i = 0; i < filteredPlaylistIndices_.size(); ++i)
    if (filteredPlaylistIndices_[i] == comboIndex)
      ::SendMessageW(playlistList_, LB_SETCURSEL, static_cast<WPARAM>(i), 0);

  collectionKind_ =
      comboIndex == 0 ? CollectionKind::Queue : CollectionKind::Playlist;
  workspaceTitle_ =
      comboIndex == 0 ? L"Queue" : Utf8ToWide(playlists_[comboIndex - 1].name);
  UpdateWorkspaceArtwork(
      comboIndex > 0 ? playlists_[comboIndex - 1].cover_url : std::string{});
  if (activate && !demoMode_) {
    middleTracks_.clear();
    workspaceDurationMs_ = 0;
  }
  ShowWorkspace(WorkspaceKind::Collection);
  UpdateWorkspaceHeader();
  if (activate && demoMode_) {
    SetMiddleTracks(comboIndex == 0 ? std::vector<TrackRef>{} : demoTracks_);
    return;
  }
  if (activate && app_) {
    middleRows_.clear();
    middleLoading_ = true;
    SetListMessage(tracksList_, L"Loading collection",
                   L"Spotify is fetching tracks for this view.");
    app_->OnMiddleCombo(comboIndex);
  }
}

void MainWindow::ShowWorkspace(WorkspaceKind kind) {
  if (kind != workspaceKind_ && workspaceKind_ != WorkspaceKind::Settings)
    previousWorkspaceKind_ = workspaceKind_;
  workspaceKind_ = kind;
  if (kind == WorkspaceKind::Settings && app_) app_->OnSettingsShown();
  Layout();
}

void MainWindow::UpdateWorkspaceHeader() {
  if (!middleLabel_) return;
  std::wstring type = L"QUEUE";
  std::wstring meta;
  const size_t itemCount = middleTracks_.size();
  if (collectionKind_ == CollectionKind::Playlist) {
    type = L"PLAYLIST";
    if (selectedMiddleIndex_ > 0 &&
        static_cast<size_t>(selectedMiddleIndex_ - 1) < playlists_.size()) {
      const PlaylistRef& playlist = playlists_[selectedMiddleIndex_ - 1];
      if (!playlist.owner.empty()) meta = Utf8ToWide(playlist.owner) + L"  ·  ";
      size_t count = playlist.tracks_total > 0
                         ? static_cast<size_t>(playlist.tracks_total)
                         : itemCount;
      meta += std::to_wstring(count) + (count == 1 ? L" track" : L" tracks");
    }
  } else if (collectionKind_ == CollectionKind::Album) {
    type = L"ALBUM";
    if (!middleTracks_.empty()) {
      std::wstring artists = JoinArtists(middleTracks_.front().artist_names);
      if (!artists.empty()) meta = artists + L"  ·  ";
    }
    meta += std::to_wstring(itemCount) +
            (itemCount == 1 ? L" track" : L" tracks");
  } else if (collectionKind_ == CollectionKind::Artist) {
    type = L"ARTIST";
    meta = std::to_wstring(itemCount) +
           (itemCount == 1 ? L" top track" : L" top tracks");
  } else {
    meta = std::to_wstring(itemCount) +
           (itemCount == 1 ? L" track" : L" tracks");
  }
  if (workspaceDurationMs_ > 0) {
    int64_t totalMinutes = workspaceDurationMs_ / 60000;
    int64_t hours = totalMinutes / 60;
    int64_t minutes = totalMinutes % 60;
    meta += L"  ·  ";
    if (hours > 0)
      meta += std::to_wstring(hours) + L" hr " + std::to_wstring(minutes) +
              L" min";
    else
      meta += std::to_wstring(minutes) + L" min";
  }
  ::SetWindowTextW(workspaceTypeLbl_, type.c_str());
  ::SetWindowTextW(middleLabel_, workspaceTitle_.c_str());
  ::SetWindowTextW(workspaceMetaLbl_, meta.c_str());
  const BOOL playlist = collectionKind_ == CollectionKind::Playlist;
  ::ShowWindow(renPlBtn_,
               workspaceKind_ == WorkspaceKind::Collection && playlist
                   ? SW_SHOW
                   : SW_HIDE);
  ::ShowWindow(delPlBtn_,
               workspaceKind_ == WorkspaceKind::Collection && playlist
                   ? SW_SHOW
                   : SW_HIDE);
}

void MainWindow::UpdateWorkspaceArtwork(const std::string& url) {
  workspaceArtworkUrl_ = url;
  if (!workspaceCover_) return;
  auto* context = reinterpret_cast<CoverCtx*>(
      ::GetWindowLongPtrW(workspaceCover_, GWLP_USERDATA));
  if (context) {
    delete context->img;
    context->img = nullptr;
    if (artworkCache_ && !url.empty()) {
      auto found = artworkCache_->images.find(url);
      if (found != artworkCache_->images.end())
        context->img = found->second->Clone();
    }
  }
  ::InvalidateRect(workspaceCover_, nullptr, TRUE);
  if (app_ && !url.empty() && (!context || !context->img))
    app_->OnTrackArtworkNeeded(url);
}

void MainWindow::CreateChildren() {
  artworkCache_ = new ArtworkCache();
  brushBg_ = ::CreateSolidBrush(kBg);
  brushSidebar_ = ::CreateSolidBrush(kSidebar);
  brushPanel_ = ::CreateSolidBrush(kPanel);
  brushEdit_ = ::CreateSolidBrush(kEdit);
  brushControl_ = ::CreateSolidBrush(kControl);
  brushPlayer_ = ::CreateSolidBrush(kPlayer);

  auto makeButton = [this](const wchar_t* text, int id) {
    HWND button = ::CreateWindowExW(
        0, L"BUTTON", text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS | BS_OWNERDRAW,
        0, 0, 10, 10, hwnd_,
        reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hinst_,
        nullptr);
    ::SetWindowSubclass(button, ButtonSubclass, 1, 0);
    return button;
  };
  auto makeStatic = [this](const wchar_t* text, int id,
                           DWORD style = SS_LEFT) {
    return ::CreateWindowExW(
        0, L"STATIC", text,
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | SS_NOPREFIX | style, 0, 0,
        10, 10, hwnd_,
        reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hinst_, nullptr);
  };
  auto makeList = [this](int id) {
    HWND list = ::CreateWindowExW(
        0, WC_LISTVIEWW, L"",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS | LVS_REPORT |
            LVS_NOCOLUMNHEADER | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
        0, 0, 10, 10, hwnd_,
        reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hinst_, nullptr);
    ::SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0,
                   LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER |
                       LVS_EX_LABELTIP);
    ::SendMessageW(list, LVM_SETBKCOLOR, 0, static_cast<LPARAM>(kPanel));
    ::SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, static_cast<LPARAM>(kPanel));
    ::SendMessageW(list, LVM_SETTEXTCOLOR, 0, static_cast<LPARAM>(kDim));
    LVCOLUMNW column{};
    column.mask = LVCF_TEXT;
    column.pszText = const_cast<LPWSTR>(L"");
    ::SendMessageW(list, LVM_INSERTCOLUMNW, 0,
                   reinterpret_cast<LPARAM>(&column));
    ::SetWindowSubclass(list, ListSubclass, 1, 0);
    return list;
  };
  auto makeEdit = [this](int id) {
    HWND edit = ::CreateWindowExW(
        0, L"EDIT", L"",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS | WS_BORDER |
            ES_AUTOHSCROLL,
        0, 0, 10, 10, hwnd_,
        reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), hinst_, nullptr);
    // Interior margins must scale with DPI so typed text lines up with the
    // cue banner (both are rendered by the edit control itself).
    int margin = ::MulDiv(12, dpi_, 96);
    ::SendMessageW(edit, EM_SETMARGINS, EC_LEFTMARGIN | EC_RIGHTMARGIN,
                   MAKELPARAM(margin, margin));
    if (EditRoleForControl(id) == EditRole::Search)
      ::SendMessageW(edit, EM_SETCUEBANNER, TRUE,
                     reinterpret_cast<LPARAM>(L"Search tracks, artists, albums"));
    else if (EditRoleForControl(id) == EditRole::Filter)
      ::SendMessageW(edit, EM_SETCUEBANNER, TRUE,
                     reinterpret_cast<LPARAM>(L"Filter playlists"));
    ::SetWindowSubclass(edit, EditSubclass, 1, 0);
    return edit;
  };

  brandLbl_ = makeStatic(L"SpotifyRenderer", CID_BRAND);
  libraryGroupLbl_ = makeStatic(L"YOUR LIBRARY", CID_LIBRARY_GROUP);
  playlistFilterEdit_ = makeEdit(CID_PLAYLIST_FILTER);
  playlistList_ = ::CreateWindowExW(
      0, L"LISTBOX", L"",
      WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS | WS_VSCROLL |
          LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS |
          LBS_NOINTEGRALHEIGHT,
      0, 0, 10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_PLAYLIST_LIST)), hinst_,
      nullptr);
  newPlBtn_ = makeButton(L"Create playlist", CID_NEWPL_BTN);
  settingsBtn_ = makeButton(L"Settings", CID_SETTINGS_BTN);

  searchEdit_ = makeEdit(CID_SEARCH_EDIT);
  searchBtn_ = makeButton(L"Search", CID_SEARCH_BTN);
  resultsLabel_ = makeStatic(L"SEARCH RESULTS", CID_RESULTS_LABEL);
  resultsList_ = makeList(CID_RESULTS_LIST);

  middleCombo_ = ::CreateWindowExW(
      0, WC_COMBOBOXW, L"",
      WS_CHILD | WS_CLIPSIBLINGS | CBS_DROPDOWNLIST | CBS_HASSTRINGS |
          CBS_OWNERDRAWFIXED,
      0, 0, 10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_MIDDLE_COMBO)), hinst_,
      nullptr);
  ::SendMessageW(middleCombo_, CB_SETITEMHEIGHT, static_cast<WPARAM>(-1), 32);
  ::SendMessageW(middleCombo_, CB_SETITEMHEIGHT, 0, 30);

  workspaceCover_ = ::CreateWindowExW(
      0, L"SROCoverArea", L"", WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS, 0, 0,
      10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_WORKSPACE_COVER)),
      hinst_, nullptr);
  ::SetWindowLongPtrW(
      workspaceCover_, GWLP_USERDATA,
      reinterpret_cast<LONG_PTR>(new CoverCtx{nullptr, this}));
  workspaceTypeLbl_ = makeStatic(L"QUEUE", CID_WORKSPACE_TYPE);
  middleLabel_ =
      makeStatic(L"Queue", CID_MIDDLE_LABEL, SS_LEFT | SS_ENDELLIPSIS);
  workspaceMetaLbl_ =
      makeStatic(L"0 tracks", CID_WORKSPACE_META, SS_LEFT | SS_ENDELLIPSIS);
  workspaceColumnsLbl_ = makeStatic(
      L"TITLE / ARTIST / ALBUM", CID_WORKSPACE_COLUMNS,
      SS_LEFT | SS_ENDELLIPSIS);
  workspaceTimeColumnLbl_ = makeStatic(L"TIME", 0, SS_RIGHT);
  workspaceActionColumnLbl_ = makeStatic(L"\xE712", 0, SS_CENTER);
  backBtn_ = makeButton(L"Back", CID_BACK_BTN);
  renPlBtn_ = makeButton(L"Rename playlist", CID_RENPL_BTN);
  delPlBtn_ = makeButton(L"Delete playlist", CID_DELPL_BTN);
  tracksList_ = makeList(CID_TRACKS_LIST);

  coverArea_ = ::CreateWindowExW(
      0, L"SROCoverArea", L"", WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS, 0, 0,
      10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_COVER)), hinst_, nullptr);
  ::SetWindowLongPtrW(
      coverArea_, GWLP_USERDATA,
      reinterpret_cast<LONG_PTR>(new CoverCtx{nullptr, this}));
  nowPlayingLbl_ = makeStatic(L"NOW PLAYING", CID_NOW_PLAYING_LABEL);
  titleLbl_ =
      makeStatic(L"Nothing playing", CID_TITLE, SS_LEFT | SS_ENDELLIPSIS);
  artistLbl_ = makeStatic(L"Choose a track to begin", CID_ARTIST,
                          SS_LEFT | SS_ENDELLIPSIS);
  albumLbl_ = makeStatic(L"", CID_ALBUM, SS_LEFT | SS_ENDELLIPSIS);
  elapsedLbl_ = makeStatic(L"0:00", CID_ELAPSED);
  durationLbl_ = makeStatic(L"0:00", CID_DURATION, SS_RIGHT);
  seekBar_ = ::CreateWindowExW(
      0, L"SROSlider", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS,
      0, 0, 10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_SEEK)), hinst_, nullptr);
  ::SendMessageW(seekBar_, TBM_SETRANGEMIN, FALSE, 0);
  ::SendMessageW(seekBar_, TBM_SETRANGEMAX, TRUE, 1000);
  shuffleBtn_ = makeButton(L"Shuffle off", CID_SHUFFLE_BTN);
  prevBtn_ = makeButton(L"Previous", CID_PREV_BTN);
  playBtn_ = makeButton(L"Play", CID_PLAY_BTN);
  nextBtn_ = makeButton(L"Next", CID_NEXT_BTN);
  repeatBtn_ = makeButton(L"Repeat off", CID_REPEAT_BTN);
  localControlsLbl_ = makeStatic(L"", CID_LOCAL_CONTROLS_LABEL);
  volumeLbl_ = makeStatic(L"", CID_VOLUME_LABEL, SS_RIGHT | SS_ENDELLIPSIS);
  volumeBar_ = ::CreateWindowExW(
      0, L"SROSlider", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_CLIPSIBLINGS,
      0, 0, 10, 10, hwnd_,
      reinterpret_cast<HMENU>(static_cast<INT_PTR>(CID_VOLUME)), hinst_,
      nullptr);
  ::SendMessageW(volumeBar_, TBM_SETRANGEMIN, FALSE, 0);
  ::SendMessageW(volumeBar_, TBM_SETRANGEMAX, TRUE, 100);

  engineGroupLbl_ = makeStatic(L"SPOTIFY SESSION", CID_ENGINE_GROUP);
  engineGuideLbl_ = makeStatic(
      L"SpotifyPlaybackEngine signs in once with your Spotify account. That "
      L"same local session powers audio and browsing — search, playlists, "
      L"albums, and artists — no developer app is involved.",
      CID_ENGINE_GUIDE);
  engineStatusLbl_ = makeStatic(
      L"Standalone engine: starting", CID_ENGINE_STATUS,
      SS_LEFT | SS_ENDELLIPSIS);
  loginBtn_ = makeButton(L"Log in", CID_LOGIN_BTN);
  logoutBtn_ = makeButton(L"Log out", CID_LOGOUT_BTN);
  cacheStatusLbl_ = makeStatic(
      L"Audio: Ogg Vorbis 320 kbps  ·  WASAPI  ·  cache limit 1 GiB",
      CID_CACHE_STATUS, SS_LEFT | SS_ENDELLIPSIS);
  statusLbl_ = makeStatic(L"", CID_STATUS, SS_LEFT | SS_ENDELLIPSIS);

  settingsTitle_ = makeStatic(L"Settings", CID_SETTINGS_TITLE);
  settingsGuide_ = makeStatic(
      L"One Spotify sign-in covers local playback and browsing through the "
      L"engine's session. No developer app is required.",
      0);

  tooltip_ = ::CreateWindowExW(
      WS_EX_TOPMOST, TOOLTIPS_CLASSW, nullptr,
      WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX, CW_USEDEFAULT, CW_USEDEFAULT,
      CW_USEDEFAULT, CW_USEDEFAULT, hwnd_, nullptr, hinst_, nullptr);
  AddTooltip(searchBtn_, L"Search Spotify");
  AddTooltip(settingsBtn_, L"Open settings");
  AddTooltip(backBtn_, L"Back");
  AddTooltip(newPlBtn_, L"Create playlist");
  AddTooltip(renPlBtn_, L"Rename selected playlist");
  AddTooltip(delPlBtn_, L"Delete selected playlist");
  AddTooltip(shuffleBtn_, L"Toggle shuffle");
  AddTooltip(prevBtn_, L"Previous track");
  AddTooltip(playBtn_, L"Play or pause");
  AddTooltip(nextBtn_, L"Next track");
  AddTooltip(repeatBtn_, L"Change repeat mode");
  AddTooltip(seekBar_, L"Seek");
  AddTooltip(volumeBar_, L"Playback volume");
  AddTooltip(loginBtn_, L"Open Spotify sign-in in your browser");
  AddTooltip(logoutBtn_, L"Clear the saved sign-in and end the session");

  int rowHeight = ::MulDiv(64, dpi_, 96);
  rowHeightImageList_ = ::ImageList_Create(1, rowHeight, ILC_COLOR32, 1, 1);
  ::SendMessageW(resultsList_, LVM_SETIMAGELIST, LVSIL_SMALL,
                 reinterpret_cast<LPARAM>(rowHeightImageList_));
  ::SendMessageW(tracksList_, LVM_SETIMAGELIST, LVSIL_SMALL,
                 reinterpret_cast<LPARAM>(rowHeightImageList_));
  SetListMessage(resultsList_, resultsEmptyTitle_, resultsEmptyDetail_);
  SetListMessage(tracksList_, middleEmptyTitle_, middleEmptyDetail_);
  RebuildPlaylistRail();
  ApplyFonts();
  SetDarkTheme();
  SetPlayback({});
  UpdateWorkspaceHeader();
}

void MainWindow::Layout() {
  if (!hwnd_) return;
  RECT client{};
  ::GetClientRect(hwnd_, &client);
  const int width = client.right;
  const int height = client.bottom;
  auto scale = [this](int value) { return ::MulDiv(value, dpi_, 96); };
  auto move = [](HWND control, int x, int y, int w, int h) {
    if (control)
      ::MoveWindow(control, x, y, std::max(0, w), std::max(0, h), TRUE);
  };

  const int sidebarWidth = scale(238);
  const int playerHeight = scale(98);
  const int playerY = height - playerHeight;
  const int railPad = scale(16);
  const int workspaceX = sidebarWidth + scale(12);
  const int workspaceRight = width - scale(12);
  const int workspaceWidth = workspaceRight - workspaceX;
  const int searchWidth = std::min(scale(430), std::max(scale(280),
                                                        workspaceWidth / 2));
  const int searchX = workspaceX + (workspaceWidth - searchWidth) / 2;

  move(brandLbl_, railPad, scale(15), sidebarWidth - 2 * railPad, scale(30));
  move(libraryGroupLbl_, railPad, scale(60), sidebarWidth - 2 * railPad,
       scale(18));
  move(newPlBtn_, sidebarWidth - railPad - scale(32), scale(52), scale(32),
       scale(32));
  move(playlistFilterEdit_, railPad, scale(86),
       sidebarWidth - 2 * railPad, scale(36));
  move(settingsBtn_, sidebarWidth - railPad - scale(38),
       playerY - scale(50), scale(38), scale(38));
  move(playlistList_, railPad, scale(132), sidebarWidth - 2 * railPad,
       playerY - scale(194));

  move(backBtn_, workspaceX, scale(14), scale(38), scale(38));
  move(searchEdit_, searchX, scale(14), searchWidth - scale(42), scale(38));
  move(searchBtn_, searchX + searchWidth - scale(38), scale(14), scale(38),
       scale(38));

  const bool collection = workspaceKind_ == WorkspaceKind::Collection;
  const bool search = workspaceKind_ == WorkspaceKind::Search;
  const bool settings = workspaceKind_ == WorkspaceKind::Settings;
  SetControlGroupVisible(
      {workspaceCover_, workspaceTypeLbl_, middleLabel_, workspaceMetaLbl_,
       workspaceColumnsLbl_, workspaceTimeColumnLbl_,
       workspaceActionColumnLbl_, tracksList_},
      collection);
  const bool playlistActions =
      collection && collectionKind_ == CollectionKind::Playlist;
  ::ShowWindow(renPlBtn_, playlistActions ? SW_SHOW : SW_HIDE);
  ::ShowWindow(delPlBtn_, playlistActions ? SW_SHOW : SW_HIDE);
  SetControlGroupVisible({resultsLabel_, resultsList_}, search);
  SetControlGroupVisible(
      {settingsTitle_, settingsGuide_, engineGroupLbl_, engineGuideLbl_,
       engineStatusLbl_, loginBtn_, logoutBtn_, cacheStatusLbl_, statusLbl_},
      settings);

  if (collection) {
    const int headerTop = scale(72);
    const int coverSize = scale(112);
    move(workspaceCover_, workspaceX + scale(18), headerTop, coverSize,
         coverSize);
    const int textX = workspaceX + scale(18) + coverSize + scale(20);
    move(workspaceTypeLbl_, textX, headerTop + scale(3),
         workspaceRight - textX - scale(104), scale(18));
    move(middleLabel_, textX, headerTop + scale(25),
         workspaceRight - textX - scale(20), scale(42));
    move(workspaceMetaLbl_, textX, headerTop + scale(77),
         workspaceRight - textX - scale(20), scale(24));
    move(renPlBtn_, workspaceRight - scale(92), headerTop + scale(12),
         scale(34), scale(34));
    move(delPlBtn_, workspaceRight - scale(50), headerTop + scale(12),
         scale(34), scale(34));
    const int columnsY = headerTop + coverSize + scale(15);
    const int listLeft = workspaceX + scale(10);
    const int listRight = workspaceRight - scale(10);
    const int actionLeft = listRight - scale(kRowActionWidthDip);
    const int timeRight = actionLeft - scale(kRowRightPaddingDip);
    const int timeLeft = timeRight - scale(kRowDurationWidthDip);
    const int contentLeft =
        listLeft + scale(kRowArtworkLeftDip + kRowArtworkSizeDip +
                         kRowTextGapDip);
    move(workspaceColumnsLbl_, contentLeft, columnsY,
         timeLeft - contentLeft - scale(12), scale(20));
    move(workspaceTimeColumnLbl_, timeLeft, columnsY,
         scale(kRowDurationWidthDip), scale(20));
    move(workspaceActionColumnLbl_, actionLeft, columnsY,
         scale(kRowActionWidthDip), scale(20));
    move(tracksList_, listLeft, columnsY + scale(23),
         listRight - listLeft,
         std::max(0, playerY - columnsY - scale(23) - scale(52)));
  } else if (search) {
    move(resultsLabel_, workspaceX + scale(18), scale(82),
         workspaceWidth - scale(36), scale(22));
    move(resultsList_, workspaceX + scale(10), scale(116),
         workspaceWidth - scale(20),
         std::max(0, playerY - scale(116) - scale(52)));
  } else {
    const int top = scale(76);
    const int pad = scale(22);
    const int innerWidth = workspaceWidth - 2 * pad;
    const int left = workspaceX + pad;
    move(settingsTitle_, left, top, innerWidth, scale(36));
    move(settingsGuide_, left, top + scale(42), innerWidth, scale(42));
    move(engineGroupLbl_, left, top + scale(110), innerWidth, scale(18));
    move(engineGuideLbl_, left, top + scale(138), innerWidth, scale(68));
    move(engineStatusLbl_, left, top + scale(226), innerWidth, scale(38));
    move(loginBtn_, left, top + scale(266), scale(96), scale(34));
    move(logoutBtn_, left + scale(108), top + scale(266), scale(96), scale(34));
    move(cacheStatusLbl_, left, top + scale(316), innerWidth, scale(38));
  }

  // Status feedback stays visible in every workspace.
  move(statusLbl_, workspaceX + scale(22), playerY - scale(38),
       workspaceWidth - 2 * scale(22), scale(22));
  ::ShowWindow(statusLbl_, SW_SHOW);

  const int art = scale(68);
  const int playerPad = scale(14);
  move(coverArea_, playerPad, playerY + scale(15), art, art);
  const int metadataX = playerPad + art + scale(14);
  const int metadataWidth = std::min(scale(240), width / 4);
  move(nowPlayingLbl_, metadataX, playerY + scale(12), metadataWidth,
       scale(16));
  move(titleLbl_, metadataX, playerY + scale(29), metadataWidth, scale(22));
  move(artistLbl_, metadataX, playerY + scale(51), metadataWidth, scale(18));
  move(albumLbl_, metadataX, playerY + scale(69), metadataWidth, scale(18));

  const int volumeWidth = scale(180);
  const int volumeX = width - playerPad - volumeWidth;
  const int transportX = metadataX + metadataWidth + scale(18);
  const int transportWidth =
      std::max(scale(300), volumeX - scale(22) - transportX);
  const int icon = scale(30);
  const int playIcon = scale(38);
  const int controlsWidth = 4 * icon + playIcon + 4 * scale(7);
  const int controlsX = transportX + (transportWidth - controlsWidth) / 2;
  move(shuffleBtn_, controlsX, playerY + scale(7), icon, icon);
  move(prevBtn_, controlsX + icon + scale(7), playerY + scale(7), icon, icon);
  move(playBtn_, controlsX + 2 * (icon + scale(7)) - scale(4),
       playerY + scale(3), playIcon, playIcon);
  move(nextBtn_, controlsX + 2 * icon + playIcon + 3 * scale(7) - scale(4),
       playerY + scale(7), icon, icon);
  move(repeatBtn_, controlsX + 3 * icon + playIcon + 4 * scale(7) - scale(4),
       playerY + scale(7), icon, icon);
  move(elapsedLbl_, transportX, playerY + scale(54), scale(40), scale(18));
  move(seekBar_, transportX + scale(43), playerY + scale(48),
       transportWidth - scale(86), scale(28));
  move(durationLbl_, transportX + transportWidth - scale(40),
       playerY + scale(54), scale(40), scale(18));
  move(volumeBar_, volumeX, playerY + scale(50), volumeWidth - scale(44),
       scale(24));
  move(volumeLbl_, volumeX + volumeWidth - scale(40), playerY + scale(53),
       scale(40), scale(18));
  ::ShowWindow(localControlsLbl_, SW_HIDE);

  RECT listRect{};
  ::GetClientRect(resultsList_, &listRect);
  ::SendMessageW(resultsList_, LVM_SETCOLUMNWIDTH, 0,
                 std::max(0, static_cast<int>(listRect.right) - scale(8)));
  ::GetClientRect(tracksList_, &listRect);
  ::SendMessageW(tracksList_, LVM_SETCOLUMNWIDTH, 0,
                 std::max(0, static_cast<int>(listRect.right) - scale(8)));
  ::InvalidateRect(hwnd_, nullptr, TRUE);
}

LRESULT MainWindow::OnDrawItem(WPARAM, LPARAM lParam) {
  auto* draw = reinterpret_cast<DRAWITEMSTRUCT*>(lParam);
  if (!draw) return FALSE;

  if (draw->CtlType == ODT_LISTBOX && draw->CtlID == CID_PLAYLIST_LIST) {
    if (draw->itemID == static_cast<UINT>(-1)) return TRUE;
    const bool selected = (draw->itemState & ODS_SELECTED) != 0;
    const bool disabled = (draw->itemState & ODS_DISABLED) != 0;
    HBRUSH background =
        ::CreateSolidBrush(selected ? kSelect : kSidebar);
    ::FillRect(draw->hDC, &draw->rcItem, background);
    ::DeleteObject(background);
    auto scale = [this](int value) { return ::MulDiv(value, dpi_, 96); };
    RECT iconRect = draw->rcItem;
    iconRect.left += scale(2);
    iconRect.right = iconRect.left + scale(32);
    if (draw->itemID == 0) {
      // The Queue entry is not a playlist; it keeps its library glyph.
      DrawFluentIcon(draw->hDC, iconRect, fontIcon16_,
                     selected ? kAccent : kDim, FluentIcon::Queue);
    } else {
      // Playlist rows show the playlist cover art from the shared artwork
      // cache; until art arrives (or when a playlist has none) a seeded tile
      // with the playlist glyph is drawn instead.
      const PlaylistRef* playlist = RailPlaylistForRow(
          playlists_, filteredPlaylistIndices_,
          static_cast<int>(draw->itemID));
      Gdiplus::Image* image = nullptr;
      if (playlist && artworkCache_ && !playlist->cover_url.empty()) {
        auto found = artworkCache_->images.find(playlist->cover_url);
        if (found != artworkCache_->images.end()) image = found->second.get();
      }
      if (image && image->GetWidth() > 0 && image->GetHeight() > 0) {
        Gdiplus::Graphics graphics(draw->hDC);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        graphics.SetInterpolationMode(
            Gdiplus::InterpolationModeHighQualityBicubic);
        UINT sourceWidth = image->GetWidth();
        UINT sourceHeight = image->GetHeight();
        UINT sourceSize = std::min(sourceWidth, sourceHeight);
        graphics.DrawImage(
            image,
            Gdiplus::Rect(iconRect.left, iconRect.top,
                          iconRect.right - iconRect.left,
                          iconRect.bottom - iconRect.top),
            (sourceWidth - sourceSize) / 2, (sourceHeight - sourceSize) / 2,
            sourceSize, sourceSize, Gdiplus::UnitPixel);
      } else {
        uint32_t seed =
            playlist
                ? RowArtworkSeed(playlist->cover_url,
                                 Utf8ToWide(playlist->name))
                : RowArtworkSeed(std::string(), L"");
        COLORREF tile =
            RGB(0x25 + (seed & 0x0F), 0x29 + ((seed >> 4) & 0x0F),
                0x2D + ((seed >> 8) & 0x0F));
        {
          Gdiplus::Graphics graphics(draw->hDC);
          graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
          FillRoundedRectGp(graphics, iconRect, scale(6), tile);
        }
        DrawFluentIcon(draw->hDC, iconRect, fontIcon16_,
                       selected ? kText : kDim, FluentIcon::Playlist);
      }
    }
    wchar_t text[512] = {};
    ::SendMessageW(draw->hwndItem, LB_GETTEXT, draw->itemID,
                   reinterpret_cast<LPARAM>(text));
    RECT textRect = draw->rcItem;
    textRect.left += scale(38);
    textRect.right -= scale(8);
    ::SetBkMode(draw->hDC, TRANSPARENT);
    ::SetTextColor(draw->hDC,
                   disabled ? kDisabled : (selected ? kText : kDim));
    ::DrawTextW(draw->hDC, text, -1, &textRect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS |
                    DT_NOPREFIX);
    if (draw->itemState & ODS_FOCUS) {
      RECT focus = draw->rcItem;
      ::InflateRect(&focus, -scale(2), -scale(2));
      ::DrawFocusRect(draw->hDC, &focus);
    }
    return TRUE;
  }

  if (draw->CtlType == ODT_COMBOBOX) {
    bool selected = (draw->itemState & ODS_SELECTED) != 0;
    bool disabled = (draw->itemState & ODS_DISABLED) != 0;
    HBRUSH brush = ::CreateSolidBrush(selected ? kSelect : kControl);
    ::FillRect(draw->hDC, &draw->rcItem, brush);
    ::DeleteObject(brush);
    wchar_t text[512] = {};
    if (draw->itemID != static_cast<UINT>(-1))
      ::SendMessageW(draw->hwndItem, CB_GETLBTEXT, draw->itemID,
                     reinterpret_cast<LPARAM>(text));
    RECT textRect = draw->rcItem;
    textRect.left += ::MulDiv(12, dpi_, 96);
    textRect.right -= ::MulDiv(8, dpi_, 96);
    ::SetBkMode(draw->hDC, TRANSPARENT);
    ::SetTextColor(draw->hDC, disabled ? kDisabled : kText);
    ::DrawTextW(draw->hDC, text, -1, &textRect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS |
                    DT_NOPREFIX);
    return TRUE;
  }

  if (draw->CtlType != ODT_BUTTON) return FALSE;
  const bool pushed = (draw->itemState & ODS_SELECTED) != 0;
  const bool disabled = (draw->itemState & ODS_DISABLED) != 0;
  const bool hot = ::GetPropW(draw->hwndItem, kHotProp) != nullptr;
  const bool icon = IsIconButton(draw->CtlID);
  // Shuffle/repeat on-state is shown by an accent glyph plus an accent dot;
  // the off-state glyph is dim. Repeat-one additionally shows a "1" badge.
  const bool shuffleOn = draw->CtlID == CID_SHUFFLE_BTN && playback_.shuffle;
  const bool repeatOn = draw->CtlID == CID_REPEAT_BTN && playback_.repeat != "off";
  const bool repeatTrack = draw->CtlID == CID_REPEAT_BTN && playback_.repeat == "track";
  const bool settingsActive =
      draw->CtlID == CID_SETTINGS_BTN &&
      workspaceKind_ == WorkspaceKind::Settings;
  const bool active = shuffleOn || repeatOn || settingsActive;
  COLORREF background = kControl;
  if (draw->CtlID == CID_PLAY_BTN) {
    background = disabled ? kControl : (hot ? kAccentHot : kText);
  } else if (active) {
    background = hot ? kControlHot : kSelect;
  } else {
    background = hot ? kControlHot : kControl;
  }
  if (pushed && !disabled) background = active ? kControl : kPanel;
  const int radius = ::MulDiv(icon ? 18 : 8, dpi_, 96);
  // Clear the whole item rect first so the rounded fill never leaves stale
  // corner pixels behind when the button's surroundings change. (The
  // "obscuring rectangle on load" was a sibling paint stomping the buttons:
  // fixed by WS_CLIPSIBLINGS on every child.)
  HBRUSH base = ::CreateSolidBrush(ButtonBaseColor(draw->hwndItem));
  ::FillRect(draw->hDC, &draw->rcItem, base);
  ::DeleteObject(base);
  {
    Gdiplus::Graphics graphics(draw->hDC);
    graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
    FillRoundedRectGp(graphics, draw->rcItem, radius, background);
    if (draw->CtlID != CID_PLAY_BTN) {
      graphics.SetPixelOffsetMode(Gdiplus::PixelOffsetModeHalf);
      StrokeRoundedRectGp(graphics, draw->rcItem, radius,
                          disabled ? kBorderSoft : kBorder, 1.0f);
    }
  }
  COLORREF foreground =
      disabled ? kDisabled
               : (draw->CtlID == CID_PLAY_BTN
                      ? kAccentText
                      : (active ? kAccent : kText));
  if (icon) {
    FluentIcon glyph = FluentIcon::More;
    switch (draw->CtlID) {
      case CID_SEARCH_BTN:
        glyph = FluentIcon::Search;
        break;
      case CID_SETTINGS_BTN:
        glyph = FluentIcon::Settings;
        break;
      case CID_BACK_BTN:
        glyph = FluentIcon::Back;
        break;
      case CID_NEWPL_BTN:
        glyph = FluentIcon::Add;
        break;
      case CID_RENPL_BTN:
        glyph = FluentIcon::Edit;
        break;
      case CID_DELPL_BTN:
        glyph = FluentIcon::Delete;
        break;
      case CID_PREV_BTN:
        glyph = FluentIcon::Previous;
        break;
      case CID_PLAY_BTN:
        glyph = playback_.playing ? FluentIcon::Pause : FluentIcon::Play;
        break;
      case CID_NEXT_BTN:
        glyph = FluentIcon::Next;
        break;
      case CID_SHUFFLE_BTN:
        glyph = FluentIcon::Shuffle;
        break;
      case CID_REPEAT_BTN:
        glyph = FluentIcon::Repeat;
        break;
    }
    HFONT font = draw->CtlID == CID_PLAY_BTN ? fontIcon20_ : fontIcon16_;
    DrawFluentIcon(draw->hDC, draw->rcItem, font, foreground, glyph);
    if (!disabled && (shuffleOn || repeatOn)) {
      // Small accent dot under the glyph makes the on-state unambiguous.
      const int dot = std::max(3, ::MulDiv(4, dpi_, 96));
      RECT dotRect{(draw->rcItem.left + draw->rcItem.right) / 2 - dot / 2,
                   draw->rcItem.bottom - ::MulDiv(8, dpi_, 96) - dot,
                   (draw->rcItem.left + draw->rcItem.right) / 2 + dot / 2 + 1,
                   draw->rcItem.bottom - ::MulDiv(8, dpi_, 96)};
      Gdiplus::Graphics graphics(draw->hDC);
      graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
      FillEllipseGp(graphics, dotRect, kAccent);
    }
    if (!disabled && repeatTrack) {
      const int badge = std::max(12, ::MulDiv(16, dpi_, 96));
      RECT badgeRect{draw->rcItem.right - badge - ::MulDiv(2, dpi_, 96),
                     draw->rcItem.top + ::MulDiv(2, dpi_, 96),
                     draw->rcItem.right - ::MulDiv(2, dpi_, 96),
                     draw->rcItem.top + ::MulDiv(2, dpi_, 96) + badge};
      {
        Gdiplus::Graphics graphics(draw->hDC);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillEllipseGp(graphics, badgeRect, kAccent);
      }
      HGDIOBJ oldBadgeFont = ::SelectObject(draw->hDC, fontSmall_);
      ::SetBkMode(draw->hDC, TRANSPARENT);
      ::SetTextColor(draw->hDC, kAccentText);
      ::DrawTextW(draw->hDC, L"1", 1, &badgeRect,
                  DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
      ::SelectObject(draw->hDC, oldBadgeFont);
    }
  } else {
    wchar_t text[128] = {};
    ::GetWindowTextW(draw->hwndItem, text, 127);
    ::SetBkMode(draw->hDC, TRANSPARENT);
    ::SetTextColor(draw->hDC, foreground);
    ::DrawTextW(draw->hDC, text, -1, &draw->rcItem,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS |
                    DT_NOPREFIX);
  }
  // No keyboard-focus outline on buttons by design; focus stays visible only
  // on text fields (caret) and list rows.
  return TRUE;
}

LRESULT MainWindow::OnMeasureItem(WPARAM, LPARAM lParam) {
  auto* measure = reinterpret_cast<MEASUREITEMSTRUCT*>(lParam);
  if (!measure) return FALSE;
  if (measure->CtlType == ODT_COMBOBOX) {
    measure->itemHeight = ::MulDiv(32, dpi_, 96);
    return TRUE;
  }
  if (measure->CtlType == ODT_LISTBOX &&
      measure->CtlID == CID_PLAYLIST_LIST) {
    measure->itemHeight = ::MulDiv(38, dpi_, 96);
    return TRUE;
  }
  return FALSE;
}
const ListRow* MainWindow::RowAt(HWND list, int index) const {
  if (index < 0) return nullptr;
  const auto& rows = list == resultsList_ ? searchRows_ : middleRows_;
  if (static_cast<size_t>(index) >= rows.size()) return nullptr;
  return &rows[index];
}

COLORREF MainWindow::ButtonBaseColor(HWND control) const {
  if (!hwnd_ || !control) return kPanel;
  RECT parent{};
  ::GetClientRect(hwnd_, &parent);
  RECT rc{};
  ::GetWindowRect(control, &rc);
  POINT origin{rc.left, rc.top};
  ::ScreenToClient(hwnd_, &origin);
  const int centerY = origin.y + (rc.bottom - rc.top) / 2;
  const int playerY =
      parent.bottom - ::MulDiv(98, dpi_, 96);
  if (centerY >= playerY) return kPlayer;
  if (origin.x + rc.right - rc.left <= ::MulDiv(238, dpi_, 96))
    return kSidebar;
  if (origin.y < ::MulDiv(64, dpi_, 96)) return kBg;
  return kPanel;
}

void MainWindow::SetListMessage(HWND list, const std::wstring& title,
                                const std::wstring& detail) {
  if (list == resultsList_) {
    resultsEmptyTitle_ = title;
    resultsEmptyDetail_ = detail;
  } else {
    middleEmptyTitle_ = title;
    middleEmptyDetail_ = detail;
  }
  ::SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
  std::wstring accessible = title + L". " + detail;
  ::SetWindowTextW(list, accessible.c_str());
  ::InvalidateRect(list, nullptr, TRUE);
}

void MainWindow::BeginNestedCollection(CollectionKind kind,
                                       const std::wstring& title,
                                       const std::string& artworkUrl) {
  navStack_.push_back(
      {workspaceKind_, collectionKind_, selectedMiddleIndex_,
       workspaceTitle_, workspaceArtworkUrl_});
  collectionKind_ = kind;
  workspaceTitle_ = title;
  UpdateWorkspaceArtwork(artworkUrl);
  ShowWorkspace(WorkspaceKind::Collection);
  UpdateWorkspaceHeader();
}

void MainWindow::PopNestedCollection() {
  if (navStack_.empty()) {
    app_->OnBack();
    return;
  }
  NavEntry entry = std::move(navStack_.back());
  navStack_.pop_back();
  selectedMiddleIndex_ = entry.middleIndex;
  collectionKind_ = entry.collection;
  workspaceTitle_ = entry.title;
  UpdateWorkspaceArtwork(entry.artworkUrl);
  if (entry.workspace == WorkspaceKind::Search) {
    ShowWorkspace(WorkspaceKind::Search);
    return;
  }
  // Restore the previous collection and reload its contents (queue or the
  // cached playlist) so the list is never stale.
  ShowWorkspace(WorkspaceKind::Collection);
  SelectPlaylistRow(entry.middleIndex, true);
}

void MainWindow::ActivateSelection(HWND list) {
  int index =
      list == resultsList_ ? SelectedResultIndex() : SelectedTrackIndex();
  const ListRow* row = RowAt(list, index);
  if (!row) return;
  ListRowKind kind = row->kind;
  if (kind == ListRowKind::Album || kind == ListRowKind::Artist) {
    BeginNestedCollection(kind == ListRowKind::Album ? CollectionKind::Album
                                                     : CollectionKind::Artist,
                          row->title, row->artworkUrl);
    if (app_->IsAuthed()) {
      middleRows_.clear();
      middleLoading_ = true;
      SetListMessage(tracksList_, L"Loading collection",
                     L"Spotify is fetching the selected collection.");
    }
  }
  if (list == resultsList_)
    app_->OnSearchActivate(index);
  else
    app_->OnMiddleActivate(index);
}

void MainWindow::RequestArtwork(const std::vector<ListRow>& rows) {
  if (!app_ || !artworkCache_) return;
  if (artworkCache_->requested.size() >= 512)
    artworkCache_->requested.clear();
  size_t issued = 0;
  for (const auto& row : rows) {
    if (row.artworkUrl.empty() ||
        artworkCache_->images.find(row.artworkUrl) !=
            artworkCache_->images.end())
      continue;
    if (artworkCache_->requested.insert(row.artworkUrl).second) {
      app_->OnTrackArtworkNeeded(row.artworkUrl);
      if (++issued == 12) break;
    }
  }
}


LRESULT MainWindow::OnNotify(WPARAM, LPARAM lParam) {
  auto* header = reinterpret_cast<NMHDR*>(lParam);
  HWND list = header->hwndFrom;

  if (header->code == NM_CUSTOMDRAW &&
      (list == resultsList_ || list == tracksList_)) {
    auto* custom = reinterpret_cast<NMLVCUSTOMDRAW*>(lParam);
    HDC dc = custom->nmcd.hdc;
    auto scale = [this](int value) { return ::MulDiv(value, dpi_, 96); };
    if (custom->nmcd.dwDrawStage == CDDS_PREPAINT) {
      if (ListView_GetItemCount(list) == 0) {
        RECT client{};
        ::GetClientRect(list, &client);
        ::FillRect(dc, &client, brushPanel_);
        const std::wstring& title =
            list == resultsList_ ? resultsEmptyTitle_ : middleEmptyTitle_;
        const std::wstring& detail =
            list == resultsList_ ? resultsEmptyDetail_ : middleEmptyDetail_;
        int blockHeight = scale(92);
        int top =
            std::max(scale(18),
                     (static_cast<int>(client.bottom) - blockHeight) / 2);
        RECT glyph{client.left + (client.right - scale(44)) / 2, top,
                   client.left + (client.right + scale(44)) / 2,
                   top + scale(44)};
        {
          Gdiplus::Graphics graphics(dc);
          graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
          FillRoundedRectGp(graphics, glyph, scale(12), kControl);
        }
        DrawFluentIcon(dc, glyph, fontIcon24_, kAccent,
                       FluentIcon::Queue);
        RECT titleRect{client.left + scale(18), glyph.bottom + scale(10),
                       client.right - scale(18), glyph.bottom + scale(32)};
        HGDIOBJ oldFont = ::SelectObject(dc, fontRowTitle_);
        ::SetBkMode(dc, TRANSPARENT);
        ::SetTextColor(dc, kText);
        ::DrawTextW(dc, title.c_str(), -1, &titleRect,
                    DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        RECT detailRect{client.left + scale(24), titleRect.bottom + scale(3),
                        client.right - scale(24), titleRect.bottom + scale(40)};
        ::SelectObject(dc, fontSmall_);
        ::SetTextColor(dc, kDim);
        ::DrawTextW(dc, detail.c_str(), -1, &detailRect,
                    DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX);
        ::SelectObject(dc, oldFont);
        return CDRF_SKIPDEFAULT;
      }
      return CDRF_NOTIFYITEMDRAW;
    }
    if (custom->nmcd.dwDrawStage != CDDS_ITEMPREPAINT)
      return CDRF_DODEFAULT;

    int index = static_cast<int>(custom->nmcd.dwItemSpec);
    const ListRow* row = RowAt(list, index);
    if (!row) return CDRF_DODEFAULT;
    RECT item{};
    if (!ListView_GetItemRect(list, index, &item, LVIR_BOUNDS))
      return CDRF_DODEFAULT;
    RECT client{};
    ::GetClientRect(list, &client);
    item.right = client.right;
    UINT itemState =
        ListView_GetItemState(list, index, LVIS_SELECTED | LVIS_FOCUSED);
    bool selected = (itemState & LVIS_SELECTED) != 0;
    bool focused = (itemState & LVIS_FOCUSED) != 0;
    int hover = DecodeHoverIndex(
        reinterpret_cast<INT_PTR>(::GetPropW(list, kListHoverProp)));
    bool hot = hover == index;
    HBRUSH background =
        ::CreateSolidBrush(selected ? kSelect : (hot ? kControl : kPanel));
    ::FillRect(dc, &item, background);
    ::DeleteObject(background);

    RECT separator{item.left + scale(8), item.bottom - 1,
                   item.right - scale(8), item.bottom};
    HBRUSH separatorBrush = ::CreateSolidBrush(kBorderSoft);
    ::FillRect(dc, &separator, separatorBrush);
    ::DeleteObject(separatorBrush);
    if (selected) {
      RECT accent{item.left, item.top + scale(7), item.left + scale(3),
                  item.bottom - scale(7)};
      HBRUSH accentBrush = ::CreateSolidBrush(kAccent);
      ::FillRect(dc, &accent, accentBrush);
      ::DeleteObject(accentBrush);
    }

    int artworkSize = scale(kRowArtworkSizeDip);
    RECT artworkRect{
        item.left + scale(kRowArtworkLeftDip), item.top + scale(9),
        item.left + scale(kRowArtworkLeftDip) + artworkSize,
        item.top + scale(9) + artworkSize};
    Gdiplus::Image* image = nullptr;
    if (artworkCache_ && !row->artworkUrl.empty()) {
      auto found = artworkCache_->images.find(row->artworkUrl);
      if (found != artworkCache_->images.end()) {
        image = found->second.get();
      } else if (artworkCache_->requested.insert(row->artworkUrl).second) {
        app_->OnTrackArtworkNeeded(row->artworkUrl);
      }
    }
    if (image && image->GetWidth() > 0 && image->GetHeight() > 0) {
      Gdiplus::Graphics graphics(dc);
      graphics.SetInterpolationMode(Gdiplus::InterpolationModeHighQualityBicubic);
      UINT imageWidth = image->GetWidth();
      UINT imageHeight = image->GetHeight();
      UINT sourceSize = std::min(imageWidth, imageHeight);
      UINT sourceX = (imageWidth - sourceSize) / 2;
      UINT sourceY = (imageHeight - sourceSize) / 2;
      graphics.DrawImage(
          image,
          Gdiplus::Rect(artworkRect.left, artworkRect.top, artworkSize,
                        artworkSize),
          sourceX, sourceY, sourceSize, sourceSize, Gdiplus::UnitPixel);
    } else {
      uint32_t seed = row->artworkSeed;
      COLORREF tile =
          RGB(0x25 + (seed & 0x0F), 0x29 + ((seed >> 4) & 0x0F),
              0x2D + ((seed >> 8) & 0x0F));
      {
        Gdiplus::Graphics graphics(dc);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillRoundedRectGp(graphics, artworkRect, scale(7), tile);
      }
      DrawFluentIcon(dc, artworkRect, fontIcon24_,
                     selected ? kText : kDim, FluentIcon::Album);
    }

    // Hover-revealed play button overlaid on the artwork tile for tracks.
    // Clicking the tile plays the row's context from this index.
    if (row->kind == ListRowKind::Track && hot) {
      const int buttonSize = std::max(26, scale(30));
      RECT play{(artworkRect.left + artworkRect.right - buttonSize) / 2,
                (artworkRect.top + artworkRect.bottom - buttonSize) / 2,
                (artworkRect.left + artworkRect.right + buttonSize) / 2,
                (artworkRect.top + artworkRect.bottom + buttonSize) / 2};
      {
        Gdiplus::Graphics graphics(dc);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillEllipseGp(graphics, play, kAccent);
      }
      DrawFluentIcon(dc, play, fontIcon16_, kAccentText, FluentIcon::Play);
    }

    const bool track = row->kind == ListRowKind::Track;
    const int textLeft = artworkRect.right + scale(kRowTextGapDip);
    const int actionLeft =
        track ? item.right - scale(kRowActionWidthDip) : item.right;
    const int textRight = actionLeft - scale(kRowRightPaddingDip);
    const int durationWidth =
        track ? scale(kRowDurationWidthDip) : 0;
    ::SetBkMode(dc, TRANSPARENT);
    HGDIOBJ oldFont = ::SelectObject(dc, fontSmall_);
    ::SetTextColor(dc, selected ? kText : kDim);
    RECT eyebrow{textLeft, item.top + scale(5), textRight,
                 item.top + scale(18)};
    ::DrawTextW(dc, row->eyebrow.c_str(), -1, &eyebrow,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);

    ::SelectObject(dc, fontRowTitle_);
    ::SetTextColor(dc, kText);
    RECT title{textLeft, item.top + scale(17),
               textRight - durationWidth - (durationWidth ? scale(12) : 0),
               item.top + scale(39)};
    ::DrawTextW(dc, row->title.c_str(), -1, &title,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS |
                    DT_NOPREFIX);
    if (durationWidth) {
      RECT duration{textRight - durationWidth, item.top + scale(17), textRight,
                    item.top + scale(39)};
      ::SelectObject(dc, fontList_);
      ::SetTextColor(dc, selected ? kText : kDim);
      ::DrawTextW(dc, row->duration.c_str(), -1, &duration,
                  DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
      RECT overflow{actionLeft, item.top, item.right, item.bottom};
      ::SetTextColor(dc, selected || hot ? kText : kDim);
      DrawFluentIcon(dc, overflow, fontIcon16_,
                     selected || hot ? kText : kDim, FluentIcon::More);
    }
    ::SelectObject(dc, fontList_);
    ::SetTextColor(dc, selected || hot ? kText : kDim);
    RECT detail{textLeft, item.top + scale(40), textRight,
                item.bottom - scale(5)};
    ::DrawTextW(dc, row->detail.c_str(), -1, &detail,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
    ::SelectObject(dc, oldFont);

    if (focused) {
      RECT focus = item;
      ::InflateRect(&focus, -scale(2), -scale(2));
      HPEN focusPen = ::CreatePen(PS_SOLID, 1, kAccent);
      HGDIOBJ oldFocusPen = ::SelectObject(dc, focusPen);
      HGDIOBJ oldFocusBrush =
          ::SelectObject(dc, ::GetStockObject(NULL_BRUSH));
      ::RoundRect(dc, focus.left, focus.top, focus.right - 1,
                  focus.bottom - 1, scale(8), scale(8));
      ::SelectObject(dc, oldFocusBrush);
      ::SelectObject(dc, oldFocusPen);
      ::DeleteObject(focusPen);
    }
    return CDRF_SKIPDEFAULT;
  }

  if (header->code == NM_CLICK &&
      (list == resultsList_ || list == tracksList_)) {
    auto* click = reinterpret_cast<NMITEMACTIVATE*>(lParam);
    const ListRow* row = RowAt(list, click->iItem);
    if (row) {
      RECT item{};
      if (ListView_GetItemRect(list, click->iItem, &item, LVIR_BOUNDS)) {
        if (row->kind == ListRowKind::Track &&
            RowTileHit(click->ptAction.x, click->ptAction.y, item.left,
                       item.top, static_cast<int>(dpi_))) {
          // Same activation path as double-click / Enter: play the full
          // context starting at this row. The flag keeps the follow-up
          // NM_DBLCLK from replaying the same row.
          suppressNextDoubleActivate_ = true;
          ListView_SetItemState(list, click->iItem,
                                LVIS_SELECTED | LVIS_FOCUSED,
                                LVIS_SELECTED | LVIS_FOCUSED);
          ActivateSelection(list);
          return 0;
        }
      } else {
        suppressNextDoubleActivate_ = false;
      }
      RECT clientRect{};
      ::GetClientRect(list, &clientRect);
      if (row->kind == ListRowKind::Track &&
          click->ptAction.x >=
              clientRect.right - ::MulDiv(kRowActionWidthDip, dpi_, 96)) {
        ListView_SetItemState(list, click->iItem,
                              LVIS_SELECTED | LVIS_FOCUSED,
                              LVIS_SELECTED | LVIS_FOCUSED);
        POINT point{click->ptAction.x, click->ptAction.y};
        ::ClientToScreen(list, &point);
        ShowContextMenu(list, point.x, point.y);
        return 0;
      }
    }
  }
  if (header->code == NM_DBLCLK) {
    auto* activate = reinterpret_cast<NMITEMACTIVATE*>(lParam);
    if ((list == resultsList_ || list == tracksList_) &&
        activate->iItem >= 0) {
      // A single tile click already played this row; do not replay it.
      if (suppressNextDoubleActivate_) {
        suppressNextDoubleActivate_ = false;
      } else {
        ListView_SetItemState(list, activate->iItem,
                              LVIS_SELECTED | LVIS_FOCUSED,
                              LVIS_SELECTED | LVIS_FOCUSED);
        ActivateSelection(list);
      }
    }
    return 0;
  }
  if (header->code == LVN_KEYDOWN) {
    auto* key = reinterpret_cast<NMLVKEYDOWN*>(lParam);
    int index = list == resultsList_ ? SelectedResultIndex() : SelectedTrackIndex();
    if (index >= 0) {
      if (key->wVKey == VK_RETURN || key->wVKey == VK_SPACE) {
        ActivateSelection(list);
      } else if (key->wVKey == VK_DELETE && list == tracksList_ &&
                 collectionKind_ == CollectionKind::Playlist) {
        const ListRow* row = RowAt(list, index);
        if (row && row->kind == ListRowKind::Track)
          app_->OnMiddleContext(IDM_CTX_MIDDLE_REMOVE, index);
      }
    }
    return 0;
  }
  return 0;
}

void MainWindow::ShowContextMenu(HWND ctrl, int x, int y) {
  int idx =
      ctrl == resultsList_ ? SelectedResultIndex() : SelectedTrackIndex();
  const ListRow* row = RowAt(ctrl, idx);
  if (idx < 0 || !row) return;

  const TrackRef* track = nullptr;
  if (row->kind == ListRowKind::Track) {
    if (ctrl == tracksList_) {
      if (static_cast<size_t>(idx) < middleTracks_.size())
        track = &middleTracks_[idx];
    } else {
      size_t trackIndex = 0;
      for (int i = 0; i < idx; ++i)
        if (resultKinds_[i] == 0) ++trackIndex;
      if (trackIndex < search_.tracks.size()) track = &search_.tracks[trackIndex];
    }
  }

  HMENU menu = ::CreatePopupMenu();
  auto add = [menu](UINT id, const wchar_t* text) {
    ::AppendMenuW(menu, MF_STRING, id, text);
  };
  if (ctrl == resultsList_) {
    if (row->kind == ListRowKind::Track) {
      add(IDM_CTX_PLAY_TRACK, L"Play");
      add(IDM_CTX_ADD_QUEUE, L"Add to queue");
      if (track && (!track->album_id.empty() || !track->artist_id.empty()))
        ::AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
      if (track && !track->album_id.empty())
        add(IDM_CTX_OPEN_ALBUM, L"Go to album");
      if (track && !track->artist_id.empty())
        add(IDM_CTX_ARTIST_ALBUMS, L"Go to artist");
      if (!playlists_.empty()) {
        ::AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
        HMENU sub = ::CreatePopupMenu();
        for (size_t i = 0; i < playlists_.size(); ++i)
          ::AppendMenuW(sub, MF_STRING,
                        IDM_CTX_ADD_PLAYLIST_BASE + static_cast<UINT>(i),
                        Utf8ToWide(playlists_[i].name).c_str());
        ::AppendMenuW(menu, MF_POPUP, reinterpret_cast<UINT_PTR>(sub),
                      L"Add to playlist");
      }
    } else if (row->kind == ListRowKind::Album) {
      add(IDM_CTX_OPEN_ALBUM, L"Go to album");
    } else {
      add(IDM_CTX_ARTIST_ALBUMS, L"Go to artist");
    }
  } else {
    if (row->kind == ListRowKind::Album) {
      add(IDM_CTX_PLAY_MIDDLE, L"Go to album");
    } else if (row->kind == ListRowKind::Track) {
      add(IDM_CTX_PLAY_MIDDLE, L"Play");
      if (collectionKind_ != CollectionKind::Queue)
        add(IDM_CTX_MIDDLE_ADD_QUEUE, L"Add to queue");
      if (track && (!track->album_id.empty() || !track->artist_id.empty()))
        ::AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
      if (track && !track->album_id.empty())
        add(IDM_CTX_OPEN_ALBUM, L"Go to album");
      if (track && !track->artist_id.empty())
        add(IDM_CTX_ARTIST_ALBUMS, L"Go to artist");
      if (collectionKind_ == CollectionKind::Playlist ||
          collectionKind_ == CollectionKind::Queue) {
        ::AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
        add(IDM_CTX_MIDDLE_REMOVE,
            collectionKind_ == CollectionKind::Queue
                ? L"Remove from queue"
                : L"Remove from playlist");
        add(IDM_CTX_MIDDLE_UP, L"Move up");
        add(IDM_CTX_MIDDLE_DOWN, L"Move down");
      }
    }
  }

  UINT command = ::TrackPopupMenu(
      menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, x, y, 0, hwnd_,
      nullptr);
  ::DestroyMenu(menu);
  if (command == 0) return;
  if (command == IDM_CTX_OPEN_ALBUM ||
      (command == IDM_CTX_PLAY_MIDDLE &&
       row->kind == ListRowKind::Album)) {
    std::wstring title =
        track && !track->album_name.empty() ? Utf8ToWide(track->album_name)
                                            : row->title;
    BeginNestedCollection(CollectionKind::Album, title, row->artworkUrl);
  } else if (command == IDM_CTX_ARTIST_ALBUMS) {
    std::wstring title =
        track && !track->artist_names.empty()
            ? Utf8ToWide(track->artist_names.front())
            : row->title;
    BeginNestedCollection(CollectionKind::Artist, title, row->artworkUrl);
  }
  if (ctrl == resultsList_)
    app_->OnSearchContext(command, idx);
  else
    app_->OnMiddleContext(command, idx);
}

LRESULT CALLBACK MainWindow::CoverProc(HWND h, UINT m, WPARAM w, LPARAM l) {
  CoverCtx* ctx = (CoverCtx*)::GetWindowLongPtrW(h, GWLP_USERDATA);
  switch (m) {
    case WM_SR_COVER_LOAD: {
      wchar_t* path = (wchar_t*)l;
      if (ctx) {
        if (ctx->img) {
          delete ctx->img;
          ctx->img = nullptr;
        }
        if (path && *path) {
          ctx->img = Gdiplus::Image::FromFile(path);
        }
      }
      free(path);
      ::InvalidateRect(h, nullptr, TRUE);
      return 0;
    }
    case WM_PAINT: {
      PAINTSTRUCT ps;
      HDC dc = ::BeginPaint(h, &ps);
      RECT rc;
      ::GetClientRect(h, &rc);
      ::FillRect(dc, &rc, (HBRUSH)::GetClassLongPtrW(h, GCLP_HBRBACKGROUND));
      if (ctx && ctx->img && ctx->img->GetWidth() > 0) {
        Gdiplus::Graphics g(dc);
        g.SetInterpolationMode(Gdiplus::InterpolationModeHighQualityBicubic);
        float imageWidth = static_cast<float>(ctx->img->GetWidth());
        float imageHeight = static_cast<float>(ctx->img->GetHeight());
        float controlWidth = static_cast<float>(rc.right - rc.left);
        float controlHeight = static_cast<float>(rc.bottom - rc.top);
        float scale = std::min(controlWidth / imageWidth, controlHeight / imageHeight);
        float drawWidth = imageWidth * scale, drawHeight = imageHeight * scale;
        g.DrawImage(ctx->img, (controlWidth - drawWidth) / 2.0f,
                    (controlHeight - drawHeight) / 2.0f, drawWidth, drawHeight);
      } else if (ctx && ctx->owner && ctx->owner->fontIcon40_) {
        DrawFluentIcon(dc, rc, ctx->owner->fontIcon40_, kDim,
                       FluentIcon::Album);
      }
      ::EndPaint(h, &ps);
      return 0;
    }
  }
  return ::DefWindowProcW(h, m, w, l);
}

LRESULT CALLBACK MainWindow::WndProc(HWND h, UINT m, WPARAM w, LPARAM l) {
  MainWindow* self = (MainWindow*)::GetWindowLongPtrW(h, GWLP_USERDATA);
  if (m == WM_NCCREATE) {
    self = (MainWindow*)((CREATESTRUCTW*)l)->lpCreateParams;
    ::SetWindowLongPtrW(h, GWLP_USERDATA, (LONG_PTR)self);
    self->hwnd_ = h;
    return ::DefWindowProcW(h, m, w, l);
  }
  if (!self) return ::DefWindowProcW(h, m, w, l);

  switch (m) {
    case WM_CREATE: {
      self->CreateChildren();
      self->Layout();
      return 0;
    }
    case WM_ERASEBKGND:
      return 1;
    case WM_PAINT: {
      PAINTSTRUCT paint{};
      HDC dc = ::BeginPaint(h, &paint);
      RECT client{};
      ::GetClientRect(h, &client);
      ::FillRect(dc, &client, self->brushBg_);
      auto scale = [self](int value) {
        return ::MulDiv(value, self->dpi_, 96);
      };
      const int sidebarWidth = scale(238);
      const int playerHeight = scale(98);
      const int playerY = client.bottom - playerHeight;
      RECT sidebar{0, 0, sidebarWidth, playerY};
      ::FillRect(dc, &sidebar, self->brushSidebar_);
      RECT workspace{sidebarWidth + scale(12), scale(64),
                     client.right - scale(12), playerY - scale(10)};
      {
        Gdiplus::Graphics graphics(dc);
        graphics.SetSmoothingMode(Gdiplus::SmoothingModeAntiAlias);
        FillRoundedRectGp(graphics, workspace, scale(12), kPanel);
      }
      RECT sidebarRule{sidebarWidth - 1, 0, sidebarWidth, playerY};
      HBRUSH rule = ::CreateSolidBrush(kBorderSoft);
      ::FillRect(dc, &sidebarRule, rule);
      RECT player{0, playerY, client.right, client.bottom};
      ::FillRect(dc, &player, self->brushPlayer_);
      RECT playerRule{0, playerY, client.right, playerY + 1};
      ::FillRect(dc, &playerRule, rule);
      ::DeleteObject(rule);
      ::EndPaint(h, &paint);
      return 0;
    }
    case WM_SIZE:
      self->Layout();
      return 0;
    case WM_DPICHANGED: {
      RECT* suggested = reinterpret_cast<RECT*>(l);
      ::SetWindowPos(h, nullptr, suggested->left, suggested->top,
                     suggested->right - suggested->left,
                     suggested->bottom - suggested->top,
                     SWP_NOZORDER | SWP_NOACTIVATE);
      self->dpi_ = HIWORD(w);
      self->ApplyFonts();
      if (self->rowHeightImageList_) {
        ::SendMessageW(self->resultsList_, LVM_SETIMAGELIST, LVSIL_SMALL, 0);
        ::SendMessageW(self->tracksList_, LVM_SETIMAGELIST, LVSIL_SMALL, 0);
        ::ImageList_Destroy(self->rowHeightImageList_);
      }
      self->rowHeightImageList_ =
          ::ImageList_Create(1, ::MulDiv(64, self->dpi_, 96), ILC_COLOR32, 1, 1);
      ::SendMessageW(self->resultsList_, LVM_SETIMAGELIST, LVSIL_SMALL,
                     reinterpret_cast<LPARAM>(self->rowHeightImageList_));
      ::SendMessageW(self->tracksList_, LVM_SETIMAGELIST, LVSIL_SMALL,
                     reinterpret_cast<LPARAM>(self->rowHeightImageList_));
      int comboHeight = ::MulDiv(32, self->dpi_, 96);
      ::SendMessageW(self->middleCombo_, CB_SETITEMHEIGHT, static_cast<WPARAM>(-1),
                     comboHeight);
      ::SendMessageW(self->middleCombo_, CB_SETITEMHEIGHT, 0, comboHeight);
      ::SendMessageW(self->playlistList_, LB_SETITEMHEIGHT, 0,
                     ::MulDiv(38, self->dpi_, 96));
      const int editMargin = ::MulDiv(12, self->dpi_, 96);
      for (HWND edit : {self->searchEdit_, self->playlistFilterEdit_}) {
        if (edit)
          ::SendMessageW(edit, EM_SETMARGINS, EC_LEFTMARGIN | EC_RIGHTMARGIN,
                         MAKELPARAM(editMargin, editMargin));
      }
      self->Layout();
      return 0;
    }
    case WM_GETMINMAXINFO: {
      auto* minmax = reinterpret_cast<MINMAXINFO*>(l);
      minmax->ptMinTrackSize.x = ::MulDiv(1040, self->dpi_, 96);
      minmax->ptMinTrackSize.y = ::MulDiv(680, self->dpi_, 96);
      return 0;
    }
    case WM_DRAWITEM:
      return self->OnDrawItem(w, l);
    case WM_MEASUREITEM:
      return self->OnMeasureItem(w, l);
    case WM_NOTIFY:
      return self->OnNotify(w, l);
    case WM_CONTEXTMENU: {
      HWND control = reinterpret_cast<HWND>(w);
      if (control == self->resultsList_ || control == self->tracksList_) {
        int x = GET_X_LPARAM(l);
        int y = GET_Y_LPARAM(l);
        if (x == -1 && y == -1) {
          int index = control == self->resultsList_ ? self->SelectedResultIndex()
                                                   : self->SelectedTrackIndex();
          RECT item{};
          if (index >= 0 &&
              ListView_GetItemRect(control, index, &item, LVIR_BOUNDS)) {
            POINT point{item.left + ::MulDiv(18, self->dpi_, 96), item.bottom};
            ::ClientToScreen(control, &point);
            x = point.x;
            y = point.y;
          } else {
            POINT point{0, 0};
            ::ClientToScreen(control, &point);
            x = point.x;
            y = point.y;
          }
        }
        self->ShowContextMenu(control, x, y);
      }
      return 0;
    }
    case TrayIcon::WM_CALLBACK:
      if (LOWORD(l) == WM_LBUTTONDBLCLK) {
        self->app_->OnTrayShow();
      } else if (LOWORD(l) == WM_RBUTTONUP || LOWORD(l) == WM_CONTEXTMENU) {
        self->app_->OnTrayCommand(0);
      }
      return 0;
    case WM_COMMAND: {
      UINT id = LOWORD(w);
      switch (id) {
        case CID_PLAYLIST_FILTER:
          if (HIWORD(w) == EN_CHANGE) self->RebuildPlaylistRail();
          return 0;
        case CID_PLAYLIST_LIST:
          if (HIWORD(w) == LBN_SELCHANGE) {
            int row = static_cast<int>(
                ::SendMessageW(self->playlistList_, LB_GETCURSEL, 0, 0));
            if (row >= 0 &&
                static_cast<size_t>(row) <
                    self->filteredPlaylistIndices_.size())
              self->SelectPlaylistRow(
                  self->filteredPlaylistIndices_[row], true);
          }
          return 0;
        case CID_SETTINGS_BTN:
          self->ShowWorkspace(
              self->workspaceKind_ == WorkspaceKind::Settings
                  ? self->previousWorkspaceKind_
                  : WorkspaceKind::Settings);
          return 0;
        case CID_MIDDLE_COMBO:
          if (HIWORD(w) == CBN_SELCHANGE) {
            if (self->app_->IsAuthed()) {
              self->middleRows_.clear();
              self->middleLoading_ = true;
              self->SetListMessage(self->tracksList_, L"Loading collection",
                                   L"Spotify is fetching tracks for this view.");
            }
            self->app_->OnMiddleCombo(self->MiddleComboIndex());
          }
          return 0;
        case CID_SEARCH_BTN: {
          std::string query = self->SearchQuery();
          if (query.find_first_not_of(" \t\r\n") == std::string::npos)
            return 0;
          self->ShowWorkspace(WorkspaceKind::Search);
          if (self->demoMode_) return 0;
          if (self->app_->IsAuthed()) {
            self->searchRows_.clear();
            self->resultKinds_.clear();
            self->resultsLoading_ = true;
            self->SetListMessage(
                self->resultsList_, L"Searching Spotify",
                L"Tracks, albums and artists will appear here.");
          }
          self->app_->OnSearch(query);
          return 0;
        }
        case CID_BACK_BTN:
          if (self->workspaceKind_ == WorkspaceKind::Settings) {
            self->ShowWorkspace(self->previousWorkspaceKind_);
          } else if (self->workspaceKind_ == WorkspaceKind::Search) {
            self->ShowWorkspace(WorkspaceKind::Collection);
          } else {
            self->PopNestedCollection();
          }
          return 0;
        case CID_NEWPL_BTN:
          self->app_->OnNewPlaylist();
          return 0;
        case CID_RENPL_BTN:
          self->app_->OnRenamePlaylist();
          return 0;
        case CID_DELPL_BTN:
          self->app_->OnDeletePlaylist();
          return 0;
        case CID_PREV_BTN:
          self->app_->OnPrevious();
          return 0;
        case CID_PLAY_BTN:
          self->app_->OnTogglePlay();
          return 0;
        case CID_NEXT_BTN:
          self->app_->OnNext();
          return 0;
        case CID_SHUFFLE_BTN:
          self->app_->OnToggleShuffle();
          return 0;
        case CID_REPEAT_BTN:
          self->app_->OnCycleRepeat();
          return 0;
        case CID_LOGIN_BTN:
          self->app_->OnLogin();
          return 0;
        case CID_LOGOUT_BTN:
          self->app_->OnLogout();
          return 0;
        case IDC_ACC_SEARCH:
          ::SetFocus(self->searchEdit_);
          return 0;
        case IDC_ACC_NEW_PLAYLIST:
          self->app_->OnNewPlaylist();
          return 0;
        case IDC_ACC_REFRESH:
          self->app_->OnRefreshAll();
          return 0;
        case IDC_ACC_PLAY_PAUSE:
          self->app_->OnTogglePlay();
          return 0;
        case IDM_TRAY_SHOW:
          self->app_->OnTrayShow();
          return 0;
        case IDM_TRAY_SETTINGS:
          self->ShowWorkspace(WorkspaceKind::Settings);
          return 0;
        case IDM_TRAY_EXIT:
          self->app_->OnExit();
          return 0;
        default:
          break;
      }
      if (id >= IDM_CTX_ADD_PLAYLIST_BASE && id < IDM_CTX_ADD_PLAYLIST_BASE + 64) {
        self->app_->OnAddToPlaylist(id - IDM_CTX_ADD_PLAYLIST_BASE);
        return 0;
      }
      break;
    }
    case WM_HSCROLL: {
      HWND slider = reinterpret_cast<HWND>(l);
      UINT code = LOWORD(w);
      if (slider == self->seekBar_) {
        int position =
            static_cast<int>(::SendMessageW(self->seekBar_, TBM_GETPOS, 0, 0));
        if (code == TB_THUMBTRACK || code == TB_ENDTRACK) {
          self->seekDragging_ = code == TB_THUMBTRACK;
          std::wstring elapsed = FormatTime(position);
          ::SetWindowTextW(self->elapsedLbl_, elapsed.c_str());
        }
        if (code == TB_ENDTRACK) self->app_->OnSeekTo(position);
      } else if (slider == self->volumeBar_) {
        int volume =
            static_cast<int>(::SendMessageW(self->volumeBar_, TBM_GETPOS, 0, 0));
        if (code == TB_THUMBTRACK || code == TB_ENDTRACK) {
          self->volumeDragging_ = code == TB_THUMBTRACK;
          std::wstring label = std::to_wstring(volume) + L"%";
          ::SetWindowTextW(self->volumeLbl_, label.c_str());
        }
        if (code == TB_ENDTRACK) self->app_->OnSetVolumePercent(volume);
      }
      return 0;
    }
    case WM_CTLCOLORSTATIC: {
      HDC dc = reinterpret_cast<HDC>(w);
      HWND control = reinterpret_cast<HWND>(l);
      ::SetBkMode(dc, TRANSPARENT);
      bool accent = control == self->workspaceTypeLbl_;
      bool bright = control == self->brandLbl_ || control == self->titleLbl_ ||
                    control == self->middleLabel_ ||
                    control == self->settingsTitle_;
      ::SetTextColor(dc, accent ? kAccent : (bright ? kText : kDim));
      bool player = control == self->nowPlayingLbl_ ||
                    control == self->titleLbl_ ||
                    control == self->artistLbl_ ||
                    control == self->albumLbl_ ||
                    control == self->elapsedLbl_ ||
                    control == self->durationLbl_ ||
                    control == self->localControlsLbl_ ||
                    control == self->volumeLbl_;
      bool sidebar = control == self->brandLbl_ ||
                     control == self->libraryGroupLbl_;
      if (player) return reinterpret_cast<LRESULT>(self->brushPlayer_);
      if (sidebar) return reinterpret_cast<LRESULT>(self->brushSidebar_);
      return reinterpret_cast<LRESULT>(self->brushPanel_);
    }
    case WM_CTLCOLOREDIT: {
      HDC dc = reinterpret_cast<HDC>(w);
      ::SetTextColor(dc, kText);
      ::SetBkColor(dc, kEdit);
      return reinterpret_cast<LRESULT>(self->brushEdit_);
    }
    case WM_CTLCOLORLISTBOX: {
      HDC dc = reinterpret_cast<HDC>(w);
      HWND control = reinterpret_cast<HWND>(l);
      ::SetTextColor(dc, kText);
      ::SetBkColor(dc, control == self->playlistList_ ? kSidebar : kControl);
      return reinterpret_cast<LRESULT>(
          control == self->playlistList_ ? self->brushSidebar_
                                         : self->brushControl_);
    }
    case WM_CTLCOLORBTN: {
      HDC dc = reinterpret_cast<HDC>(w);
      ::SetTextColor(dc, kText);
      ::SetBkMode(dc, TRANSPARENT);
      return reinterpret_cast<LRESULT>(self->brushPanel_);
    }
    case WM_SR_RUN: {
      auto* fn = (std::function<void()>*)l;
      (*fn)();
      delete fn;
      return 0;
    }
    case WM_TIMER:
      self->app_->OnTimer((UINT)w);
      return 0;
    case WM_CLOSE:
      self->app_->OnExit();
      return 0;
    case WM_DESTROY: {
      HWND coverControls[] = {self->coverArea_, self->workspaceCover_};
      for (HWND cover : coverControls) {
        if (!cover) continue;
        auto* ctx =
            reinterpret_cast<CoverCtx*>(::GetWindowLongPtrW(cover, GWLP_USERDATA));
        if (ctx) {
          delete ctx->img;
          delete ctx;
        }
      }
      delete self->artworkCache_;
      self->artworkCache_ = nullptr;
      if (self->rowHeightImageList_) ::ImageList_Destroy(self->rowHeightImageList_);
      if (self->brushBg_) ::DeleteObject(self->brushBg_);
      if (self->brushSidebar_) ::DeleteObject(self->brushSidebar_);
      if (self->brushPanel_) ::DeleteObject(self->brushPanel_);
      if (self->brushEdit_) ::DeleteObject(self->brushEdit_);
      if (self->brushControl_) ::DeleteObject(self->brushControl_);
      if (self->brushPlayer_) ::DeleteObject(self->brushPlayer_);
      if (self->fontUi_) ::DeleteObject(self->fontUi_);
      if (self->fontList_) ::DeleteObject(self->fontList_);
      if (self->fontRowTitle_) ::DeleteObject(self->fontRowTitle_);
      if (self->fontTitle_) ::DeleteObject(self->fontTitle_);
      if (self->fontDisplay_) ::DeleteObject(self->fontDisplay_);
      if (self->fontSmall_) ::DeleteObject(self->fontSmall_);
      if (self->fontIcon16_) ::DeleteObject(self->fontIcon16_);
      if (self->fontIcon20_) ::DeleteObject(self->fontIcon20_);
      if (self->fontIcon24_) ::DeleteObject(self->fontIcon24_);
      if (self->fontIcon40_) ::DeleteObject(self->fontIcon40_);
      ::PostQuitMessage(0);
      return 0;
    }
  }
  return ::DefWindowProcW(h, m, w, l);
}

void MainWindow::FillList(HWND list, const std::vector<ListRow>& rows) {
  ::SendMessageW(list, WM_SETREDRAW, FALSE, 0);
  ::SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
  ::SetWindowTextW(list, list == resultsList_ ? L"Search results"
                                              : L"Collection tracks");
  for (size_t i = 0; i < rows.size(); ++i) {
    LVITEMW item{};
    item.mask = LVIF_TEXT;
    item.iItem = static_cast<int>(i);
    item.pszText = const_cast<LPWSTR>(rows[i].accessibleText.c_str());
    ::SendMessageW(list, LVM_INSERTITEMW, 0,
                   reinterpret_cast<LPARAM>(&item));
  }
  ::SendMessageW(list, WM_SETREDRAW, TRUE, 0);
  ::InvalidateRect(list, nullptr, TRUE);
}

void MainWindow::SetSearchResults(const SearchResult& result) {
  search_ = result;
  resultsLoading_ = false;
  resultKinds_.clear();
  searchRows_.clear();
  searchRows_.reserve(result.tracks.size() + result.albums.size() +
                      result.artists.size());
  for (const auto& track : result.tracks) {
    resultKinds_.push_back(0);
    searchRows_.push_back(MakeTrackRow(track));
  }
  for (const auto& album : result.albums) {
    resultKinds_.push_back(1);
    searchRows_.push_back(MakeAlbumRow(album));
  }
  for (const auto& artist : result.artists) {
    resultKinds_.push_back(2);
    searchRows_.push_back(MakeArtistRow(artist));
  }
  resultsEmptyTitle_ = L"No results";
  resultsEmptyDetail_ = L"Try a different title, artist, or album.";
  FillList(resultsList_, searchRows_);
  RequestArtwork(searchRows_);
}

void MainWindow::SetMiddleTracks(const std::vector<TrackRef>& tracks) {
  middleTracks_ = tracks;
  middleLoading_ = false;
  middleRows_.clear();
  middleRows_.reserve(tracks.size());
  workspaceDurationMs_ = 0;
  for (size_t i = 0; i < tracks.size(); ++i) {
    middleRows_.push_back(MakeTrackRow(tracks[i], i + 1));
    workspaceDurationMs_ += std::max(0, tracks[i].duration_ms);
  }
  middleEmptyTitle_ = L"No tracks here";
  middleEmptyDetail_ =
      L"This collection does not contain any playable tracks.";
  FillList(tracksList_, middleRows_);
  RequestArtwork(middleRows_);
  std::string artwork =
      tracks.empty() ? std::string{} : tracks.front().cover_url;
  if (collectionKind_ == CollectionKind::Playlist &&
      selectedMiddleIndex_ > 0 &&
      static_cast<size_t>(selectedMiddleIndex_ - 1) < playlists_.size() &&
      !playlists_[selectedMiddleIndex_ - 1].cover_url.empty())
    artwork = playlists_[selectedMiddleIndex_ - 1].cover_url;
  UpdateWorkspaceArtwork(artwork);
  UpdateWorkspaceHeader();
  if (workspaceKind_ != WorkspaceKind::Settings)
    ShowWorkspace(WorkspaceKind::Collection);
}

void MainWindow::SetArtistPage(const ArtistRef& artist,
                               const std::vector<TrackRef>& tracks) {
  collectionKind_ = CollectionKind::Artist;
  workspaceTitle_ =
      artist.name.empty() ? L"Artist" : Utf8ToWide(artist.name);
  middleTracks_ = tracks;
  middleLoading_ = false;
  middleRows_.clear();
  middleRows_.reserve(tracks.size());
  workspaceDurationMs_ = 0;
  for (size_t i = 0; i < tracks.size(); ++i) {
    middleRows_.push_back(MakeTrackRow(tracks[i], i + 1));
    workspaceDurationMs_ += std::max(0, tracks[i].duration_ms);
  }
  middleEmptyTitle_ = L"No top tracks";
  middleEmptyDetail_ = L"Spotify did not return top tracks for this artist.";
  FillList(tracksList_, middleRows_);
  RequestArtwork(middleRows_);
  UpdateWorkspaceArtwork(artist.cover_url);
  UpdateWorkspaceHeader();
  if (workspaceKind_ != WorkspaceKind::Settings)
    ShowWorkspace(WorkspaceKind::Collection);
}

void MainWindow::SetQueueTracks(const std::vector<TrackRef>& tracks) {
  collectionKind_ = CollectionKind::Queue;
  selectedMiddleIndex_ = 0;
  workspaceTitle_ = L"Queue";
  middleTracks_ = tracks;
  middleLoading_ = false;
  middleRows_.clear();
  middleRows_.reserve(tracks.size());
  workspaceDurationMs_ = 0;
  for (size_t i = 0; i < tracks.size(); ++i) {
    middleRows_.push_back(MakeTrackRow(tracks[i], i + 1));
    workspaceDurationMs_ += std::max(0, tracks[i].duration_ms);
  }
  middleEmptyTitle_ = L"Queue is empty";
  middleEmptyDetail_ = L"Add a search result to build the local queue.";
  FillList(tracksList_, middleRows_);
  RequestArtwork(middleRows_);
  UpdateWorkspaceArtwork(tracks.empty() ? std::string{}
                                        : tracks.front().cover_url);
  RebuildPlaylistRail();
  UpdateWorkspaceHeader();
}

void MainWindow::SetPlaylists(const std::vector<PlaylistRef>& pls) {
  playlists_ = pls;
  selectedMiddleIndex_ =
      std::clamp(selectedMiddleIndex_, 0, static_cast<int>(playlists_.size()));
  if (collectionKind_ == CollectionKind::Playlist &&
      selectedMiddleIndex_ > 0)
    workspaceTitle_ = Utf8ToWide(playlists_[selectedMiddleIndex_ - 1].name);
  ::SendMessageW(middleCombo_, CB_RESETCONTENT, 0, 0);
  ::SendMessageW(middleCombo_, CB_ADDSTRING, 0,
                 reinterpret_cast<LPARAM>(L"Queue"));
  for (const auto& playlist : playlists_)
    ::SendMessageW(
        middleCombo_, CB_ADDSTRING, 0,
        reinterpret_cast<LPARAM>(Utf8ToWide(playlist.name).c_str()));
  ::SendMessageW(middleCombo_, CB_SETCURSEL, selectedMiddleIndex_, 0);
  RebuildPlaylistRail();
  UpdateWorkspaceHeader();
}
void MainWindow::SetMiddleLabel(const std::wstring& text) {
  workspaceTitle_ = text;
  if (text == L"Queue") {
    collectionKind_ = CollectionKind::Queue;
  }
  UpdateWorkspaceHeader();
}

void MainWindow::SetPlayback(const PlaybackEngineState& playback) {
  // Capture what actually changed before overwriting the stored state so
  // timer-driven updates repaint controls only when their visuals change.
  const bool shuffleChanged = playback_.shuffle != playback.shuffle;
  const bool repeatChanged = playback_.repeat != playback.repeat;
  const bool playingChanged = playback_.playing != playback.playing;
  playback_ = playback;
  const TrackRef* track = nullptr;
  if (playback.current_index >= 0 &&
      playback.current_index < static_cast<int>(playback.queue.size()))
    track = &playback.queue[playback.current_index];

  if (!track) {
    SetTextIfChanged(titleLbl_, L"Nothing playing");
    SetTextIfChanged(artistLbl_, L"Choose a track to begin");
    SetTextIfChanged(albumLbl_, L"");
  } else {
    SetTextIfChanged(titleLbl_, Utf8ToWide(track->name).c_str());
    SetTextIfChanged(artistLbl_, JoinArtists(track->artist_names).c_str());
    SetTextIfChanged(albumLbl_, Utf8ToWide(track->album_name).c_str());
  }
  if (shuffleChanged) {
    ::SetWindowTextW(shuffleBtn_,
                     playback.shuffle ? L"Shuffle on" : L"Shuffle off");
    ::InvalidateRect(shuffleBtn_, nullptr, FALSE);
  }
  SetTooltipText(shuffleBtn_, playback.shuffle ? L"Shuffle: on"
                                               : L"Shuffle: off");
  std::wstring repeat = L"Repeat off";
  std::wstring repeatTip = L"Repeat: off";
  if (playback.repeat == "context") {
    repeat = L"Repeat all";
    repeatTip = L"Repeat: all";
  }
  if (playback.repeat == "track") {
    repeat = L"Repeat one";
    repeatTip = L"Repeat: one";
  }
  if (repeatChanged) {
    ::SetWindowTextW(repeatBtn_, repeat.c_str());
    ::InvalidateRect(repeatBtn_, nullptr, FALSE);
  }
  SetTooltipText(repeatBtn_, repeatTip);

  const BOOL available = playback.ready ? TRUE : FALSE;
  ::EnableWindow(prevBtn_, available);
  ::EnableWindow(playBtn_, available);
  ::EnableWindow(nextBtn_, available);
  ::EnableWindow(shuffleBtn_, available);
  ::EnableWindow(repeatBtn_, available);
  // Session controls: Log in exactly when the engine publishes a fresh
  // authorize URL, Log out while a session is live (both disabled while a
  // flow is in flight, so no double-submit).
  ::EnableWindow(loginBtn_, LoginButtonEnabled(playback) ? TRUE : FALSE);
  ::EnableWindow(logoutBtn_, LogoutButtonEnabled(playback) ? TRUE : FALSE);
  if (playingChanged) {
    ::SetWindowTextW(playBtn_, playback.playing ? L"Pause" : L"Play");
    ::InvalidateRect(playBtn_, nullptr, FALSE);
  }

  const int duration = static_cast<int>(std::clamp<int64_t>(
      playback.duration_ms, 0, std::numeric_limits<int>::max()));
  const int position = static_cast<int>(std::clamp<int64_t>(
      playback.position_ms, 0, static_cast<int64_t>(duration)));
  ::SendMessageW(seekBar_, TBM_SETRANGEMIN, FALSE, 0);
  ::SendMessageW(seekBar_, TBM_SETRANGEMAX, TRUE, std::max(1, duration));
  if (!seekDragging_)
    ::SendMessageW(seekBar_, TBM_SETPOS, TRUE, position);
  ::EnableWindow(seekBar_, playback.ready && duration > 0);
  if (!seekDragging_)
    SetTextIfChanged(elapsedLbl_, FormatTime(position).c_str());
  SetTextIfChanged(durationLbl_, FormatTime(duration).c_str());

  ::EnableWindow(volumeBar_, available);
  if (!volumeDragging_) {
    const int volume = std::clamp(playback.volume_percent, 0, 100);
    ::SendMessageW(volumeBar_, TBM_SETPOS, TRUE, volume);
    SetTextIfChanged(volumeLbl_,
                     (std::to_wstring(volume) + L"%").c_str());
    SetTooltipText(volumeBar_, L"Playback volume: " +
                                   std::to_wstring(volume) + L"%");
  }
}

void MainWindow::SetStatus(const std::wstring& text) {
  ::SetWindowTextW(statusLbl_, text.c_str());
  auto startsWith = [&text](const wchar_t* prefix) {
    return text.rfind(prefix, 0) == 0;
  };
  if (resultsLoading_ && startsWith(L"Search:")) {
    resultsLoading_ = false;
    searchRows_.clear();
    SetListMessage(resultsList_, L"Search unavailable",
                   L"Check the status below, then try the search again.");
  }
  if (middleLoading_ &&
      (startsWith(L"Album:") || startsWith(L"Artist albums:") ||
       startsWith(L"Album tracks:") || startsWith(L"Playlist tracks:") ||
       startsWith(L"Queue:"))) {
    middleLoading_ = false;
    middleRows_.clear();
    SetListMessage(tracksList_, L"Collection unavailable",
                   L"Check the status below, then retry this collection.");
  }
}

void MainWindow::SetEngineStatus(const std::wstring& text) {
  ::SetWindowTextW(engineStatusLbl_, text.c_str());
}

void MainWindow::SetCacheUsage(const std::wstring& text) {
  if (!cacheStatusLbl_) return;
  ::SetWindowTextW(cacheStatusLbl_, text.c_str());
}


void MainWindow::SetCoverFile(const std::wstring& path) {
  if (!coverArea_ || !::IsWindow(coverArea_)) return;
  ::SendMessageW(coverArea_, WM_SR_COVER_LOAD, 0,
                 reinterpret_cast<LPARAM>(_wcsdup(path.c_str())));
}

void MainWindow::SetTrackArtwork(const std::string& url,
                                 const std::wstring& path) {
  if (!artworkCache_ || url.empty() || path.empty()) return;
  auto setWorkspaceImage = [this, &url](Gdiplus::Image* image) {
    if (url != workspaceArtworkUrl_ || !workspaceCover_ || !image) return;
    auto* context = reinterpret_cast<CoverCtx*>(
        ::GetWindowLongPtrW(workspaceCover_, GWLP_USERDATA));
    if (!context) return;
    delete context->img;
    context->img = image->Clone();
    ::InvalidateRect(workspaceCover_, nullptr, TRUE);
  };
  auto existing = artworkCache_->images.find(url);
  if (existing != artworkCache_->images.end()) {
    setWorkspaceImage(existing->second.get());
    if (playlistList_) ::InvalidateRect(playlistList_, nullptr, FALSE);
    return;
  }
  std::unique_ptr<Gdiplus::Image> source(
      Gdiplus::Image::FromFile(path.c_str()));
  if (!source || source->GetLastStatus() != Gdiplus::Ok ||
      source->GetWidth() == 0 || source->GetHeight() == 0)
    return;

  constexpr int kThumbnailPixels = 192;
  auto thumbnail =
      std::make_unique<Gdiplus::Bitmap>(kThumbnailPixels, kThumbnailPixels);
  Gdiplus::Graphics graphics(thumbnail.get());
  graphics.SetInterpolationMode(Gdiplus::InterpolationModeHighQualityBicubic);
  UINT sourceWidth = source->GetWidth();
  UINT sourceHeight = source->GetHeight();
  UINT sourceSize = std::min(sourceWidth, sourceHeight);
  graphics.DrawImage(
      source.get(), Gdiplus::Rect(0, 0, kThumbnailPixels, kThumbnailPixels),
      (sourceWidth - sourceSize) / 2, (sourceHeight - sourceSize) / 2,
      sourceSize, sourceSize, Gdiplus::UnitPixel);
  if (graphics.GetLastStatus() != Gdiplus::Ok) return;
  if (artworkCache_->images.size() >= 128) {
    auto victim = artworkCache_->images.begin();
    artworkCache_->requested.erase(victim->first);
    artworkCache_->images.erase(victim);
  }
  Gdiplus::Image* workspaceImage = thumbnail.get();
  artworkCache_->images[url] = std::move(thumbnail);
  setWorkspaceImage(workspaceImage);
  if (resultsList_) ::InvalidateRect(resultsList_, nullptr, FALSE);
  if (tracksList_) ::InvalidateRect(tracksList_, nullptr, FALSE);
  if (playlistList_) ::InvalidateRect(playlistList_, nullptr, FALSE);
}


void MainWindow::SetMiddleMode(int modeIndex) {
  selectedMiddleIndex_ =
      std::clamp(modeIndex, 0, static_cast<int>(playlists_.size()));
  ::SendMessageW(middleCombo_, CB_SETCURSEL, selectedMiddleIndex_, 0);
  RebuildPlaylistRail();
}


void MainWindow::SetDemo() {
  demoMode_ = true;
  SearchResult result;
  TrackRef first;
  first.name = "First Track";
  first.artist_names = {"Demo Artist"};
  first.uri = "spotify:track:demo1";
  first.album_name = "Demo Album";
  first.album_id = "demo-album";
  first.artist_id = "demo-artist";
  first.duration_ms = 214000;
  TrackRef second;
  second.name = "Graphite Afterglow";
  second.artist_names = {"Demo Artist", "Night Transit"};
  second.album_name = "Warm Signals";
  second.duration_ms = 187000;
  second.uri = "spotify:track:demo2";
  second.album_id = "warm-signals";
  second.artist_id = "demo-artist";
  TrackRef third;
  third.name = "Quiet Geometry";
  third.artist_names = {"Night Transit"};
  third.album_name = "Warm Signals";
  third.duration_ms = 242000;
  third.uri = "spotify:track:demo3";
  third.album_id = "warm-signals";
  third.artist_id = "night-transit";
  demoTracks_ = {first, second, third};
  result.tracks = {first, second};
  AlbumRef album;
  album.name = "Demo Album";
  album.artist_names = {"Demo Artist"};
  album.uri = "spotify:album:demo1";
  result.albums.push_back(album);
  ArtistRef artist;
  artist.name = "Demo Artist";
  artist.uri = "spotify:artist:demo1";
  result.artists.push_back(artist);
  SetSearchResults(result);

  PlaylistRef playlist;
  playlist.name = "Demo playlist";
  playlist.id = "demo";
  playlist.uri = "spotify:playlist:demo";
  playlist.owner = "Demo Listener";
  playlist.tracks_total = 3;
  SetPlaylists({playlist});
  SelectPlaylistRow(1, false);
  SetMiddleTracks(demoTracks_);
  SetMiddleLabel(L"Demo playlist");

  PlaybackEngineState state;
  state.ready = true;
  state.auth_state = EngineAuthState::Ready;
  state.playing = true;
  state.position_ms = 61000;
  state.duration_ms = first.duration_ms;
  state.volume_percent = 68;
  state.queue = demoTracks_;
  state.current_index = 0;
  state.current_uri = first.uri;
  SetPlayback(state);
  SetEngineStatus(
      L"Standalone engine: authenticated · Ogg Vorbis 320 kbps · cache limit 1 GiB");
  SetStatus(L"Demo data (no network or engine process)");
}

std::string MainWindow::SearchQuery() const {
  char buf[512] = {};
  ::GetWindowTextA(searchEdit_, buf, 511);
  return buf;
}

int MainWindow::SelectedTrackIndex() const {
  return (int)::SendMessageW(tracksList_, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_SELECTED);
}

int MainWindow::SelectedResultIndex() const {
  return (int)::SendMessageW(resultsList_, LVM_GETNEXTITEM, (WPARAM)-1, LVNI_SELECTED);
}

int MainWindow::MiddleComboIndex() const {
  return selectedMiddleIndex_;
}


}  // namespace sr
