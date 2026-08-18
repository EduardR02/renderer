#pragma once
#include <windows.h>

// Application-specific window messages.
enum {
  // LPARAM = std::function<void()>* allocated with new; handler runs + deletes.
  WM_SR_RUN = WM_APP + 1,
  // LPARAM = wchar_t* (cover file path); handler owns it.
  WM_SR_COVER_LOAD = WM_APP + 3,
};

// Tray menu / context menu command ids.
enum {
  IDM_TRAY_SHOW = 40001,
  IDM_TRAY_SETTINGS = 40002,
  IDM_TRAY_EXIT = 40003,

  IDM_CTX_PLAY_TRACK = 40101,
  IDM_CTX_ADD_QUEUE = 40102,
  IDM_CTX_ARTIST_ALBUMS = 40103,
  IDM_CTX_OPEN_ALBUM = 40104,
  IDM_CTX_ADD_PLAYLIST_BASE = 40110,  // + index into playlist submenu

  IDM_CTX_PLAY_MIDDLE = 40201,
  IDM_CTX_MIDDLE_ADD_QUEUE = 40202,
  IDM_CTX_MIDDLE_REMOVE = 40203,
  IDM_CTX_MIDDLE_UP = 40204,
  IDM_CTX_MIDDLE_DOWN = 40205,
};

// Accelerator command ids.
enum {
  IDC_ACC_SEARCH = 40301,
  IDC_ACC_NEW_PLAYLIST = 40302,
  IDC_ACC_REFRESH = 40303,
  IDC_ACC_PLAY_PAUSE = 40304,
};
