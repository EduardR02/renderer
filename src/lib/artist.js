/**
 * Projects the two playlist shelves from one artist overview. Dedupe order is
 * part of the UI contract: Artist Pick wins, then Artist playlists, then
 * Discovered on. Both the overview and See-all views use this exact projection.
 */
export function artistPlaylistCollections(overview) {
  const seen = new Set();
  if (overview?.artist_pick?.id) seen.add(overview.artist_pick.id);

  function unique(items) {
    const result = [];
    for (const playlist of items ?? []) {
      if (!playlist?.id || seen.has(playlist.id)) continue;
      seen.add(playlist.id);
      result.push(playlist);
    }
    return result;
  }

  return {
    artist: unique(overview?.artist_playlists),
    discovered: unique(overview?.discovered_on),
  };
}

const HTML_ENTITIES = Object.freeze({
  amp: "&",
  apos: "'",
  gt: ">",
  lt: "<",
  nbsp: " ",
  quot: '"',
  copy: "©",
  reg: "®",
  trade: "™",
  hellip: "…",
  ndash: "–",
  mdash: "—",
  lsquo: "‘",
  rsquo: "’",
  ldquo: "“",
  rdquo: "”",
  bull: "•",
  middot: "·",
  laquo: "«",
  raquo: "»",
  cent: "¢",
  pound: "£",
  yen: "¥",
  euro: "€",
  sect: "§",
  para: "¶",
  plusmn: "±",
  times: "×",
  divide: "÷",
  micro: "µ",
  deg: "°",
});

function decodeHtmlEntities(value) {
  return value.replace(
    /&(#(?:x[0-9a-f]+|\d+)|[a-z][a-z0-9]+);/giu,
    (match, body) => {
      if (String(body).length > 32) return match;
      const key = String(body).toLowerCase();
      if (Object.prototype.hasOwnProperty.call(HTML_ENTITIES, key)) {
        return HTML_ENTITIES[key];
      }
      const raw = String(body);
      const code = raw.toLowerCase().startsWith("#x")
        ? Number.parseInt(raw.slice(2), 16)
        : raw.startsWith("#")
          ? Number.parseInt(raw.slice(1), 10)
          : NaN;
      if (
        Number.isInteger(code) &&
        code > 0 &&
        code <= 0x10ffff &&
        (code < 0xd800 || code > 0xdfff)
      ) {
        return String.fromCodePoint(code);
      }
      return match;
    },
  );
}

function stripPlaylistMarkup(value) {
  let text = "";
  let cursor = 0;
  while (cursor < value.length) {
    const open = value.indexOf("<", cursor);
    if (open < 0) {
      text += value.slice(cursor);
      break;
    }
    text += value.slice(cursor, open);
    if (value.startsWith("<!--", open)) {
      const end = value.indexOf("-->", open + 4);
      if (end < 0) {
        // Preserve malformed/incomplete markup as text. Svelte escapes the
        // result, and dropping the remainder would corrupt a description.
        text += value.slice(open);
        break;
      }
      text += " ";
      cursor = end + 3;
      continue;
    }
    const close = value.indexOf(">", open + 1);
    if (close < 0) {
      // Preserve malformed/incomplete markup as text. Svelte escapes the
      // result, and dropping the remainder would corrupt a description.
      text += value.slice(open);
      break;
    }
    text += " ";
    cursor = close + 1;
  }
  return text;
}

function normalizePlainText(value) {
  return value
    .replace(/\s+/gu, " ")
    .replace(/\s+([,.;:!?])/gu, "$1")
    .trim();
}

function containsCompletePlaylistMarkup(value) {
  return /<!--[\s\S]*?-->|<\/?[A-Za-z][^>]*>/u.test(value);
}

export function sanitizePlaylistDescription(value) {
  const raw = String(value ?? "");
  if (!raw) return "";
  return normalizePlainText(decodeHtmlEntities(stripPlaylistMarkup(raw)));
}

/**
 * Normalises a description that has already crossed the engine boundary.
 * Entity decoding is deliberately not repeated: `&amp;lt;` is canonicalised
 * once by the engine to `&lt;`, and decoding it again would turn user text into
 * markup before a later sanitizer could erase it. A pair of comparison
 * brackets is ordinary text unless the enclosed spelling can actually start
 * an HTML tag.
 */
export function normalizeCanonicalPlaylistDescription(value) {
  const raw = String(value ?? "");
  if (!raw) return "";
  return containsCompletePlaylistMarkup(raw)
    ? sanitizePlaylistDescription(raw)
    : normalizePlainText(raw);
}

function meaningfulOwner(playlist) {
  const isGeneric = (owner) =>
    !owner ||
    /^(?:spotify(?:\b|[_-]).*|user(?:\b|[_-]).*|unknown|owner|me|you)$/i.test(owner);
  return [playlist?.owner, playlist?.owner_name, playlist?.owner_id]
    .map((value) => String(value ?? "").replace(/\s+/g, " ").trim())
    .find((owner) => !isGeneric(owner)) ?? "";
}
/** Shared plain-text subtitle for every playlist card surface. */

export function playlistSubtitle(playlist) {
  const description = normalizeCanonicalPlaylistDescription(playlist?.description);
  if (description) return description;

  const owner = meaningfulOwner(playlist);
  if (owner) return `By ${owner}`;

  const count = [playlist?.tracks_total, playlist?.track_count]
    .map((value) => Number(value))
    .find((value) => Number.isFinite(value) && value > 0);
  if (count !== undefined) return `${Math.round(count)} songs`;
  return "Playlist";
}
