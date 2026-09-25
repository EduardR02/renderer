/* The kinds this app can name. One list, read from both directions: `RESOURCE`
   is what search accepts, and `spotifyLink` builds only these. */
const KINDS = ["track", "album", "artist", "playlist"];
const SPOTIFY_ID = "[A-Za-z0-9]{22}";
const RESOURCE = `(${KINDS.join("|")})`;
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

/**
 * The public page for one Spotify resource — the inverse of the parser above,
 * and the reason a link copied out of this app pastes back into its own search
 * and opens here.
 *
 * `id` is the id already on the payload. A `spotify:<kind>:<id>` URI is taken
 * too, because a row that carries only the URI still has an id and it is the
 * trailing segment. Nothing is assembled from anything else: a guessed URL that
 * resolves to the wrong page is worse than the menu item the empty string
 * removes.
 */
export function spotifyLink(kind, id) {
  if (!KINDS.includes(kind)) return "";
  const value = String(id ?? "").split(":").pop().trim();
  return value ? `https://open.spotify.com/${kind}/${value}` : "";
}

/**
 * Put text on the system clipboard; true when it landed, which is what the
 * menus turn into "Link copied".
 *
 * Uses the Clipboard API when permitted by the webview. No clipboard plugin is
 * installed; `execCommand` handles a refused write (for example, when the
 * document is not focused) without claiming the link was copied.
 */
export async function writeClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      /* `select()` moves focus into the field, and removing the field would
         then drop the page's focus to `<body>` — mid-menu, while the item is
         about to say whether the write landed. Whatever had focus gets it
         back; a `<body>` that had none is a no-op. */
      const active = document.activeElement;
      const field = document.createElement("textarea");
      field.value = text;
      field.setAttribute("readonly", "");
      field.style.cssText = "position:fixed;top:-1000px;opacity:0";
      document.body.appendChild(field);
      field.select();
      const ok = document.execCommand("copy");
      field.remove();
      if (active?.isConnected) active.focus?.();
      return ok;
    } catch {
      return false;
    }
  }
}
