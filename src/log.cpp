#include "log.h"

#include <windows.h>

#include <cstdio>
#include <mutex>

#include "util.h"
#include "app_paths.h"
#include "version.h"

namespace sr::log {

namespace {
std::mutex g_mu;
std::wstring g_file;
bool g_console = false;

void WriteUnlocked(Level lvl, const std::string& msg) {
  SYSTEMTIME st;
  ::GetLocalTime(&st);
  char ts[64];
  snprintf(ts, sizeof ts, "%04d-%02d-%02d %02d:%02d:%02d.%03d", (int)st.wYear,
           (int)st.wMonth, (int)st.wDay, (int)st.wHour, (int)st.wMinute, (int)st.wSecond,
           (int)st.wMilliseconds);
  const char* tag = lvl == Level::Info ? "INFO" : (lvl == Level::Warn ? "WARN" : "ERROR");
  std::string clean = RedactSecrets(msg);
  std::string line = "[" + std::string(ts) + "] " + tag + " " + clean + "\n";
  if (!g_file.empty() && !paths::AppendOwnedFile(g_file, line)) g_file.clear();
  if (g_console) {
    fprintf(stdout, "[%s] %s %s\n", ts, tag, clean.c_str());
    fflush(stdout);
  }
  OutputDebugStringA(("[SpotifyRenderer] " + clean + "\n").c_str());
}
}  // namespace

void Init(const std::wstring& logFile) {
  if (_wcsicmp(paths::Canonical(logFile).c_str(), paths::Canonical(paths::LogFile()).c_str()) !=
      0)
    return;
  std::lock_guard<std::mutex> lock(g_mu);
  g_file.clear();
  if (!logFile.empty()) {
    g_file = logFile;
    WriteUnlocked(Level::Info, std::string("log opened (v" SR_APP_VERSION ")"));
  }
}

void SetConsole(bool on) {
  std::lock_guard<std::mutex> lock(g_mu);
  g_console = on;
}

void Write(Level lvl, const std::string& msg) {
  std::lock_guard<std::mutex> lock(g_mu);
  WriteUnlocked(lvl, msg);
}

void Close() {
  std::lock_guard<std::mutex> lock(g_mu);
  g_file.clear();
}

}  // namespace sr::log
