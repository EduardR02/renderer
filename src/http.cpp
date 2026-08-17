#include "http.h"

#include "util.h"
#include "version.h"

#include <algorithm>
#include <mutex>
#include <utility>

namespace sr {

namespace {
struct RequestDeadline {
  std::mutex mutex;
  HINTERNET request = nullptr;
  bool timed_out = false;
};

void CALLBACK CancelTimedOutRequest(PTP_CALLBACK_INSTANCE, void* value, PTP_TIMER) {
  auto& deadline = *static_cast<RequestDeadline*>(value);
  std::lock_guard<std::mutex> lock(deadline.mutex);
  if (!deadline.request) return;
  deadline.timed_out = true;
  ::WinHttpCloseHandle(std::exchange(deadline.request, nullptr));
}
}  // namespace

HttpClient::HttpClient(const std::string& baseUrl) {
  std::string url = baseUrl;
  while (!url.empty() && url.back() == '/') url.pop_back();
  base_url_ = url;

  size_t schemeEnd = url.find("://");
  if (schemeEnd == std::string::npos) return;
  std::string scheme = ToLower(url.substr(0, schemeEnd));
  secure_ = (scheme == "https");
  if (scheme != "https" && scheme != "http") return;

  std::string rest = url.substr(schemeEnd + 3);
  size_t slash = rest.find('/');
  std::string authority = (slash == std::string::npos) ? rest : rest.substr(0, slash);
  port_ = secure_ ? INTERNET_DEFAULT_HTTPS_PORT : INTERNET_DEFAULT_HTTP_PORT;
  size_t colon = authority.find(':');
  if (colon != std::string::npos && colon != authority.size() - 1) {
    std::string hostPart = authority.substr(0, colon);
    std::string portText = authority.substr(colon + 1);
    if (portText.find_first_not_of("0123456789") != std::string::npos) return;
    int p = atoi(portText.c_str());
    if (hostPart.empty() || p <= 0 || p > 65535) return;
    host_ = Utf8ToWide(hostPart);
    port_ = static_cast<INTERNET_PORT>(p);
  } else {
    host_ = Utf8ToWide(authority);
  }
  if (host_.empty()) return;
  if (!secure_) {
    std::string hostLower = ToLower(WideToUtf8(host_));
    if (hostLower != "127.0.0.1" && hostLower != "localhost") {
      host_.clear();
      return;
    }
  }

  session_ = ::WinHttpOpen(SR_APP_USER_AGENT_W, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                           WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
}

HttpClient::~HttpClient() {
  if (session_) ::WinHttpCloseHandle(session_);
}

HttpResponse HttpClient::Send(const std::string& method, const std::string& path,
                              const std::string& body, const std::vector<Header>& headers,
                              int timeoutMs) {
  HttpResponse out;
  if (!session_) {
    out.error = "invalid base URL";
    return out;
  }
  if (path.empty() || path.front() != '/') {
    out.error = "request path must be origin-relative";
    return out;
  }

  HINTERNET conn = ::WinHttpConnect(session_, host_.c_str(), port_, 0);
  if (!conn) {
    out.error = "WinHttpConnect failed";
    return out;
  }
  std::wstring wmethod = Utf8ToWide(method);
  std::wstring wpath = Utf8ToWide(path);
  HINTERNET req = ::WinHttpOpenRequest(conn, wmethod.c_str(), wpath.c_str(), nullptr,
                                       WINHTTP_NO_REFERER, WINHTTP_DEFAULT_ACCEPT_TYPES,
                                       secure_ ? WINHTTP_FLAG_SECURE : 0);
  if (!req) {
    ::WinHttpCloseHandle(conn);
    out.error = "WinHttpOpenRequest failed";
    return out;
  }

  const int boundedTimeout = std::max(1, timeoutMs);
  ::WinHttpSetTimeouts(req, boundedTimeout, boundedTimeout, boundedTimeout,
                       boundedTimeout);

  RequestDeadline deadline;
  deadline.request = req;
  PTP_TIMER deadlineTimer =
      ::CreateThreadpoolTimer(CancelTimedOutRequest, &deadline, nullptr);
  if (!deadlineTimer) {
    ::WinHttpCloseHandle(req);
    ::WinHttpCloseHandle(conn);
    out.error = "could not create request deadline";
    return out;
  }
  LARGE_INTEGER due;
  due.QuadPart = -static_cast<LONGLONG>(boundedTimeout) * 10000;
  ::SetThreadpoolTimer(deadlineTimer, reinterpret_cast<FILETIME*>(&due), 0, 0);

  std::string allHeaders;
  for (const auto& [k, v] : headers) {
    allHeaders += k + ": " + v + "\r\n";
  }
  if (!allHeaders.empty()) {
    std::wstring w = Utf8ToWide(allHeaders);
    ::WinHttpAddRequestHeaders(req, w.c_str(), (DWORD)-1L, WINHTTP_ADDREQ_FLAG_ADD);
  }

  BOOL sent;
  if (!body.empty()) {
    // Content-Length is required for entity bodies.
    std::string cl = "Content-Length: " + std::to_string(body.size()) + "\r\n";
    ::WinHttpAddRequestHeaders(req, Utf8ToWide(cl).c_str(), (DWORD)-1L,
                               WINHTTP_ADDREQ_FLAG_ADD);
    sent = ::WinHttpSendRequest(req, WINHTTP_NO_ADDITIONAL_HEADERS, 0, (LPVOID)body.data(),
                                (DWORD)body.size(), (DWORD)body.size(), 0);
  } else {
    sent = ::WinHttpSendRequest(req, WINHTTP_NO_ADDITIONAL_HEADERS, 0, WINHTTP_NO_REQUEST_DATA,
                                0, 0, 0);
  }
  if (sent && ::WinHttpReceiveResponse(req, nullptr)) {
    DWORD status = 0, size = sizeof(status);
    ::WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                          WINHTTP_HEADER_NAME_BY_INDEX, &status, &size, WINHTTP_NO_HEADER_INDEX);
    out.status = (long)status;

    wchar_t ra[64] = {};
    DWORD raSize = sizeof(ra);
    if (::WinHttpQueryHeaders(req, WINHTTP_QUERY_RETRY_AFTER, WINHTTP_HEADER_NAME_BY_INDEX, ra,
                              &raSize, WINHTTP_NO_HEADER_INDEX))
      out.retry_after = WideToUtf8(ra);

    DWORD avail = 0;
    do {
      avail = 0;
      if (!::WinHttpQueryDataAvailable(req, &avail)) break;
      if (avail == 0) break;
      std::vector<char> buf(avail);
      DWORD read = 0;
      if (!::WinHttpReadData(req, buf.data(), avail, &read) || read == 0) break;
      out.body.append(buf.data(), read);
    } while (true);
    out.succeeded = true;
  } else {
    out.error = "request failed: " + std::to_string(::GetLastError());
  }

  HINTERNET requestToClose = nullptr;
  {
    std::lock_guard<std::mutex> lock(deadline.mutex);
    requestToClose = std::exchange(deadline.request, nullptr);
  }
  ::SetThreadpoolTimer(deadlineTimer, nullptr, 0, 0);
  ::WaitForThreadpoolTimerCallbacks(deadlineTimer, TRUE);
  ::CloseThreadpoolTimer(deadlineTimer);
  if (requestToClose) ::WinHttpCloseHandle(requestToClose);
  bool timedOut = false;
  {
    std::lock_guard<std::mutex> lock(deadline.mutex);
    timedOut = deadline.timed_out;
  }
  if (timedOut) {
    out.succeeded = false;
    out.status = 0;
    out.body.clear();
    out.retry_after.clear();
    out.error = "request timed out";
  }
  ::WinHttpCloseHandle(conn);
  return out;
}

}  // namespace sr
