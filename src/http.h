#pragma once
#include <string>
#include <utility>
#include <vector>

#include <windows.h>
#include <winhttp.h>

namespace sr {

struct HttpResponse {
  long status = 0;
  std::string body;        // raw bytes (may be binary for image downloads)
  std::string retry_after; // seconds, from the Retry-After header ("" if absent)
  bool succeeded = false;  // transport-level success (status may still be 4xx/5xx)
  std::string error;
};

using Header = std::pair<std::string, std::string>;

// Thin synchronous WinHTTP wrapper. One client per origin. Supports plain
// http://127.0.0.1 origins for tests and https:// for real endpoints.
class HttpClient {
 public:
  explicit HttpClient(const std::string& baseUrl);
  ~HttpClient();
  HttpClient(const HttpClient&) = delete;
  HttpClient& operator=(const HttpClient&) = delete;

  bool Valid() const { return session_ != nullptr; }

  // `path` must start with '/' and may already include a query string.
  // `body` participates for POST/PUT/DELETE (Content-Length driven).
  HttpResponse Send(const std::string& method, const std::string& path,
                    const std::string& body = {}, const std::vector<Header>& headers = {},
                    int timeoutMs = 20000);

  const std::string& base_url() const { return base_url_; }

 private:
  std::string base_url_;
  std::wstring host_;
  INTERNET_PORT port_ = INTERNET_DEFAULT_HTTP_PORT;
  bool secure_ = false;
  HINTERNET session_ = nullptr;
};

}  // namespace sr
