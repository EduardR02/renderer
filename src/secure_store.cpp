#include "secure_store.h"

#include <windows.h>
#include <dpapi.h>

#include <vector>

#include <nlohmann/json.hpp>

#include "app_paths.h"
#include "util.h"

namespace sr {

using nlohmann::json;

namespace {
// Entropy binds the blob to this application.
const wchar_t kEntropy[] = L"SpotifyRenderer.TokenStore.v1";

bool ProtectBlob(const std::string& plain, std::vector<uint8_t>* out) {
  DATA_BLOB in = {(DWORD)plain.size(), (BYTE*)plain.data()};
  DATA_BLOB ent = {sizeof(kEntropy) - sizeof(wchar_t), (BYTE*)kEntropy};
  DATA_BLOB enc = {};
  if (!::CryptProtectData(&in, L"SpotifyRenderer OAuth tokens", &ent, nullptr, nullptr,
                          CRYPTPROTECT_UI_FORBIDDEN, &enc))
    return false;
  out->assign((uint8_t*)enc.pbData, (uint8_t*)enc.pbData + enc.cbData);
  ::LocalFree(enc.pbData);
  return true;
}

bool UnprotectBlob(const std::vector<uint8_t>& enc, std::string* out) {
  DATA_BLOB in = {(DWORD)enc.size(), (BYTE*)enc.data()};
  DATA_BLOB ent = {sizeof(kEntropy) - sizeof(wchar_t), (BYTE*)kEntropy};
  DATA_BLOB plain = {};
  if (!::CryptUnprotectData(&in, nullptr, &ent, nullptr, nullptr, CRYPTPROTECT_UI_FORBIDDEN,
                            &plain)) {
    return false;
  }
  out->assign((const char*)plain.pbData, plain.cbData);
  ::LocalFree(plain.pbData);
  return true;
}
}  // namespace

bool SaveTokenSet(const std::wstring& path, const TokenSet& t) {
  if (_wcsicmp(paths::Canonical(path).c_str(), paths::Canonical(paths::TokensFile()).c_str()) !=
      0)
    return false;
  json j = {{"access_token", t.access_token},
            {"refresh_token", t.refresh_token},
            {"expires_at", t.expires_at}};
  std::string plain = j.dump();
  std::vector<uint8_t> blob;
  if (!ProtectBlob(plain, &blob)) return false;
  return paths::AtomicWriteOwnedFile(path, blob.data(), blob.size());
}

std::optional<TokenSet> LoadTokenSet(const std::wstring& path) {
  if (_wcsicmp(paths::Canonical(path).c_str(), paths::Canonical(paths::TokensFile()).c_str()) !=
      0)
    return std::nullopt;
  std::string bytes;
  if (paths::ReadOwnedFile(path, &bytes, 1024 * 1024) !=
          paths::OwnedFileReadResult::Ok ||
      bytes.empty())
    return std::nullopt;
  std::vector<uint8_t> blob(bytes.begin(), bytes.end());
  std::string plain;
  if (!UnprotectBlob(blob, &plain)) return std::nullopt;
  try {
    json j = json::parse(plain);
    if (!j.is_object()) return std::nullopt;
    TokenSet t;
    if (j.contains("access_token") && j["access_token"].is_string())
      t.access_token = j["access_token"].get<std::string>();
    if (j.contains("refresh_token") && j["refresh_token"].is_string()) {
      t.refresh_token = j["refresh_token"].get<std::string>();
      t.has_refresh = true;
    }
    if (j.contains("expires_at") && j["expires_at"].is_number_integer())
      t.expires_at = j["expires_at"].get<int64_t>();
    if (t.access_token.empty()) return std::nullopt;
    return t;
  } catch (...) {
    return std::nullopt;
  }
}

}  // namespace sr
