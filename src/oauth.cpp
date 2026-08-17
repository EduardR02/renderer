#include "oauth.h"

#include <winsock2.h>
#include <ws2tcpip.h>

#include <nlohmann/json.hpp>

#include "log.h"
#include "util.h"

namespace sr {

using nlohmann::json;

namespace {
const char kScopes[] =
    "user-read-private playlist-read-private playlist-read-collaborative "
    "playlist-modify-private playlist-modify-public";
}  // namespace

Pkce GeneratePkce(RandomFill randomProvider, Sha256Digest hashProvider) {
  Pkce pkce;
  std::vector<uint8_t> raw = RandomBytes(64, randomProvider);
  pkce.verifier = Base64UrlEncode(raw.data(), raw.size());
  pkce.challenge = Sha256Base64Url(pkce.verifier, hashProvider);
  return pkce;
}

std::string BuildAuthorizeUrl(const std::string& clientId, const std::string& redirectUri,
                              const std::string& challenge, const std::string& state) {
  return "https://accounts.spotify.com/authorize"
         "?client_id=" + UrlEncode(clientId) +
         "&response_type=code"
         "&redirect_uri=" + UrlEncode(redirectUri) +
         "&scope=" + UrlEncode(kScopes) +
         "&code_challenge=" + UrlEncode(challenge) +
         "&code_challenge_method=S256"
         "&state=" + UrlEncode(state);
}

std::optional<CallbackResult> ParseCallbackRequestLine(const std::string& line,
                                                       const std::string& expectedState) {
  size_t sp1 = line.find(' ');
  if (sp1 == std::string::npos || line.substr(0, sp1) != "GET") return std::nullopt;
  size_t sp2 = line.find(' ', sp1 + 1);
  if (sp2 == std::string::npos || line.substr(sp2 + 1, 5) != "HTTP/") return std::nullopt;
  std::string path = line.substr(sp1 + 1, sp2 - sp1 - 1);
  size_t q = path.find('?');
  if (path.substr(0, q) != "/callback") return std::nullopt;
  if (q == std::string::npos) return CallbackResult{};

  std::string code, state, error;
  for (const std::string& pair : Split(path.substr(q + 1), '&')) {
    size_t eq = pair.find('=');
    std::string k = UrlDecode(eq == std::string::npos ? pair : pair.substr(0, eq));
    std::string v = UrlDecode(eq == std::string::npos ? std::string() : pair.substr(eq + 1));
    if (k == "code") code = v;
    else if (k == "state") state = v;
    else if (k == "error") error = v;
  }
  // State is mandatory for both success and error callbacks.
  if (state.empty() || state != expectedState) return CallbackResult{};
  if (!error.empty()) {
    CallbackResult r;
    r.kind = CallbackResult::Kind::Error;
    r.value = error;
    return r;
  }
  if (code.empty()) return CallbackResult{};
  CallbackResult r;
  r.kind = CallbackResult::Kind::Code;
  r.value = code;
  return r;
}

bool ParseLoopbackUri(const std::string& redirectUri, std::string* host, uint16_t* port) {
  if (!host || !port || !StartsWithIgnoreCase(redirectUri, "http://")) return false;
  std::string u = redirectUri.substr(7);
  size_t slash = u.find('/');
  std::string authority = slash == std::string::npos ? u : u.substr(0, slash);
  std::string path = slash == std::string::npos ? "/" : u.substr(slash);
  if (path != "/callback") return false;

  size_t colon = authority.rfind(':');
  if (colon == std::string::npos || colon == 0 || colon + 1 == authority.size()) return false;
  std::string hostPart = ToLower(authority.substr(0, colon));
  std::string portPart = authority.substr(colon + 1);
  if (hostPart != "127.0.0.1" && hostPart != "localhost") return false;
  if (portPart.find_first_not_of("0123456789") != std::string::npos) return false;
  unsigned long parsed = strtoul(portPart.c_str(), nullptr, 10);
  if (parsed == 0 || parsed > 65535) return false;
  *host = "127.0.0.1";
  *port = static_cast<uint16_t>(parsed);
  return true;
}

bool LoopbackListener::Start(uint16_t port, const std::string& expectedState,
                             std::function<void(std::string)> done, std::string* err) {
  if (running_.exchange(true)) {
    if (err) *err = "listener is already running";
    return false;
  }
  if (err) err->clear();
  if (th_.joinable()) th_.join();
  stop_.store(false);
  th_ = std::thread([this, port, expectedState, done = std::move(done)] {
    ThreadMain(port, expectedState, std::move(done));
  });
  return true;
}

void LoopbackListener::Stop() {
  stop_.store(true);
  uintptr_t client = client_socket_.exchange(UINTPTR_MAX);
  if (client != UINTPTR_MAX) {
    ::shutdown(static_cast<SOCKET>(client), SD_BOTH);
    ::closesocket(static_cast<SOCKET>(client));
  }
  uintptr_t listener = listen_socket_.exchange(UINTPTR_MAX);
  if (listener != UINTPTR_MAX) {
    ::shutdown(static_cast<SOCKET>(listener), SD_BOTH);
    ::closesocket(static_cast<SOCKET>(listener));
  }
  if (th_.joinable()) th_.join();
  running_.store(false);
}

void LoopbackListener::ThreadMain(uint16_t port, const std::string& expectedState,
                                  std::function<void(std::string)> done) {
  WSADATA wsa{};
  if (::WSAStartup(MAKEWORD(2, 2), &wsa) != 0) {
    if (done) done("error: winsock init failed");
    running_.store(false);
    return;
  }
  SOCKET server = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
  if (server == INVALID_SOCKET) {
    ::WSACleanup();
    if (done) done("error: socket failed");
    running_.store(false);
    return;
  }
  sockaddr_in address{};
  address.sin_family = AF_INET;
  address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  address.sin_port = htons(port);
  if (::bind(server, reinterpret_cast<sockaddr*>(&address), sizeof(address)) == SOCKET_ERROR ||
      ::listen(server, 4) == SOCKET_ERROR) {
    ::closesocket(server);
    ::WSACleanup();
    if (done) done("error: cannot listen on redirect port " + std::to_string(port));
    running_.store(false);
    return;
  }
  listen_socket_.store(static_cast<uintptr_t>(server));

  bool completed = false;
  while (!stop_.load() && !completed) {
    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(server, &readable);
    timeval interval{0, 100000};
    int selected = ::select(0, &readable, nullptr, nullptr, &interval);
    if (selected == SOCKET_ERROR) break;
    if (selected == 0) continue;
    SOCKET client = ::accept(server, nullptr, nullptr);
    if (client == INVALID_SOCKET) continue;
    client_socket_.store(static_cast<uintptr_t>(client));

    std::string requestLine;
    const ULONGLONG deadline = ::GetTickCount64() + 2000;
    bool completeLine = false;
    while (!stop_.load() && requestLine.size() <= 8192 &&
           ::GetTickCount64() < deadline) {
      fd_set clientReadable;
      FD_ZERO(&clientReadable);
      FD_SET(client, &clientReadable);
      timeval readInterval{0, 100000};
      int ready = ::select(0, &clientReadable, nullptr, nullptr, &readInterval);
      if (ready == SOCKET_ERROR) break;
      if (ready == 0) continue;
      char buffer[1024];
      int count = ::recv(client, buffer, sizeof(buffer), 0);
      if (count <= 0) break;
      requestLine.append(buffer, static_cast<size_t>(count));
      size_t end = requestLine.find("\r\n");
      if (end != std::string::npos) {
        requestLine.resize(end);
        completeLine = true;
        break;
      }
    }

    auto parsed = completeLine
                      ? ParseCallbackRequestLine(requestLine, expectedState)
                      : std::optional<CallbackResult>{};
    const bool valid = parsed && parsed->kind != CallbackResult::Kind::Invalid;
    std::string page = valid
        ? "<html><body style=\"background:#121212;color:#eee;font-family:sans-serif\">"
          "<p>Spotify authorization complete. You can close this tab.</p></body></html>"
        : "<html><body><p>Invalid callback. The authorization listener is still waiting."
          "</p></body></html>";
    std::string status = valid ? "200 OK" : "400 Bad Request";
    std::string response = "HTTP/1.1 " + status +
        "\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: " +
        std::to_string(page.size()) + "\r\nConnection: close\r\n\r\n" + page;
    ::send(client, response.data(), static_cast<int>(response.size()), 0);
    if (client_socket_.exchange(UINTPTR_MAX) != UINTPTR_MAX) {
      ::shutdown(client, SD_BOTH);
      ::closesocket(client);
    }
    if (valid) {
      completed = true;
      if (done) {
        if (parsed->kind == CallbackResult::Kind::Code)
          done(parsed->value);
        else
          done("error: " + parsed->value);
      }
    }
  }

  if (listen_socket_.exchange(UINTPTR_MAX) != UINTPTR_MAX) ::closesocket(server);
  ::WSACleanup();
  running_.store(false);
}

std::optional<TokenResponse> ParseTokenResponse(const std::string& jsonBody, int64_t nowSec,
                                                std::string* err) {
  try {
    json j = json::parse(jsonBody);
    if (!j.is_object()) {
      if (err) *err = "malformed token response";
      return std::nullopt;
    }
    if (j.contains("error")) {
      std::string e = j["error"].is_string() ? j["error"].get<std::string>() : "unknown";
      std::string d = j.contains("error_description") && j["error_description"].is_string()
                          ? j["error_description"].get<std::string>()
                          : "";
      if (err) *err = d.empty() ? e : (e + ": " + d);
      return std::nullopt;
    }
    TokenResponse t;
    if (!j.contains("access_token") || !j["access_token"].is_string()) {
      if (err) *err = "token response missing access_token";
      return std::nullopt;
    }
    t.access_token = j["access_token"].get<std::string>();
    if (j.contains("refresh_token") && j["refresh_token"].is_string()) {
      t.refresh_token = j["refresh_token"].get<std::string>();
      t.has_refresh = true;
    }
    int64_t expiresIn = 3600;
    if (j.contains("expires_in") && j["expires_in"].is_number_integer())
      expiresIn = j["expires_in"].get<int64_t>();
    if (expiresIn < 0) expiresIn = 0;
    if (expiresIn > 86400) expiresIn = 86400;
    // Apply a 60s safety skew so we refresh before the server revokes.
    int64_t skew = expiresIn > 120 ? 60 : (expiresIn / 2);
    t.expires_at = nowSec + expiresIn - skew;
    return t;
  } catch (...) {
    if (err) *err = "malformed token response";
    return std::nullopt;
  }
}

namespace {
std::optional<TokenResponse> TokenPost(HttpClient& accounts, const std::string& formBody,
                                       int64_t nowSec, std::string* err, int timeoutMs) {
  HttpResponse r = accounts.Send("POST", "/api/token", formBody,
                                 {{"Content-Type", "application/x-www-form-urlencoded"},
                                  {"Accept", "application/json"}}, timeoutMs);
  if (!r.succeeded) {
    if (err) *err = "token endpoint unreachable: " + r.error;
    return std::nullopt;
  }
  return ParseTokenResponse(r.body, nowSec, err);
}
}  // namespace

std::optional<TokenResponse> ExchangeCode(HttpClient& accounts, const std::string& clientId,
                                          const std::string& redirectUri,
                                          const std::string& code, const std::string& verifier,
                                          std::string* err) {
  std::string body = "grant_type=authorization_code"
                     "&code=" + UrlEncode(code) +
                     "&redirect_uri=" + UrlEncode(redirectUri) +
                     "&client_id=" + UrlEncode(clientId) +
                     "&code_verifier=" + UrlEncode(verifier);
  return TokenPost(accounts, body, NowUnixSeconds(), err, 20000);
}

std::optional<TokenResponse> RefreshAccessToken(HttpClient& accounts, const std::string& clientId,
                                                const std::string& refreshToken,
                                                std::string* err, int timeoutMs) {
  std::string body = "grant_type=refresh_token"
                     "&refresh_token=" + UrlEncode(refreshToken) +
                     "&client_id=" + UrlEncode(clientId);
  return TokenPost(accounts, body, NowUnixSeconds(), err, timeoutMs);
}

}  // namespace sr
