#include "util.h"

#include <windows.h>
#include <bcrypt.h>

#include <algorithm>
#include <cctype>
#include <stdexcept>
#include <vector>
namespace sr {

std::string WideToUtf8(const std::wstring& w) {
  if (w.empty()) return {};
  int n = ::WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), nullptr, 0, nullptr, nullptr);
  if (n <= 0) return {};
  std::string out((size_t)n, '\0');
  ::WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), out.data(), n, nullptr, nullptr);
  return out;
}

std::wstring Utf8ToWide(const std::string& s) {
  if (s.empty()) return {};
  int n = ::MultiByteToWideChar(CP_UTF8, 0, s.c_str(), (int)s.size(), nullptr, 0);
  if (n <= 0) return {};
  std::wstring out((size_t)n, L'\0');
  ::MultiByteToWideChar(CP_UTF8, 0, s.c_str(), (int)s.size(), out.data(), n);
  return out;
}

std::string Trim(const std::string& s) {
  size_t b = 0, e = s.size();
  while (b < e && std::isspace((unsigned char)s[b])) ++b;
  while (e > b && std::isspace((unsigned char)s[e - 1])) --e;
  return s.substr(b, e - b);
}

std::string ToLower(const std::string& s) {
  std::string out = s;
  std::transform(out.begin(), out.end(), out.begin(),
                 [](unsigned char c) { return (char)std::tolower(c); });
  return out;
}

bool StartsWith(const std::string& s, const std::string& prefix) {
  return s.size() >= prefix.size() && s.compare(0, prefix.size(), prefix) == 0;
}

static bool IsUnreserved(char c) {
  return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
         c == '-' || c == '.' || c == '_' || c == '~';
}

std::string UrlEncode(const std::string& s) {
  static const char* hex = "0123456789ABCDEF";
  std::string out;
  out.reserve(s.size() * 3);
  for (unsigned char c : s) {
    if (IsUnreserved((char)c)) {
      out.push_back((char)c);
    } else {
      out.push_back('%');
      out.push_back(hex[c >> 4]);
      out.push_back(hex[c & 0xF]);
    }
  }
  return out;
}

static std::string Sha1HexImpl(const std::string& s) {
  std::string out;
  BCRYPT_ALG_HANDLE alg = nullptr;
  BCRYPT_HASH_HANDLE hash = nullptr;
  if (::BCryptOpenAlgorithmProvider(&alg, BCRYPT_SHA1_ALGORITHM, nullptr, 0) < 0) return out;
  ULONG hashLen = 0, got = 0;
  ::BCryptGetProperty(alg, BCRYPT_HASH_LENGTH, (PUCHAR)&hashLen, sizeof(hashLen), &got, 0);
  if (hashLen == 0 || ::BCryptCreateHash(alg, &hash, nullptr, 0, nullptr, 0, 0) < 0) {
    ::BCryptCloseAlgorithmProvider(alg, 0);
    return out;
  }
  std::vector<uint8_t> d(hashLen);
  NTSTATUS st = ::BCryptHashData(hash, (PUCHAR)s.data(), (ULONG)s.size(), 0);
  if (st >= 0) st = ::BCryptFinishHash(hash, d.data(), hashLen, 0);
  ::BCryptDestroyHash(hash);
  ::BCryptCloseAlgorithmProvider(alg, 0);
  if (st < 0) return out;
  static const char* hex = "0123456789abcdef";
  out.reserve(hashLen * 2);
  for (uint8_t b : d) {
    out.push_back(hex[b >> 4]);
    out.push_back(hex[b & 0xF]);
  }
  return out;
}

std::string Sha1Hex(const std::string& s) { return Sha1HexImpl(s); }

int64_t NowUnixSeconds() {
  FILETIME ft;
  ::GetSystemTimeAsFileTime(&ft);
  ULARGE_INTEGER u;
  u.LowPart = ft.dwLowDateTime;
  u.HighPart = ft.dwHighDateTime;
  // 100ns intervals since 1601-01-01 → seconds since 1970-01-01.
  return (int64_t)(u.QuadPart / 10000000ULL - 11644473600ULL);
}

namespace {
// Replaces `"<key>" : "<value>"` (any whitespace) with a redacted value.
void ReplaceJsonStringValue(std::string& s, const std::string& key) {
  size_t pos = 0;
  while (true) {
    pos = s.find('"' + key + '"', pos);
    if (pos == std::string::npos) break;
    size_t p = pos + key.size() + 2;
    while (p < s.size() && (s[p] == ' ' || s[p] == '\t')) ++p;
    if (p < s.size() && s[p] == ':') {
      ++p;
      while (p < s.size() && (s[p] == ' ' || s[p] == '\t')) ++p;
      if (p < s.size() && s[p] == '"') {
        size_t e = s.find('"', p + 1);
        if (e != std::string::npos) {
          s.replace(p + 1, e - p - 1, "[redacted]");
          pos = p + 12;
          continue;
        }
      }
    }
    pos = pos + key.size() + 2;
  }
}
}  // namespace

std::string RedactSecrets(std::string s) {
  ReplaceJsonStringValue(s, "access_token");
  ReplaceJsonStringValue(s, "refresh_token");
  ReplaceJsonStringValue(s, "code_verifier");
  ReplaceJsonStringValue(s, "client_secret");
  ReplaceJsonStringValue(s, "code");

  // "Bearer <token>" occurrences.
  {
    std::string lower = ToLower(s);
    size_t pos = 0;
    while ((pos = lower.find("bearer ", pos)) != std::string::npos) {
      size_t b = pos + 7;
      size_t e = b;
      while (e < s.size() && s[e] != ' ' && s[e] != '\t' && s[e] != ',' && s[e] != '"' &&
             s[e] != '\r' && s[e] != '\n')
        ++e;
      if (e > b) {
        s.replace(b, e - b, "[redacted]");
        lower = ToLower(s);
        pos = b + 12;
      } else {
        pos = b;
      }
    }
  }
  // "Authorization: <value>" header occurrences.
  {
    std::string lower = ToLower(s);
    size_t pos = 0;
    while ((pos = lower.find("authorization", pos)) != std::string::npos) {
      size_t p = pos + 13;
      while (p < s.size() && (s[p] == ' ' || s[p] == '\t' || s[p] == ':')) ++p;
      size_t e = p;
      while (e < s.size() && s[e] != '\r' && s[e] != '\n') ++e;
      if (e > p) {
        s.replace(p, e - p, "[redacted]");
        lower = ToLower(s);
        pos = p + 12;
      } else {
        pos = p;
      }
    }
  }
  return s;
}

}  // namespace sr
