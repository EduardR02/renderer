#pragma once
#include <atomic>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <thread>

#include "http.h"
#include "util.h"

namespace sr {

// ---------------------------------------------------------------------------
// PKCE (RFC 7636)
// ---------------------------------------------------------------------------
struct Pkce {
  std::string verifier;   // 43..128 chars, [A-Za-z0-9-._~]
  std::string challenge;  // base64url(SHA-256(verifier)), no padding
};
Pkce GeneratePkce(RandomFill randomProvider = nullptr,
                  Sha256Digest hashProvider = nullptr);

// Authorization URL opened in the system browser. `state` is echoed back.
std::string BuildAuthorizeUrl(const std::string& clientId, const std::string& redirectUri,
                              const std::string& challenge, const std::string& state);

// ---------------------------------------------------------------------------
// Loopback redirect listener
// ---------------------------------------------------------------------------

// Parses an HTTP request line like "GET /callback?code=...&state=... HTTP/1.1".
// Returns nullopt if it is not a callback request; Kind::Error carries the
// OAuth error description; Kind::Invalid covers bad/missing state or code.
struct CallbackResult {
  enum class Kind { Code, Error, Invalid } kind = Kind::Invalid;
  std::string value;  // authorization code, or error description
};
std::optional<CallbackResult> ParseCallbackRequestLine(const std::string& requestLine,
                                                       const std::string& expectedState);

// Extracts host and port from a redirect URI. Only http://127.0.0.1[:port]
// or http://localhost[:port] are accepted (the app never serves on other
// interfaces).
bool ParseLoopbackUri(const std::string& redirectUri, std::string* host, uint16_t* port);

class LoopbackListener {
 public:
  ~LoopbackListener() { Stop(); }
  // `done` is called exactly once only for a valid state-bound authorization
  // code or OAuth error. Malformed/probe clients are rejected and ignored.
  bool Start(uint16_t port, const std::string& expectedState,
             std::function<void(std::string)> done, std::string* err);
  void Stop();
  bool Running() const { return running_.load(); }

 private:
  void ThreadMain(uint16_t port, const std::string& expectedState,
                  std::function<void(std::string)> done);

  std::thread th_;
  std::atomic<bool> running_{false};
  std::atomic<bool> stop_{false};
  std::atomic<uintptr_t> listen_socket_{UINTPTR_MAX};
  std::atomic<uintptr_t> client_socket_{UINTPTR_MAX};
};

// ---------------------------------------------------------------------------
// Token endpoints (accounts.spotify.com)
// ---------------------------------------------------------------------------
struct TokenResponse {
  std::string access_token;
  std::string refresh_token;
  int64_t expires_at = 0;  // unix seconds, 60s skew already applied
  bool has_refresh = false;
};

// Parses a token-endpoint JSON response. Returns nullopt on error and fills
// `err` with a human-readable message (error_description if present).
std::optional<TokenResponse> ParseTokenResponse(const std::string& jsonBody, int64_t nowSec,
                                                std::string* err);

// Authorization Code exchange (PKCE; no client secret).
std::optional<TokenResponse> ExchangeCode(HttpClient& accounts, const std::string& clientId,
                                          const std::string& redirectUri,
                                          const std::string& code, const std::string& verifier,
                                          std::string* err);

// Refresh-token grant. `err` includes the OAuth error when auth fails.
std::optional<TokenResponse> RefreshAccessToken(HttpClient& accounts, const std::string& clientId,
                                                const std::string& refreshToken,
                                                std::string* err,
                                                int timeoutMs = 20000);

}  // namespace sr
