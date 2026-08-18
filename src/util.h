#pragma once
#include <cstdint>
#include <string>

namespace sr {

std::string WideToUtf8(const std::wstring& w);
std::wstring Utf8ToWide(const std::string& s);

std::string Trim(const std::string& s);
std::string ToLower(const std::string& s);
bool StartsWith(const std::string& s, const std::string& prefix);

// Percent-encoding per RFC 3986 (unreserved characters are kept literal).
std::string UrlEncode(const std::string& s);

// SHA-1 hex digest (used for cover-art cache file names only).
std::string Sha1Hex(const std::string& s);

// Unix time in seconds.
int64_t NowUnixSeconds();

// Removes bearer tokens and token JSON values so logs and diagnostics never
// expose credentials.
std::string RedactSecrets(std::string s);

}  // namespace sr
