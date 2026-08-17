#include "util.h"

#include <windows.h>
#include <bcrypt.h>

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <ctime>
#include <stdexcept>

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

bool StartsWithIgnoreCase(const std::string& s, const std::string& prefix) {
  return s.size() >= prefix.size() &&
         ToLower(s.substr(0, prefix.size())) == ToLower(prefix);
}

bool EndsWithIgnoreCase(const std::string& s, const std::string& suffix) {
  return s.size() >= suffix.size() &&
         ToLower(s.substr(s.size() - suffix.size())) == ToLower(suffix);
}

std::vector<std::string> SplitWs(const std::string& s) {
  std::vector<std::string> out;
  size_t i = 0;
  while (i < s.size()) {
    while (i < s.size() && std::isspace((unsigned char)s[i])) ++i;
    size_t b = i;
    while (i < s.size() && !std::isspace((unsigned char)s[i])) ++i;
    if (i > b) out.push_back(s.substr(b, i - b));
  }
  return out;
}

std::vector<std::string> Split(const std::string& s, char sep) {
  std::vector<std::string> out;
  size_t b = 0;
  for (size_t i = 0; i <= s.size(); ++i) {
    if (i == s.size() || s[i] == sep) {
      out.push_back(s.substr(b, i - b));
      b = i + 1;
    }
  }
  return out;
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

std::string UrlDecode(const std::string& s) {
  std::string out;
  out.reserve(s.size());
  for (size_t i = 0; i < s.size(); ++i) {
    if (s[i] == '%' && i + 2 < s.size()) {
      auto nib = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
      };
      int hi = nib(s[i + 1]), lo = nib(s[i + 2]);
      if (hi >= 0 && lo >= 0) {
        out.push_back((char)((hi << 4) | lo));
        i += 2;
        continue;
      }
    }
    if (s[i] == '+') out.push_back(' ');
    else out.push_back(s[i]);
  }
  return out;
}

namespace {
bool BCryptSha256(const void* data, size_t len, std::vector<uint8_t>* out) {
  if (!out || len > ULONG_MAX) return false;
  BCRYPT_ALG_HANDLE alg = nullptr;
  BCRYPT_HASH_HANDLE hash = nullptr;
  if (::BCryptOpenAlgorithmProvider(&alg, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0)
    return false;
  ULONG hashLen = 0, got = 0;
  NTSTATUS status = ::BCryptGetProperty(alg, BCRYPT_HASH_LENGTH,
                                        reinterpret_cast<PUCHAR>(&hashLen),
                                        sizeof(hashLen), &got, 0);
  if (status >= 0 && hashLen != 0)
    status = ::BCryptCreateHash(alg, &hash, nullptr, 0, nullptr, 0, 0);
  std::vector<uint8_t> digest;
  if (status >= 0 && hash) {
    digest.resize(hashLen);
    status = ::BCryptHashData(hash, reinterpret_cast<PUCHAR>(const_cast<void*>(data)),
                              static_cast<ULONG>(len), 0);
    if (status >= 0)
      status = ::BCryptFinishHash(hash, digest.data(), hashLen, 0);
  }
  if (hash) ::BCryptDestroyHash(hash);
  ::BCryptCloseAlgorithmProvider(alg, 0);
  if (status < 0 || digest.empty()) return false;
  *out = std::move(digest);
  return true;
}

bool BCryptRandom(uint8_t* output, size_t size) {
  return size <= ULONG_MAX &&
         (size == 0 ||
          ::BCryptGenRandom(nullptr, output, static_cast<ULONG>(size),
                            BCRYPT_USE_SYSTEM_PREFERRED_RNG) >= 0);
}
}  // namespace

std::vector<uint8_t> Sha256(const void* data, size_t len, Sha256Digest provider) {
  std::vector<uint8_t> out;
  if (!(provider ? provider(data, len, &out) : BCryptSha256(data, len, &out)) ||
      out.size() != 32)
    throw std::runtime_error("SHA-256 provider failed");
  return out;
}

static const char kBase64Url[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

std::string Base64UrlEncode(const void* data, size_t len) {
  const uint8_t* p = (const uint8_t*)data;
  std::string out;
  out.reserve((len * 4 + 2) / 3);
  size_t i = 0;
  while (i + 3 <= len) {
    uint32_t v = (uint32_t(p[i]) << 16) | (uint32_t(p[i + 1]) << 8) | p[i + 2];
    out.push_back(kBase64Url[(v >> 18) & 63]);
    out.push_back(kBase64Url[(v >> 12) & 63]);
    out.push_back(kBase64Url[(v >> 6) & 63]);
    out.push_back(kBase64Url[v & 63]);
    i += 3;
  }
  if (i + 1 == len) {
    uint32_t v = uint32_t(p[i]) << 16;
    out.push_back(kBase64Url[(v >> 18) & 63]);
    out.push_back(kBase64Url[(v >> 12) & 63]);
  } else if (i + 2 == len) {
    uint32_t v = (uint32_t(p[i]) << 16) | (uint32_t(p[i + 1]) << 8);
    out.push_back(kBase64Url[(v >> 18) & 63]);
    out.push_back(kBase64Url[(v >> 12) & 63]);
    out.push_back(kBase64Url[(v >> 6) & 63]);
  }
  return out;
}

std::string Sha256Base64Url(const std::string& s, Sha256Digest provider) {
  auto digest = Sha256(s.data(), s.size(), provider);
  return Base64UrlEncode(digest.data(), digest.size());
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

std::vector<uint8_t> RandomBytes(size_t n, RandomFill provider) {
  std::vector<uint8_t> out(n);
  if (!(provider ? provider(out.data(), out.size()) : BCryptRandom(out.data(), out.size())))
    throw std::runtime_error("cryptographic random provider failed");
  return out;
}

std::string RandomHex(size_t nBytes, RandomFill provider) {
  auto bytes = RandomBytes(nBytes, provider);
  static const char* hex = "0123456789abcdef";
  std::string out;
  out.reserve(nBytes * 2);
  for (uint8_t value : bytes) {
    out.push_back(hex[value >> 4]);
    out.push_back(hex[value & 0xF]);
  }
  return out;
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
