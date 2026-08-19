/* ------------------------------------------------------------------ */
/* App state — updated ONLY by Tauri events + command responses.       */
/* All command wrappers use the exact Tauri contract names.            */
/* ------------------------------------------------------------------ */
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

/* ---------------- Navigation ---------------- */

export const route = $state({ name: "library", id: null });

/**
 * Back/forward history as a plain stack plus a cursor. Deliberately not URL
 * or hash routing: nothing here is addressable or shareable, so a stack is
 * the entire feature and costs no dependency.
 */
const HISTORY_MAX = 50;
const history = $state({ entries: [{ name: "library", id: null }], cursor: 0 });

export function canGoBack() {
  return history.cursor > 0;
}

export function canGoForward() {
  return history.cursor < history.entries.length - 1;
}

/** Clears stale detail so the target view shows its loading state. */
function clearStaleDetail(name) {
  if (name === "playlist") detail.playlist = null;
  if (name === "album") detail.album = null;
  if (name === "artist") detail.artist = null;
}

function applyEntry(entry) {
  route.name = entry.name;
  route.id = entry.id;
  clearStaleDetail(entry.name);
}

export function navigate(name, id = null) {
  const current = history.entries[history.cursor];
  if (current && current.name === name && current.id === id) return;
  // Navigating from the middle of the stack starts a new branch, so anything
  // ahead of the cursor is dropped.
  history.entries.splice(history.cursor + 1);
  history.entries.push({ name, id });
  if (history.entries.length > HISTORY_MAX) history.entries.shift();
  history.cursor = history.entries.length - 1;
  applyEntry({ name, id });
}

export function goBack() {
  if (!canGoBack()) return;
  history.cursor -= 1;
  applyEntry(history.entries[history.cursor]);
}

export function goForward() {
  if (!canGoForward()) return;
  history.cursor += 1;
  applyEntry(history.entries[history.cursor]);
}

export const ui = $state({ searchFocusTick: 0 });

export function focusSearch() {
  if (route.name !== "search") navigate("search");
  ui.searchFocusTick += 1;
}

/* ---------------- Playback ---------------- */

export const playback = $state({
  ready: false,
  auth_state: null,
  auth_url: null,
  playing: false,
  username: null,
  position_ms: 0,
  duration_ms: 0,
  volume: 100,
  shuffle: false,
  repeat: "off",
  current_index: -1,
  current_uri: null,
  queue: [],
  error: null,
});

export const session = $state({ auth_state: null, username: null, error: null });

/* ---------------- Playhead projection ---------------- */
/* The Rust side emits a full `state` only when something other than the
   playhead changed; a plain heartbeat arrives as a `position` number. Between
   syncs the playhead is projected here off a monotonic clock, so advancing the
   progress bar costs one number assignment instead of re-parsing the whole
   queue and rebuilding its Svelte proxies once a second. */

const playhead = $state({ base_ms: 0, at: 0, now: 0 });

/** Pins the playhead to `ms` as of right now; projection restarts from here. */
function anchorPlayhead(ms) {
  const t = performance.now();
  playhead.base_ms = ms;
  playhead.at = t;
  playhead.now = t;
}

/** The playhead in ms, projected forward from the last engine sync. */
export function positionMs() {
  const elapsed = playback.playing ? Math.max(0, playhead.now - playhead.at) : 0;
  const projected = playhead.base_ms + elapsed;
  return playback.duration_ms > 0
    ? Math.min(projected, playback.duration_ms)
    : projected;
}

export function applyPlayback(payload) {
  if (!payload) return;
  for (const key of Object.keys(playback)) {
    if (key in payload) playback[key] = payload[key];
  }
  if ("position_ms" in payload) anchorPlayhead(payload.position_ms);
}

export function applySession(payload) {
  if (!payload) return;
  if (payload.auth_state != null) {
    session.auth_state = payload.auth_state;
    playback.auth_state = payload.auth_state;
  }
  if (payload.username != null) {
    session.username = payload.username;
    playback.username = payload.username;
  }
  if (payload.error != null) session.error = payload.error;
}

export function isLoggedOut() {
  // The engine emits `needs_login` (with a fresh auth_url) when no session
  // exists or the last connect attempt failed; `logged_out` is kept for
  // compatibility with older engines. Both must show the LoginView.
  return ["logged_out", "needs_login"].includes(playback.auth_state) ||
    ["logged_out", "needs_login"].includes(session.auth_state);
}

