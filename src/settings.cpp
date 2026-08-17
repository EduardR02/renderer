#include "settings.h"

#include <windows.h>

#include <nlohmann/json.hpp>

#include "app_paths.h"

namespace sr {

Settings LoadSettings(const std::wstring& path) {
  Settings settings;
  std::string contents;
  if (paths::ReadOwnedFile(path, &contents, 1024 * 1024) !=
      paths::OwnedFileReadResult::Ok)
    return settings;
  const nlohmann::json value =
      nlohmann::json::parse(contents, nullptr, /*allow_exceptions=*/false);
  if (value.is_discarded() || !value.is_object()) return settings;
  if (auto it = value.find("client_id"); it != value.end() && it->is_string())
    settings.client_id = it->get<std::string>();
  if (auto it = value.find("redirect_uri");
      it != value.end() && it->is_string())
    settings.redirect_uri = it->get<std::string>();
  ClampSettings(settings);
  return settings;
}

void ClampSettings(Settings& settings) {
  if (settings.client_id.size() > 256) settings.client_id.resize(256);
  if (settings.redirect_uri.size() > 1024) settings.redirect_uri.resize(1024);
}

bool SaveSettings(const std::wstring& path, const Settings& settings) {
  if (_wcsicmp(paths::Canonical(path).c_str(),
               paths::Canonical(paths::SettingsFile()).c_str()) != 0)
    return false;
  Settings copy = settings;
  ClampSettings(copy);
  const nlohmann::json value = {
      {"client_id", copy.client_id},
      {"redirect_uri", copy.redirect_uri},
  };
  return paths::AtomicWriteOwnedFile(path, value.dump(2));
}

}  // namespace sr
