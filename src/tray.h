#pragma once
#include <functional>
#include <string>

#include <windows.h>
#include <shellapi.h>

namespace sr {

// Runtime-generated monochrome icon (dark tile + white play glyph).
// No .ico assets are embedded; the icon is built from 1bpp masks.
HICON MakeAppIcon(int size);

class TrayIcon {
 public:
  static constexpr UINT WM_CALLBACK = WM_APP + 40;

  bool Create(HWND owner, const std::wstring& tooltip);
  void Destroy();
  bool Created() const { return added_; }

  // Builds a popup menu (prepare adds items), tracks it, and routes the
  // selection back to the owner as WM_COMMAND.
  void ShowMenu(std::function<void(HMENU)> prepare);

  void SetTooltip(const std::wstring& text);
  HICON Icon() const { return icon_; }

 private:
  HWND owner_ = nullptr;
  NOTIFYICONDATAW nid_{};
  HICON icon_ = nullptr;
  bool added_ = false;
};

}  // namespace sr
