import { formatExactTime, formatTime, parseExactTime } from "./time.js";

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

/**
 * The instant a `YYYY-MM-DD` date field names: LOCAL midnight, not the UTC
 * midnight `new Date("2024-03-01")` builds — read that way, a rule fires an
 * hour early in Berlin, a day early in Los Angeles, and the songs it removes
 * are the ones the reader never named. A date the calendar does not have
 * (2024-02-31) is refused rather than rolled into March.
 */
export function parseLocalDate(value) {
  const parts = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(value ?? "").trim());
  if (!parts) return null;
  const [year, month, day] = parts.slice(1).map(Number);
  const date = new Date(year, month - 1, day);
  return date.getMonth() === month - 1 && date.getDate() === day ? date.getTime() : null;
}

/**
 * The instant `amount` units before `now`, for the `Older than` rule. Every
 * unit is counted on the local calendar, never as a fixed number of days: one
 * month before 31 March is the last day of February, sat on by a `setMonth`
 * rollover into 3 March — and the songs added in those three days are songs
 * the rule never asked for. A short month therefore clamps down, the only
 * direction that cannot widen the removal set.
 */
export function addedCutoff(amount, unit, now = Date.now()) {
  const date = new Date(now);
  if (unit === "days" || unit === "weeks") {
    date.setDate(date.getDate() - amount * (unit === "weeks" ? 7 : 1));
    return date.getTime();
  }
  const day = date.getDate();
  date.setDate(1);
  date.setMonth(date.getMonth() - (unit === "years" ? amount * 12 : amount));
  const lastDay = new Date(date.getFullYear(), date.getMonth() + 1, 0).getDate();
  date.setDate(Math.min(day, lastDay));
  return date.getTime();
}

const DATE_LABEL = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "short", day: "numeric" });

/** The words a song-title rule reads with, one per comparison it offers. */
const SONG_VERBS = {
  contains: "contains",
  "not-contains": "doesn't contain",
  words: "has whole words",
  "not-words": "doesn't have whole words",
};

const UNIT_NAMES = { days: "day", weeks: "week", months: "month", years: "year" };

/**
 * What a rule says, in the words its chip shows. Read off the rule object
 * alone — the same object the matcher reads — so the chip and the predicate
 * cannot describe two different rules, and a negation cannot reach one of them
 * and miss the other.
 */
export function ruleLabel(rule) {
  if (rule.field === "artist" || rule.field === "album") {
    const noun = rule.field === "artist" ? "Artist" : "Album";
    return `${noun} ${rule.operator === "is-not" ? "is not" : "is"} ${rule.choice.name}`;
  }
  if (rule.field === "song") return `Song ${SONG_VERBS[rule.operator]} “${rule.text}”`;
  if (rule.field === "duration") {
    const time = rule.duration % 1000 ? formatExactTime(rule.duration) : formatTime(rule.duration);
    return `${rule.operator === "shorter" ? "Shorter" : "Longer"} than ${time}`;
  }
  if (rule.field === "added-before" || rule.field === "added-after") {
    return `Added ${rule.field === "added-before" ? "before" : "after"} ${DATE_LABEL.format(rule.date)}`;
  }
  if (rule.field === "older") {
    const unit = UNIT_NAMES[rule.unit];
    return `Older than ${rule.amount} ${unit}${rule.amount === 1 ? "" : "s"}`;
  }
  return "No longer playable";
}

/** The fields whose predicate reads `added_at`. A row without one cannot be
    judged by them at all, which the preview discloses instead of guessing. */
const ADDED_FIELDS = new Set(["added-before", "added-after", "older"]);

/** Comparisons the reader asked for as the complement of a plain one. The
    negation is applied once, around the finished predicate, never inside it:
    one `!` per rule is one place for it to be right or wrong. */
const NEGATED = new Set(["is-not", "not-contains", "not-words"]);

/** Playlist browsing is the only writer of `added_at`, so it is absent on some
    entries: zero and missing both mean unknown, never a 1970 date. */
function addedAt(track) {
  const value = Number(track.added_at);
  return Number.isFinite(value) && value > 0 ? value : null;
}

