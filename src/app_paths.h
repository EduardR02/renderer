#pragma once
#include <cstddef>
#include <optional>
#include <string>

// Production state always lives under %LOCALAPPDATA%\SpotifyRenderer. Tests may
// install a process-local root explicitly; environment variables are ignored.
namespace sr::paths {

std::wstring Root();
void SetTestRootForCurrentProcess(const std::wstring& root);
// Test-only fault injection for the post-rename final-path query.
void SetPostRenameVerificationFailureForTest(bool fail);


std::wstring Canonical(const std::wstring& path);
bool IsPathUnder(const std::wstring& root, const std::wstring& path);
std::optional<std::wstring> Resolve(const std::wstring& leaf);

std::wstring SettingsFile();
std::wstring TokensFile();
std::wstring EngineStateDir();
std::wstring EngineLogFile();
std::wstring LogFile();
std::wstring CoverDir();
std::wstring CoverFile(const std::string& cacheName);

// The engine's audio cache directory (engine state dir + "audio"; matches the
// path the engine builds in create_state).
std::wstring EngineAudioCacheDir();
// Matches the engine's hard-coded AUDIO_CACHE_LIMIT_BYTES.
inline constexpr uint64_t kAudioCacheLimitBytes =
    static_cast<uint64_t>(1024) * 1024 * 1024;
// Recursively sums regular-file sizes under dir, never following reparse
// points. Returns 0 for a missing/unreadable dir or one outside the root.
uint64_t SumFileBytesUnderDir(const std::wstring& dir);

bool EnsureDirs();
// Rejects roots/directories redirected through junctions, symlinks, or other
// reparse points.
bool ValidateOwnedRoot();
bool IsSafeOwnedPath(const std::wstring& path);
enum class OwnedFileReadResult {
  Ok,
  Missing,
  UnsafeOrError,
};

// Reads and appends through the same handle whose attributes and resolved final
// path were verified while its parent directory was held against replacement.
OwnedFileReadResult ReadOwnedFile(const std::wstring& path, std::string* data,
                                  size_t maxSize);
bool AppendOwnedFile(const std::wstring& path, const std::string& data);


// Creates a new non-reparse temporary file and atomically replaces only an
// app-owned regular file. Existing temp symlinks are never opened or followed.
bool AtomicWriteOwnedFile(const std::wstring& path, const void* data, size_t size);
bool AtomicWriteOwnedFile(const std::wstring& path, const std::string& data);
bool DeleteOwnedFile(const std::wstring& path);

}  // namespace sr::paths
