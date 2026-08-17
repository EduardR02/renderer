#include "app_paths.h"

#include <windows.h>
#include <shlobj.h>

#include <algorithm>

#include <atomic>
#include <mutex>
#include <limits>
#include <vector>
#include <cstring>



#include "version.h"

namespace sr::paths {
namespace {

std::mutex g_root_mutex;
std::wstring g_test_root;
std::atomic<uint64_t> g_temp_sequence{0};
std::atomic<bool> g_fail_post_rename_verification_for_test{false};

bool IsValidLeaf(const std::wstring& leaf) {
  if (leaf.empty() || leaf.size() > 200) return false;
  for (wchar_t c : leaf) {
    bool valid = (c >= L'a' && c <= L'z') || (c >= L'A' && c <= L'Z') ||
                 (c >= L'0' && c <= L'9') || c == L'.' || c == L'_' || c == L'-';
    if (!valid) return false;
  }
  return leaf != L"." && leaf != L".." && leaf.find(L"..") == std::wstring::npos;
}

std::wstring StripDevicePrefix(std::wstring value) {
  if (value.rfind(L"\\\\?\\UNC\\", 0) == 0)
    return L"\\\\" + value.substr(8);
  if (value.rfind(L"\\\\?\\", 0) == 0) return value.substr(4);
  return value;
}

class ScopedHandle {
 public:
  explicit ScopedHandle(HANDLE value = INVALID_HANDLE_VALUE) : value_(value) {}
  ~ScopedHandle() {
    if (value_ != INVALID_HANDLE_VALUE) ::CloseHandle(value_);
  }
  ScopedHandle(const ScopedHandle&) = delete;
  ScopedHandle& operator=(const ScopedHandle&) = delete;
  ScopedHandle(ScopedHandle&& other) noexcept : value_(other.release()) {}
  ScopedHandle& operator=(ScopedHandle&& other) noexcept {
    if (this != &other) {
      if (value_ != INVALID_HANDLE_VALUE) ::CloseHandle(value_);
      value_ = other.release();
    }
    return *this;
  }

  HANDLE get() const { return value_; }
  HANDLE release() {
    HANDLE value = value_;
    value_ = INVALID_HANDLE_VALUE;
    return value;
  }

