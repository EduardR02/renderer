#pragma once
#include <string>

// Diagnostics log. All output is redacted (never contains tokens, auth codes
// or credentials) and written only under the app-owned data directory.
namespace sr::log {

enum class Level { Info, Warn, Error };

void Init(const std::wstring& logFile);
void SetConsole(bool on);  // mirror lines to stdout (used by --smoke)
void Write(Level lvl, const std::string& msg);
void Close();

}  // namespace sr::log

#define LOG_INFO(msg) ::sr::log::Write(::sr::log::Level::Info, (msg))
#define LOG_WARN(msg) ::sr::log::Write(::sr::log::Level::Warn, (msg))
#define LOG_ERROR(msg) ::sr::log::Write(::sr::log::Level::Error, (msg))