function rulePredicate(rule, lenient) {
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
  if (rule.field === "song") {
    const text = normalize(rule.text);
    if (rule.operator !== "words" && rule.operator !== "not-words") {
      return (track) => normalize(track.name).includes(text);
    }
    // User text is always literal. Unicode boundaries avoid treating accented
    // letters as punctuation, unlike JavaScript's ASCII-only word boundary.
    const escaped = text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const words = new RegExp(`(?<![\\p{L}\\p{N}\\p{M}_])${escaped}(?![\\p{L}\\p{N}\\p{M}_])`, "u");
    return (track) => words.test(normalize(track.name));
  }
  if (rule.field === "unavailable") return (track) => track.unavailable === true;
  if (rule.field === "older") {
    const cutoff = rule.before;
    return (track) => {
      const added = addedAt(track);
      return added === null ? lenient : added < cutoff;
    };
  }
  const cutoff = rule.date;
  const before = rule.field === "added-before";
  return (track) => {
    const added = addedAt(track);
    return added === null ? lenient : (before ? added < cutoff : added > cutoff);
  };
}

/**
 * One rule as one predicate. `lenient` answers the single question the preview
 * has to ask about a row its added-date rules cannot see: what would this row
 * be if the missing date had matched? Counting those rows needs them
 * matchable; for every other purpose a missing date is simply not a match.
 */
function ruleMatcher(rule, lenient = false) {
  const matches = rulePredicate(rule, lenient);
  return NEGATED.has(rule.operator) ? (track) => !matches(track) : matches;
}

/** Rows and counts of an empty preview: nothing to remove, nothing to explain. */
const NO_MATCHES = {
  rows: [], uris: [], keptUris: [], removalCount: 0, duplicateCount: 0,
  extraCount: 0, missingCount: 0, undatedCount: 0,
};
const NOTHING_KEPT = new Set();

/**
 * Every entry removal would take, and exactly what it would do to them.
 *
 * Spotify removes by URI, so one entry selected takes all of its copies with
 * it. `kept` holds the URIs the reader unticked in this preview: a mark is a
 * statement about a song, not about a row, so every row of a kept URI is kept
 * with it and none of them leaves the preview — they are shown, marked, so the
 * count above the table and the copies below it never disagree.
 */
export function cleanupPreview(tracks, rules, grouping, kept = NOTHING_KEPT) {
  if (!rules.length) return NO_MATCHES;
  const matches = grouping === "all"
    ? (matchers, track) => matchers.every((one) => one(track))
    : (matchers, track) => matchers.some((one) => one(track));
  const matchers = rules.map((rule) => ruleMatcher(rule));
  const direct = tracks.map((track) => matches(matchers, track));
  /* What the same rules would say with an undecidable added date answered yes.
     Only an added rule can tell a row apart from its strict self, so the second
     set costs nothing when no added rule is on screen. */
  const lenient = rules.some((rule) => ADDED_FIELDS.has(rule.field))
    ? rules.map((rule) => ruleMatcher(rule, true))
    : null;
  const candidates = new Set();
  let missingCount = 0;
  let undatedCount = 0;
  tracks.forEach((track, index) => {
    if (!direct[index]) {
      /* Skipped *because* of the missing date, not merely beside it: the row
         this rule set would have taken had the date been there to read. */
      if (lenient && matches(lenient, track)) undatedCount += 1;
      return;
    }
    if (!track.uri) missingCount += 1;
    else candidates.add(track.uri);
  });
  const rows = [];
  let removalCount = 0;
  let extraCount = 0;
  tracks.forEach((track, index) => {
    if (!candidates.has(track.uri)) return;
    const isKept = kept.has(track.uri);
    const extra = !direct[index];
    if (!isKept) {
      removalCount += 1;
      if (extra) extraCount += 1;
    }
    rows.push({ track, index, extra, kept: isKept });
  });
  const uris = [];
  const keptUris = [];
  for (const uri of candidates) (kept.has(uri) ? keptUris : uris).push(uri);
  return {
    rows, uris, keptUris,
    /* Entries, not songs: every copy of every URI still going is an entry
       Spotify will delete, which is what the reader is being asked to allow. */
    removalCount,
    duplicateCount: removalCount - uris.length,
    extraCount, missingCount, undatedCount,
  };
}