 private:
  HANDLE value_;
};

bool HandleResolvesTo(HANDLE handle, const std::wstring& expected) {
  std::wstring finalPath(32768, L'\0');
  DWORD length = ::GetFinalPathNameByHandleW(handle, finalPath.data(),
                                             static_cast<DWORD>(finalPath.size()),
                                             FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
  if (length == 0 || length >= finalPath.size()) return false;
  finalPath.resize(length);
  std::wstring actual = Canonical(StripDevicePrefix(std::move(finalPath)));
  std::wstring canonicalExpected = Canonical(expected);
  return !actual.empty() && !canonicalExpected.empty() &&
         _wcsicmp(actual.c_str(), canonicalExpected.c_str()) == 0;
}

bool HandleIsRegularFile(HANDLE handle) {
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  return ::GetFileInformationByHandleEx(handle, FileAttributeTagInfo, &attributes,
                                        sizeof(attributes)) &&
         !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) &&
         !(attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT);
}

HANDLE OpenVerifiedDirectory(const std::wstring& directory) {
  HANDLE handle = ::CreateFileW(
      directory.c_str(), FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
      FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                                OPEN_EXISTING,
                                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                                nullptr);
  if (handle == INVALID_HANDLE_VALUE) return INVALID_HANDLE_VALUE;
  FILE_ATTRIBUTE_TAG_INFO attributes{};
  if (!::GetFileInformationByHandleEx(handle, FileAttributeTagInfo, &attributes,
                                      sizeof(attributes)) ||
      !(attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
      (attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
      !HandleResolvesTo(handle, directory)) {
    ::CloseHandle(handle);
    return INVALID_HANDLE_VALUE;
  }
  return handle;
}

bool IsDirectoryWithoutReparse(const std::wstring& directory) {
  ScopedHandle handle(OpenVerifiedDirectory(directory));
  return handle.get() != INVALID_HANDLE_VALUE;
}

bool CreateSafeDirectory(const std::wstring& directory) {
  if (!::CreateDirectoryW(directory.c_str(), nullptr) &&
      ::GetLastError() != ERROR_ALREADY_EXISTS)
    return false;
  return IsDirectoryWithoutReparse(directory);
}

std::wstring Parent(const std::wstring& path) {
  size_t separator = path.find_last_of(L"\\/");
  return separator == std::wstring::npos ? std::wstring() : path.substr(0, separator);
}

std::wstring Leaf(const std::wstring& path) {
  size_t separator = path.find_last_of(L"\\/");
  return separator == std::wstring::npos ? path : path.substr(separator + 1);
}

struct OwnedPathLocks {
  ScopedHandle root;
  ScopedHandle parent;
};

bool PrepareOwnedPath(const std::wstring& path, std::wstring* canonical,
                      OwnedPathLocks* locks) {
  std::wstring root = Root();
  std::wstring candidate = Canonical(path);
  if (root.empty() || candidate.empty() || !IsPathUnder(root, candidate))
    return false;
  std::wstring parent = Parent(candidate);
  if (parent.empty() || Leaf(candidate).empty()) return false;
  ScopedHandle verifiedRoot(OpenVerifiedDirectory(root));
  if (verifiedRoot.get() == INVALID_HANDLE_VALUE) return false;
  ScopedHandle verifiedParent(OpenVerifiedDirectory(parent));
  if (verifiedParent.get() == INVALID_HANDLE_VALUE) return false;
  *canonical = std::move(candidate);
  locks->root = std::move(verifiedRoot);
  locks->parent = std::move(verifiedParent);
  return true;
}

bool MissingError(DWORD error) {
  return error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND;
}

HANDLE OpenVerifiedRegularFile(const std::wstring& path, DWORD access,
                               DWORD share, DWORD creation, DWORD flags,
                               DWORD* error) {
  HANDLE file = ::CreateFileW(path.c_str(), access, share, nullptr, creation,
                              flags | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
  if (file == INVALID_HANDLE_VALUE) {
    if (error) *error = ::GetLastError();
    return INVALID_HANDLE_VALUE;
  }
  if (!HandleIsRegularFile(file) || !HandleResolvesTo(file, path)) {
    ::CloseHandle(file);
    if (error) *error = ERROR_ACCESS_DENIED;
    return INVALID_HANDLE_VALUE;
  }
  if (error) *error = ERROR_SUCCESS;
  return file;
}

bool ExistingRegularFileIsSafe(const std::wstring& path) {
  DWORD error = ERROR_SUCCESS;
  ScopedHandle file(OpenVerifiedRegularFile(
      path, FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE,
      OPEN_EXISTING, 0, &error));
  return file.get() != INVALID_HANDLE_VALUE || MissingError(error);
}

}  // namespace

std::wstring Canonical(const std::wstring& path) {
  if (path.empty()) return {};
  std::wstring full(32768, L'\0');
  DWORD length = ::GetFullPathNameW(path.c_str(), static_cast<DWORD>(full.size()),
                                    full.data(), nullptr);
  if (length == 0 || length >= full.size()) return {};
  full.resize(length);

  while (full.size() > 3 && (full.back() == L'\\' || full.back() == L'/')) full.pop_back();
  return full;
}
void SetPostRenameVerificationFailureForTest(bool fail) {
  g_fail_post_rename_verification_for_test.store(fail);
}

void SetTestRootForCurrentProcess(const std::wstring& root) {
  std::lock_guard<std::mutex> lock(g_root_mutex);
  g_test_root = Canonical(root);
}

std::wstring Root() {
  {
    std::lock_guard<std::mutex> lock(g_root_mutex);
    if (!g_test_root.empty()) return g_test_root;
  }
  PWSTR known = nullptr;
  if (FAILED(::SHGetKnownFolderPath(FOLDERID_LocalAppData, KF_FLAG_DEFAULT, nullptr, &known)))
    return {};
  std::wstring result = Canonical(std::wstring(known) + L"\\" SR_APP_DIR_NAME);
  ::CoTaskMemFree(known);
  return result;
}

bool IsPathUnder(const std::wstring& root, const std::wstring& path) {
  std::wstring base = Canonical(root);
  std::wstring candidate = Canonical(path);
  if (base.empty() || candidate.empty()) return false;
  if (candidate.size() == base.size()) return _wcsicmp(candidate.c_str(), base.c_str()) == 0;
  return candidate.size() > base.size() &&
         _wcsnicmp(candidate.c_str(), base.c_str(), base.size()) == 0 &&
         candidate[base.size()] == L'\\';
}

std::optional<std::wstring> Resolve(const std::wstring& leaf) {
  if (!IsValidLeaf(leaf)) return std::nullopt;
  std::wstring result = Root() + L"\\" + leaf;
  return IsPathUnder(Root(), result) ? std::optional<std::wstring>(std::move(result))
                                    : std::nullopt;
}

std::wstring SettingsFile() { return Resolve(L"settings.json").value_or(L""); }
std::wstring TokensFile() { return Resolve(L"tokens.dat").value_or(L""); }
std::wstring EngineStateDir() { return Root() + L"\\engine"; }
std::wstring LogDir() { return Root() + L"\\logs"; }
std::wstring EngineLogFile() { return LogDir() + L"\\playback_engine.log"; }
std::wstring LogFile() { return LogDir() + L"\\spotify_renderer.log"; }
std::wstring CoverDir() { return Root() + L"\\covers"; }

std::wstring CoverFile(const std::string& cacheName) {
  std::wstring leaf;
  for (unsigned char c : cacheName) {
    bool valid = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                 (c >= '0' && c <= '9') || c == '.' || c == '_' || c == '-';
    if (!valid) return {};
    leaf.push_back(static_cast<wchar_t>(c));
  }
  if (!IsValidLeaf(leaf)) return {};
  std::wstring result = CoverDir() + L"\\" + leaf;
  return IsPathUnder(Root(), result) ? result : std::wstring();
}

bool ValidateOwnedRoot() { return IsDirectoryWithoutReparse(Root()); }

bool EnsureDirs() {
  std::wstring root = Root();
  if (root.empty() || !CreateSafeDirectory(root)) return false;
  return CreateSafeDirectory(LogDir()) && CreateSafeDirectory(CoverDir()) &&
         CreateSafeDirectory(EngineStateDir());
}

bool IsSafeOwnedPath(const std::wstring& path) {
  std::wstring candidate;
  OwnedPathLocks locks;
  return PrepareOwnedPath(path, &candidate, &locks) &&
         ExistingRegularFileIsSafe(candidate);
}

OwnedFileReadResult ReadOwnedFile(const std::wstring& path, std::string* data,
                                  size_t maxSize) {
  if (!data) return OwnedFileReadResult::UnsafeOrError;
  data->clear();
  std::wstring candidate;
  OwnedPathLocks locks;
  if (!PrepareOwnedPath(path, &candidate, &locks))
    return OwnedFileReadResult::UnsafeOrError;

  DWORD error = ERROR_SUCCESS;
  ScopedHandle file(OpenVerifiedRegularFile(
      candidate, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
      OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN, &error));
  if (file.get() == INVALID_HANDLE_VALUE)
    return MissingError(error) ? OwnedFileReadResult::Missing
                               : OwnedFileReadResult::UnsafeOrError;

  LARGE_INTEGER length{};
  if (!::GetFileSizeEx(file.get(), &length) || length.QuadPart < 0 ||
      static_cast<uint64_t>(length.QuadPart) > maxSize ||
      static_cast<uint64_t>(length.QuadPart) > data->max_size())
    return OwnedFileReadResult::UnsafeOrError;
  data->resize(static_cast<size_t>(length.QuadPart));
  size_t offset = 0;
  while (offset < data->size()) {
    DWORD chunk = static_cast<DWORD>(
        std::min<size_t>(data->size() - offset, std::numeric_limits<DWORD>::max()));
    DWORD read = 0;
    if (!::ReadFile(file.get(), data->data() + offset, chunk, &read, nullptr) ||
        read == 0) {
      data->clear();
      return OwnedFileReadResult::UnsafeOrError;
    }
    offset += read;
  }
  return OwnedFileReadResult::Ok;
}

bool AppendOwnedFile(const std::wstring& path, const std::string& data) {
  if (data.size() > MAXDWORD) return false;
  std::wstring candidate;
  OwnedPathLocks locks;
  if (!PrepareOwnedPath(path, &candidate, &locks)) return false;
  DWORD error = ERROR_SUCCESS;
  ScopedHandle file(OpenVerifiedRegularFile(
      candidate, FILE_APPEND_DATA | FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
      OPEN_ALWAYS, FILE_FLAG_WRITE_THROUGH, &error));
  if (file.get() == INVALID_HANDLE_VALUE) return false;
  DWORD written = 0;
  return (data.empty() ||
          (::WriteFile(file.get(), data.data(), static_cast<DWORD>(data.size()),
                       &written, nullptr) &&
           written == data.size())) &&
         ::FlushFileBuffers(file.get());
}

bool AtomicWriteOwnedFile(const std::wstring& path, const void* data, size_t size) {
  if ((!data && size != 0) || size > MAXDWORD) return false;
  std::wstring candidate;
  OwnedPathLocks locks;
  if (!PrepareOwnedPath(path, &candidate, &locks) ||
      !ExistingRegularFileIsSafe(candidate))
    return false;

  std::wstring leaf = Leaf(candidate);
  std::wstring temporaryLeaf =
      leaf + L"." + std::to_wstring(::GetCurrentProcessId()) + L"." +
      std::to_wstring(g_temp_sequence.fetch_add(1)) + L".tmp";
  if (!IsValidLeaf(temporaryLeaf)) return false;
  std::wstring temporary = Parent(candidate) + L"\\" + temporaryLeaf;
  DWORD error = ERROR_SUCCESS;
  ScopedHandle file(OpenVerifiedRegularFile(
      temporary, GENERIC_WRITE | DELETE, 0, CREATE_NEW,
      FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_WRITE_THROUGH, &error));
  if (file.get() == INVALID_HANDLE_VALUE) return false;

  DWORD written = 0;
  bool ok = (size == 0 ||
             (::WriteFile(file.get(), data, static_cast<DWORD>(size), &written,
                          nullptr) &&
              written == size)) &&
            ::FlushFileBuffers(file.get());
  bool renamed = false;
  if (ok) {
    const DWORD nameBytes =
        static_cast<DWORD>(candidate.size() * sizeof(wchar_t));
    std::vector<unsigned char> buffer(sizeof(FILE_RENAME_INFO) + nameBytes);
    auto* rename = reinterpret_cast<FILE_RENAME_INFO*>(buffer.data());
    rename->ReplaceIfExists = TRUE;
    rename->RootDirectory = nullptr;
    rename->FileNameLength = nameBytes;
    std::memcpy(rename->FileName, candidate.data(), nameBytes);
    renamed = ::SetFileInformationByHandle(
        file.get(), FileRenameInfo, rename, static_cast<DWORD>(buffer.size()));
    ok = renamed &&
         !g_fail_post_rename_verification_for_test.load() &&
         HandleResolvesTo(file.get(), candidate);
  }
  if (!ok && !renamed) {
    FILE_DISPOSITION_INFO disposition{TRUE};
    ::SetFileInformationByHandle(file.get(), FileDispositionInfo, &disposition,
                                 sizeof(disposition));
  }
  return ok;
}

bool AtomicWriteOwnedFile(const std::wstring& path, const std::string& data) {
  return AtomicWriteOwnedFile(path, data.data(), data.size());
}

bool DeleteOwnedFile(const std::wstring& path) {
  std::wstring candidate;
  OwnedPathLocks locks;
  if (!PrepareOwnedPath(path, &candidate, &locks)) return false;
  DWORD error = ERROR_SUCCESS;
  ScopedHandle file(OpenVerifiedRegularFile(
      candidate, DELETE | FILE_READ_ATTRIBUTES,
      FILE_SHARE_READ | FILE_SHARE_WRITE, OPEN_EXISTING, 0, &error));
  if (file.get() == INVALID_HANDLE_VALUE) return MissingError(error);
  FILE_DISPOSITION_INFO disposition{TRUE};
  return ::SetFileInformationByHandle(file.get(), FileDispositionInfo,
                                      &disposition, sizeof(disposition));
}

}  // namespace sr::paths
