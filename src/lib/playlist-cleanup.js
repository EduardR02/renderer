import { parseExactTime } from "./time.js";

const normalize = (value) => String(value ?? "").normalize("NFKC").toLowerCase();

/** Choices are playlist metadata, never remote search results. IDs distinguish namesakes. */
export function cleanupChoices(tracks, field) {
  const choices = new Map();
  for (const track of tracks) {
    const names = field === "artist" ? track.artist_names ?? [] : [track.album_name];
    const ids = field === "artist" ? track.artist_ids ?? [] : [track.album_id];
    names.forEach((name, index) => {
      if (!name) return;
      const id = ids[index] || "";
      /* Folded once, here, because this runs when the playlist or the field
         changes while the filter below runs on every keystroke: stored, one
         keystroke costs one `includes` per choice instead of a new folded
         string for every artist in the playlist. */
      const normalized = normalize(name);
      const key = id ? `id:${id}` : `name:${normalized}`;
      const existing = choices.get(key);
      if (existing) existing.count += 1;
      else choices.set(key, {
        key, id, name, normalized, count: 1,
        hint: field === "artist" ? track.name : (track.artist_names ?? []).join(", "),
      });
    });
  }
  return [...choices.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/** Matches on the folded name `cleanupChoices` stored, never on a fresh fold. */
export function filterCleanupChoices(choices, query) {
  const text = normalize(query).trim();
  return choices.filter((choice) => choice.normalized.includes(text));
}

export function cleanupDuration(value) {
  const duration = parseExactTime(value);
  return duration !== null && duration > 0 && Number.isSafeInteger(duration) ? duration : null;
}

function ruleMatcher(rule) {
  if (rule.field === "artist" || rule.field === "album") {
    // The rule's own name is folded once per matcher, not once per track.
    const wanted = rule.choice.normalized;
    return (track) => {
      const names = rule.field === "artist" ? track.artist_names ?? [] : [track.album_name];
      const ids = rule.field === "artist" ? track.artist_ids ?? [] : [track.album_id];
      return names.some((name, index) => rule.choice.id
        ? ids[index] === rule.choice.id
        : !ids[index] && normalize(name) === wanted);
    };
  }
  if (rule.field === "duration") {
    return (track) => Number.isFinite(track.duration_ms) && track.duration_ms > 0 && (
      rule.operator === "shorter" ? track.duration_ms < rule.duration : track.duration_ms > rule.duration
    );
  }
  const text = normalize(rule.text);
  if (rule.operator === "contains") return (track) => normalize(track.name).includes(text);
  // User text is always literal. Unicode boundaries avoid treating accented
  // letters as punctuation, unlike JavaScript's ASCII-only word boundary.
  const escaped = text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const words = new RegExp(`(?<![\\p{L}\\p{N}\\p{M}_])${escaped}(?![\\p{L}\\p{N}\\p{M}_])`, "u");
  return (track) => words.test(normalize(track.name));
}

/** Expand by URI: Spotify removes every occurrence, not just the matching row. */
export function cleanupPreview(tracks, rules, grouping) {
  if (!rules.length) return { rows: [], uris: [], directCount: 0, extraCount: 0, missingCount: 0 };
  const matchers = rules.map(ruleMatcher);
  const direct = tracks.map((track) => grouping === "all"
    ? matchers.every((matches) => matches(track))
    : matchers.some((matches) => matches(track)));
  const uris = new Set();
  let directCount = 0;
  let missingCount = 0;
  tracks.forEach((track, index) => {
    if (!direct[index]) return;
    if (!track.uri) missingCount += 1;
    else {
      directCount += 1;
      uris.add(track.uri);
    }
  });
  const rows = [];
  tracks.forEach((track, index) => {
    if (uris.has(track.uri)) rows.push({ track, index, extra: !direct[index] });
  });
  return { rows, uris: [...uris], directCount, extraCount: rows.length - directCount, missingCount };
}
