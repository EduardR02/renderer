#pragma once
#include <cstdint>
#include <optional>
#include <string>

namespace sr {

// Spotify OAuth tokens, protected at rest with Windows DPAPI (user scope).
struct TokenSet {
  std::string access_token;
  std::string refresh_token;
  int64_t expires_at = 0;  // unix seconds; valid until expires_at (skew already applied)
  bool has_refresh = false;
  bool Expired(int64_t now) const { return now >= expires_at; }
  bool HasAccess() const { return !access_token.empty() && !Expired(0); }
};

// Both write DPAPI-encrypted data to `path` (app-owned storage only).
bool SaveTokenSet(const std::wstring& path, const TokenSet& t);
std::optional<TokenSet> LoadTokenSet(const std::wstring& path);

}  // namespace sr
