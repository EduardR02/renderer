#include <windows.h>
#include <shellapi.h>

#include <algorithm>
#include <string>
#include <vector>

#include "app.h"
#include "app_paths.h"

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, PWSTR, int show) {
  int count = 0;
  LPWSTR* raw = ::CommandLineToArgvW(::GetCommandLineW(), &count);
  if (!raw) return 64;
  std::vector<std::wstring> arguments(raw, raw + count);
  ::LocalFree(raw);

  sr::RunOptions options;
  for (size_t i = 1; i < arguments.size(); ++i) {
    if (arguments[i] == L"--smoke") {
      options.smoke = true;
      if (i + 1 < arguments.size() &&
          arguments[i + 1].find_first_not_of(L"0123456789") ==
              std::wstring::npos)
        options.smokeSeconds = std::max(1, _wtoi(arguments[++i].c_str()));
    } else if (arguments[i] == L"--demo") {
      options.demo = true;
    } else if (arguments[i] == L"--isolation-test-root" &&
               i + 1 < arguments.size()) {
      options.isolationTestRoot = arguments[++i];
    } else {
      return 64;
    }
  }
  if (options.smoke && options.demo) return 64;
  if (!options.isolationTestRoot.empty()) {
    if (!options.smoke && !options.demo) return 64;
    sr::paths::SetTestRootForCurrentProcess(options.isolationTestRoot);
  }

  sr::Application app;
  return app.Run(instance, show, options);
}