/* ---------------- Browse data ---------------- */

export const library = $state([]);
export const detail = $state({ playlist: null, album: null, artist: null });
export const search = $state({ query: "", results: null, submitted: false, busy: false });

/** See the note on `api.search`; this is the single knob that moves latency. */
export const SEARCH_LIMIT = 10;

/**
 * Debounce before a keystroke becomes a request. Every search costs at least
 * the ~580ms server floor no matter how small the limit, so firing per
 * keystroke would queue requests faster than they return. Long enough to skip
 * the middle of a word, short enough that results feel like they are keeping up.
 */
const SEARCH_DEBOUNCE_MS = 220;

let searchTimer = null;
let searchSeq = 0;

/**
 * Runs a search after the debounce, discarding replies that arrive out of
 * order. Without the sequence guard a slow early query can land after a fast
 * later one and overwrite fresher results with staler ones.
 */
export function queueSearch(query) {
  const q = query.trim();
  clearTimeout(searchTimer);
  if (!q) {
    search.submitted = false;
    search.results = null;
    search.busy = false;
    return;
  }
  search.busy = true;
  searchTimer = setTimeout(() => {
    const seq = ++searchSeq;
    api
      .search(q)
      .then((result) => {
        if (seq !== searchSeq) return; // a newer query already answered
        search.results = result ?? null;
        search.submitted = true;
      })
      .catch(() => {})
      .finally(() => {
        if (seq === searchSeq) search.busy = false;
      });
  }, SEARCH_DEBOUNCE_MS);
}

export function setLibrary(playlists) {
  library.length = 0;
  library.push(...(playlists ?? []));
}

/* ---------------- Local likes (no-op backend) ---------------- */
// Plain object: Svelte's state proxy only wraps objects/arrays (Sets are not reactive).
export const liked = $state({});

export function isTrackLiked(uri) {
  return !!liked[uri];
}

export function toggleLiked(uri) {
  if (liked[uri]) delete liked[uri];
  else liked[uri] = true;
}

/* ---------------- Cover resolution ---------------- */

const coverCache = new Map(); // remote url -> cover:// url
const coverPending = new Map(); // remote url -> Promise<string|null>

/** Reactive counters for Settings; the Maps above are not observable. */
export const stats = $state({ coversResolved: 0 });

/**
 * Turns the engine's `cover://<sha1>` into a URL the webview will actually
 * fetch. A bare custom scheme is not one of them: Tauri exposes custom
 * protocols as `http://<scheme>.localhost/<path>` on Windows and
 * `<scheme>://localhost/<path>` elsewhere, and `convertFileSrc` is what picks
 * the right shape. Handing `<img>` the raw `cover://` url fails silently on
 * every platform, which is why cover art had never rendered.
 */
