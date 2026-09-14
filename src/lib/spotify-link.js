const SPOTIFY_ID = "[A-Za-z0-9]{22}";
const RESOURCE = "(track|album|artist|playlist)";
const URI_PATTERN = new RegExp(`^spotify:${RESOURCE}:(${SPOTIFY_ID})$`);
const PATH_PATTERN = new RegExp(`^/(?:intl-[a-z]{2}(?:-[a-z]{2})?/)?${RESOURCE}/(${SPOTIFY_ID})/?$`, "i");
const INVALID_LINK = "Use a Spotify song, album, artist or playlist link with a valid Spotify ID.";
const SHORT_LINK = "Short Spotify links aren't supported here yet. Copy the full open.spotify.com link instead.";

/** Null means ordinary search text. Links are parsed locally, never opened or fetched. */
export function parseSpotifyLink(value) {
  const text = String(value ?? "").trim();
  if (/^spotify:/i.test(text)) {
    const match = URI_PATTERN.exec(text);
    return match ? { kind: match[1], id: match[2] } : { error: INVALID_LINK };
  }

  const bareSpotifyHost = /^(?:open\.spotify\.com|spotify\.link|spoti\.fi)(?:[/?#]|$)/i.test(text);
  const looksLikeUrl = bareSpotifyHost || /^(?:[a-z][a-z\d+.-]*:\/\/|https?:|javascript:|data:|file:|vbscript:|mailto:|\/\/|www\.)/i.test(text);
  if (!looksLikeUrl) return null;
  if (/[\s\\\u0000-\u001f\u007f]/.test(text)) return { error: INVALID_LINK };

  let url;
  try {
    url = new URL(bareSpotifyHost ? `https://${text}` : text);
  } catch {
    return { error: INVALID_LINK };
  }
  if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.port) {
    return { error: INVALID_LINK };
  }
  if (url.hostname === "spotify.link" || url.hostname === "spoti.fi") return { error: SHORT_LINK };
  if (url.hostname !== "open.spotify.com") {
    return { error: "This isn't a Spotify link. Paste an open.spotify.com link, or enter search text." };
  }
  const match = PATH_PATTERN.exec(url.pathname);
  return match ? { kind: match[1].toLowerCase(), id: match[2] } : { error: INVALID_LINK };
}
