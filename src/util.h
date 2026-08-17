#pragma once
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace sr {

std::string WideToUtf8(const std::wstring& w);
std::wstring Utf8ToWide(const std::string& s);

std::string Trim(const std::string& s);
std::string ToLower(const std::string& s);
bool StartsWith(const std::string& s, const std::string& prefix);
bool StartsWithIgnoreCase(const std::string& s, const std::string& prefix);
bool EndsWithIgnoreCase(const std::string& s, const std::string& suffix);
std::vector<std::string> SplitWs(const std::string& s);
std::vector<std::string> Split(const std::string& s, char sep);

// Percent-encoding per RFC 3986 (unreserved characters are kept literal).
std::string UrlEncode(const std::string& s);
std::string UrlDecode(const std::string& s);

using RandomFill = bool (*)(uint8_t* output, size_t size);
using Sha256Digest = bool (*)(const void* data, size_t size,
                             std::vector<uint8_t>* digest);

// Cryptographic helpers throw std::runtime_error on provider failure. Optional
// providers allow deterministic failure-path tests without weakening runtime.
std::vector<uint8_t> Sha256(const void* data, size_t len,
                            Sha256Digest provider = nullptr);
std::string Sha256Base64Url(const std::string& s,
                            Sha256Digest provider = nullptr);
// SHA-1 hex digest (used for cover-art cache file names only).
std::string Sha1Hex(const std::string& s);

// Base64url (RFC 4648 s5), no padding.
std::string Base64UrlEncode(const void* data, size_t len);

// Unix time in seconds.
int64_t NowUnixSeconds();

// Removes bearer tokens, OAuth codes and token JSON values so logs and
// diagnostics never expose credentials.
std::string RedactSecrets(std::string s);

std::vector<uint8_t> RandomBytes(size_t n, RandomFill provider = nullptr);
std::string RandomHex(size_t nBytes, RandomFill provider = nullptr);

}  // namespace sr