function toLocalUrl(coverUrl) {
  if (!coverUrl) return null;
  if (coverUrl.startsWith("http")) return coverUrl;
  return convertFileSrc(coverUrl.replace(/^cover:\/\//, ""), "cover");
}

export async function resolveCoverUrl(url) {
  if (!url) return null;
  if (url.startsWith("cover://") || url.startsWith("http://cover.")) return toLocalUrl(url);
  const hit = coverCache.get(url);
  if (hit) return hit;
  const inflight = coverPending.get(url);
  if (inflight) return inflight;
  const p = invoke("get_cover", { url })
    .then((u) => {
      const local = toLocalUrl(u);
      if (local) {
        coverCache.set(url, local);
        stats.coversResolved = coverCache.size;
      }
      return local;
    })
    .catch(() => null)
    .finally(() => coverPending.delete(url));
  coverPending.set(url, p);
  return p;
}

/* ---------------- Commands (exact contract names) ---------------- */

export const api = {
  play: () => invoke("play"),
  pause: () => invoke("pause"),
  next: () => invoke("next"),
  previous: () => invoke("previous"),
  seek: (ms) => {
    const target = Math.max(0, Math.round(ms));
    // Anchor optimistically: without this the knob snaps back to the last
    // engine sync until the next heartbeat lands, then jumps forward again.
    anchorPlayhead(target);
    return invoke("seek", { positionMs: target });
  },
  setVolume: (percent) => {
    const target = Math.min(100, Math.max(0, Math.round(percent)));
    // Optimistic for the same reason as `seek`: the volume slider is driven by
    // `playback.volume`, so between releasing the drag and the engine's next
    // state event it would fall back to the pre-drag value and flick forward.
    playback.volume = target;
    return invoke("set_volume", { percent: target });
  },
  setShuffle: (enabled) => invoke("set_shuffle", { enabled: !!enabled }),
  setRepeat: (mode) => invoke("set_repeat", { mode }),
  playQueue: (queue, index) => invoke("play_queue", { queue, index }),
  addQueue: (track) => invoke("add_queue", { track }),
  removeQueue: (index) => invoke("remove_queue", { index }),
  moveQueue: (from, to) => invoke("move_queue", { from, to }),
  /**
   * `limit` applies to every section the server returns, not just the three we
   * parse, and it dominates search latency: measured medians are 743ms at 10
   * against 1080ms at 40, over a ~580ms irreducible server floor. It also sets
   * how many covers the results reference (tracks + albums + artists), so 10
   * asks for ~30 instead of ~120. Both halves of the win come from this number.
   */
  search: (query, limit = SEARCH_LIMIT) => invoke("search", { query, limit }),
  touchPlaylist: (id) => invoke("touch_playlist", { id }),
  browseTrackCredits: (id) => invoke("browse_track_credits", { id }),
  getCacheStats: () => invoke("get_cache_stats"),
  browsePlaylists: () => invoke("browse_playlists"),
  browsePlaylist: (id) => invoke("browse_playlist", { id }),
  browseAlbum: (id) => invoke("browse_album", { id }),
  browseArtist: (id) => invoke("browse_artist", { id }),
  createPlaylist: (name) => invoke("create_playlist", { name }),
  renamePlaylist: (id, name) => invoke("rename_playlist", { id, name }),
  deletePlaylist: (id) => invoke("delete_playlist", { id }),
  addPlaylistTracks: (id, uris) => invoke("add_playlist_tracks", { id, uris }),
  removePlaylistTracks: (id, uris) => invoke("remove_playlist_tracks", { id, uris }),
  reorderPlaylistTracks: (id, from, to) =>
    invoke("reorder_playlist_tracks", { id, from, to }),
  status: () => invoke("status"),
  login: () => invoke("login"),
  logout: () => invoke("logout"),
  getState: () => invoke("get_state"),
};

/** Optimistic play/pause flip; reconciled by the next `state` event. */
export function togglePlay() {
  if (!playback.queue.length) return;
  // Capture where the playhead actually is before the flip, so resuming
  // projects from there rather than from the last engine sync.
  const at = positionMs();
  playback.playing = !playback.playing;
  anchorPlayhead(at);
  invoke(playback.playing ? "play" : "pause").catch(() => {});
}

export function openAuthUrl() {
  if (playback.auth_url) openUrl(playback.auth_url);
}

/* ---------------- Event wiring ---------------- */

let bootstrapped = false;

export async function initEvents() {
  if (bootstrapped) return;
  bootstrapped = true;

  listen("state", (e) => applyPlayback(e.payload)).catch(() => {});
  // Heartbeats that only moved the playhead: a bare number, no queue.
  listen("position", (e) => {
    playback.position_ms = e.payload ?? 0;
    anchorPlayhead(playback.position_ms);
  }).catch(() => {});
  listen("session", (e) => applySession(e.payload)).catch(() => {});
  listen("library", (e) => setLibrary(e.payload)).catch(() => {});
  listen("playlist-tracks", (e) => {
    detail.playlist = e.payload ?? null;
  }).catch(() => {});
  listen("album", (e) => {
    detail.album = e.payload ?? null;
  }).catch(() => {});
  listen("artist", (e) => {
    detail.artist = e.payload ?? null;
  }).catch(() => {});
  listen("search-results", (e) => {
    search.results = e.payload ?? null;
    search.submitted = true;
  }).catch(() => {});

  // Pull initial state. The engine may not be ready yet, so the cached
  // library snapshot (hydrated by the Rust side at startup) is applied here
  // too for an instant paint; the fresh `library` event replaces it once the
  // engine reports ready.
  api
    .getState()
    .then((payload) => {
      applyPlayback(payload);
      if (payload && Array.isArray(payload.playlists)) setLibrary(payload.playlists);
    })
    .catch(() => {});
  api.browsePlaylists().catch(() => {});

  // Advance the projected playhead. 250ms reads as smooth on a progress bar
  // and touches exactly one number; nothing else in the tree invalidates.
  setInterval(() => {
    if (playback.playing) playhead.now = performance.now();
  }, 250);
}
