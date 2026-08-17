#include "tray.h"

#include <vector>

#include "version.h"

namespace sr {

namespace {
void PutBit(std::vector<BYTE>& bits, int rowBytes, int x, int y, bool on) {
  int byte = y * rowBytes + x / 8;
  int bit = 7 - (x % 8);
  if (on) bits[byte] |= (1 << bit);
  else bits[byte] &= ~(1 << bit);
}

bool InTriangle(double x, double y) {
  // Right-pointing triangle inside the unit square.
  double ax = 0.28, ay = 0.20, bx = 0.28, by = 0.80, cx = 0.82, cy = 0.50;
  double d1 = (x - ax) * (by - ay) - (y - ay) * (bx - ax);
  double d2 = (x - bx) * (cy - by) - (y - by) * (cx - bx);
  double d3 = (x - cx) * (ay - cy) - (y - cy) * (ax - cx);
  bool neg = (d1 < 0) || (d2 < 0) || (d3 < 0);
  bool pos = (d1 > 0) || (d2 > 0) || (d3 > 0);
  return !(neg && pos);
}
}  // namespace

HICON MakeAppIcon(int size) {
  int rowBytes = ((size + 15) / 16) * 2;
  std::vector<BYTE> andBits((size_t)rowBytes * size, 0xFF);  // all transparent
  std::vector<BYTE> xorBits((size_t)rowBytes * size, 0);
  for (int y = 0; y < size; ++y) {
    for (int x = 0; x < size; ++x) {
      bool opaque = false, white = false;
      double u = (double)x / size, v = (double)y / size;
      if (InTriangle(u, v)) {
        opaque = true;
        white = true;
      }
      PutBit(andBits, rowBytes, x, y, !opaque);
      PutBit(xorBits, rowBytes, x, y, white);
    }
  }
  return ::CreateIcon(::GetModuleHandleW(nullptr), size, size, 1, 1, andBits.data(),
                      xorBits.data());
}

bool TrayIcon::Create(HWND owner, const std::wstring& tooltip) {
  owner_ = owner;
  icon_ = MakeAppIcon(GetSystemMetrics(SM_CXSMICON));
  nid_ = {};
  nid_.cbSize = sizeof(nid_);
  nid_.hWnd = owner_;
  nid_.uID = 1;
  nid_.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
  nid_.uCallbackMessage = WM_CALLBACK;
  nid_.hIcon = icon_;
  wcsncpy_s(nid_.szTip, tooltip.c_str(), _TRUNCATE);
  added_ = ::Shell_NotifyIconW(NIM_ADD, &nid_) != FALSE;
  return added_;
}

void TrayIcon::Destroy() {
  if (added_) {
    ::Shell_NotifyIconW(NIM_DELETE, &nid_);
    added_ = false;
  }
  if (icon_) {
    ::DestroyIcon(icon_);
    icon_ = nullptr;
  }
}

void TrayIcon::ShowMenu(std::function<void(HMENU)> prepare) {
  if (!added_) return;
  HMENU menu = ::CreatePopupMenu();
  if (!menu) return;
  prepare(menu);
  POINT pt;
  ::GetCursorPos(&pt);
  ::SetForegroundWindow(owner_);
  UINT cmd = ::TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y,
                              0, owner_, nullptr);
  ::DestroyMenu(menu);
  if (cmd != 0) ::PostMessageW(owner_, WM_COMMAND, MAKEWPARAM(cmd, 0), 0);
}

void TrayIcon::SetTooltip(const std::wstring& text) {
  if (!added_) return;
  wcsncpy_s(nid_.szTip, text.c_str(), _TRUNCATE);
  nid_.uFlags = NIF_TIP;
  ::Shell_NotifyIconW(NIM_MODIFY, &nid_);
}

}  // namespace sr
