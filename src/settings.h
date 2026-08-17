#pragma once
#include <string>

namespace sr {

// Web API OAuth is used only for browsing and library editing. Playback engine
// credentials and cache live independently under the app-owned engine folder.
struct Settings {
  std::string client_id;
  std::string redirect_uri = "http://127.0.0.1:4382/callback";
};

Settings LoadSettings(const std::wstring& path);
bool SaveSettings(const std::wstring& path, const Settings& settings);
void ClampSettings(Settings& settings);

}  // namespace sr
