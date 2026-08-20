/* ------------------------------------------------------------------ */
/* App state — updated ONLY by Tauri events + command responses.       */
/* All command wrappers use the exact Tauri contract names.            */
/* ------------------------------------------------------------------ */
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

/* ---------------- Navigation ---------------- */

export const route = $state({ name: "library", id: null, param: null });

/**
 * Back/forward history as a plain stack plus a cursor. Deliberately not URL
 * or hash routing: nothing here is addressable or shareable, so a stack is
 * the entire feature and costs no dependency.
 */
const HISTORY_MAX = 50;
const history = $state({ entries: [{ name: "library", id: null, param: null }], cursor: 0 });

export function canGoBack() {
  return history.cursor > 0;
}

export function canGoForward() {
  return history.cursor < history.entries.length - 1;
}

/** The two routes that read one and the same `detail.artist` payload. */
const ARTIST_ROUTES = new Set(["artist", "discography"]);

/**
 * Clears stale detail so the target view shows its loading state.
 *
 * The exception is the artist page and its discography, which are two
 * presentations of ONE payload: an artist's page and its "see all" both need
 * `detail.artist`, and blanking it on the way between them would make a
 * navigation that adds no new data look like a page load. Called before the
 * route is applied, so `route` still describes where we are coming FROM.
 */
function clearStaleDetail(entry) {
  if (entry.name === "playlist") detail.playlist = null;
  if (entry.name === "album") detail.album = null;
  if (ARTIST_ROUTES.has(entry.name)) {
    const sameArtist = ARTIST_ROUTES.has(route.name) && route.id === entry.id;
    if (!sameArtist) detail.artist = null;
  }
  // A failure belongs to the page that produced it, never to the next one.
  detail.error = "";
}

function applyEntry(entry) {
  clearStaleDetail(entry);
  route.name = entry.name;
  route.id = entry.id;
  route.param = entry.param ?? null;
}

/**
 * `param` is a second, non-identifying coordinate — which discography segment
 * is selected, for instance. It rides in the history entry so back and forward
 * restore the view you actually left, but it is deliberately NOT part of the
 * "are we already here" test: re-selecting the same page with a different
 * segment is a real navigation.
 */
export function navigate(name, id = null, param = null) {
  const current = history.entries[history.cursor];
  if (current && current.name === name && current.id === id && current.param === param) return;
  // Navigating from the middle of the stack starts a new branch, so anything
  // ahead of the cursor is dropped.
  history.entries.splice(history.cursor + 1);
  history.entries.push({ name, id, param });
  if (history.entries.length > HISTORY_MAX) history.entries.shift();
  history.cursor = history.entries.length - 1;
  applyEntry({ name, id, param });
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

/**
 * `paneWidth` is the measured inner width of the content pane, published by
 * App.svelte from a ResizeObserver.
 *
 * It exists because the track table has to choose its columns from the space
 * it actually has, and that space is not the window: it is the window minus
 * the rail, minus the inspector when it is open, minus three gutters. Media
 * queries were doing that arithmetic by hand in two duplicated blocks and got
 * it wrong (see TrackList). A container query is not an option either —
 * `container-type: inline-size` implies `contain: layout`, which would make
 * the pane a containing block for `position: fixed` and reposition every row
 * menu. One observed number, read on resize and never on scroll.
 */
export const ui = $state({ searchFocusTick: 0, nowPlayingOpen: false, paneWidth: 0 });

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

const lazyQueue = $state({ generation: 0, source: null, cursor: null, loading: false, retryAfter: 0 });
const QUEUE_BACKFILL_LOW_WATER = 8;
let lazyBackfillPromise = null;
const cataloguePageCache = new Map();
const cataloguePagePending = new Map();

function clearLazyQueue() {
  lazyQueue.generation += 1;
  lazyQueue.source = null;
  lazyQueue.cursor = null;
  lazyQueue.loading = false;
  lazyQueue.retryAfter = 0;
  lazyBackfillPromise = null;
}

async function startLazyQueue(tracks, source, cursor, index = 0) {
  if (!tracks?.length) throw new Error("No playable tracks were returned.");
  clearLazyQueue();
  const generation = lazyQueue.generation;
  await invoke("play_queue", { queue: tracks, index });
  if (generation !== lazyQueue.generation) return;
  lazyQueue.source = cursor == null ? null : source;
  lazyQueue.cursor = cursor;
}

function catalogueTracks(releases) {
  return (releases ?? []).flatMap((release) => release?.tracks ?? []);
}

export function loadCataloguePage(id, releaseTypes = ["albums", "singles"], offset = 0, limit = 4) {
  const key = `${id}:${releaseTypes.join(",")}:${offset}:${limit}`;
  if (cataloguePageCache.has(key)) return Promise.resolve(cataloguePageCache.get(key));
  if (cataloguePagePending.has(key)) return cataloguePagePending.get(key);
  const request = api.browseArtistCatalogue(id, releaseTypes, offset, limit)
    .then((page) => {
      cataloguePageCache.set(key, page);
      return page;
    })
    .finally(() => cataloguePagePending.delete(key));
  cataloguePagePending.set(key, request);
  return request;
}

export async function playCatalogueContext(releases, id, releaseTypes, nextOffset, index) {
  // A catalogue is progressively discovered in release order. Global shuffle
  // would require enumerating the very catalogue this path intentionally does
  // not fetch, so this context is explicitly sequential.
  if (playback.shuffle) await invoke("set_shuffle", { enabled: false });
  await startLazyQueue(
    catalogueTracks(releases),
    { kind: "catalogue", id, releaseTypes: [...releaseTypes] },
    nextOffset,
    index,
  );
}

async function backfillLazyQueue(force = false) {
  const source = lazyQueue.source;
  const cursor = lazyQueue.cursor;
  if (!source || cursor == null) return;
  if (!force && (performance.now() < lazyQueue.retryAfter || playback.current_index < 0 || playback.queue.length - playback.current_index > QUEUE_BACKFILL_LOW_WATER)) return;
  if (lazyBackfillPromise) return lazyBackfillPromise;
  const generation = lazyQueue.generation;
  lazyQueue.loading = true;
  lazyBackfillPromise = (async () => {
    try {
      const page = await loadCataloguePage(source.id, source.releaseTypes, cursor);
      if (generation !== lazyQueue.generation) return;
      const tracks = catalogueTracks(page?.releases);
      if (tracks.length) await invoke("add_queue_batch", { tracks });
      if (generation !== lazyQueue.generation) return;
      lazyQueue.retryAfter = 0;
      lazyQueue.cursor = page?.next_offset ?? null;
      if (lazyQueue.cursor == null) lazyQueue.source = null;
    } catch (error) {
      if (generation === lazyQueue.generation) lazyQueue.retryAfter = performance.now() + 5000;
      throw error;
    } finally {
      if (generation === lazyQueue.generation) lazyQueue.loading = false;
      lazyBackfillPromise = null;
    }
  })();
  return lazyBackfillPromise;
}

export function maybeBackfillLazyQueue() {
  return backfillLazyQueue(false);
}

export const session = $state({ auth_state: null, username: null, error: null });

/* ---------------- Playhead projection ---------------- */
/* The Rust side emits a full `state` only when something other than the
   playhead changed; a plain heartbeat arrives as a `position` number. Between
   syncs the playhead is projected here off a monotonic clock, so advancing the
   progress bar costs one number assignment instead of re-parsing the whole
   queue and rebuilding its Svelte proxies once a second. */

const playhead = $state({ base_ms: 0, at: 0, now: 0 });
let playheadTimer = null;

function startPlayheadTicker() {
  if (playheadTimer !== null) return;
  playhead.now = performance.now();
  playheadTimer = setInterval(() => {
    playhead.now = performance.now();
  }, 250);
}

function stopPlayheadTicker() {
  if (playheadTimer === null) return;
  clearInterval(playheadTimer);
  playheadTimer = null;
}

function syncPlayheadTicker(playing) {
  if (playing) startPlayheadTicker();
  else stopPlayheadTicker();
}

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
  const wasPlaying = playback.playing;
  for (const key of Object.keys(playback)) {
    if (key in payload) playback[key] = payload[key];
  }
  if ("position_ms" in payload) anchorPlayhead(payload.position_ms);
  if (playback.playing !== wasPlaying) syncPlayheadTicker(playback.playing);
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
/**
 * Whether the library has been ANSWERED, as distinct from being empty.
 *
 * An empty array means both "still on its way" and "you have no playlists",
 * and those want opposite treatments: a frame of the rows that are coming, or
 * a sentence explaining that there are none. Without this flag every cold
 * start showed "No playlists yet" for the length of the round trip, which is
 * an error message for a state that is not an error.
 */
export const libraryState = $state({ loaded: false });
/**
 * The payload behind the current detail route, plus why there isn't one.
 *
 * `error` is not decoration. Every detail view renders a skeleton while its
 * payload is null, and the browse commands used to swallow their rejections —
 * so an engine that was down left a playlist page showing placeholder rows
 * for ever, with nothing on screen to say the request had failed or any way
 * to try it again. A missing payload has two causes and they need two
 * different screens.
 */
export const detail = $state({ playlist: null, album: null, artist: null, error: "" });

let browseSeq = 0;

/**
 * Fetches the payload for a detail route. Lives here rather than in App so
 * that a view's "Try again" is the same call the navigation made, rather than
 * a second implementation of it.
 *
 * The sequence guard is what stops a slow answer for a page you have already
 * left from painting over the page you are on.
 */
export function loadDetail(name = route.name, id = route.id) {
  if (!id) return;
  // The artist page and its discography share one payload; arriving at either
  // with it already loaded is the common case and must not refetch.
  if ((name === "artist" || name === "discography") && detail.artist?.id === id) return;
  const seq = ++browseSeq;
  detail.error = "";
  const settle = (key) => (payload) => {
    if (seq === browseSeq) detail[key] = payload ?? null;
  };
  const fail = (reason) => {
    if (seq === browseSeq) detail.error = String(reason || "Nothing came back from the engine.");
  };
  if (name === "playlist") api.browsePlaylist(id).then(settle("playlist")).catch(fail);
  else if (name === "album") api.browseAlbum(id).then(settle("album")).catch(fail);
  else if (name === "artist" || name === "discography") {
    api.browseArtist(id).then(settle("artist")).catch(fail);
  }
}

/** The same request the route made, for a failed page's one action. */
export function retryDetail() {
  loadDetail(route.name, route.id);
}
export const search = $state({ query: "", results: null, submitted: false, busy: false });

/**
 * On-demand track credits surface state. The payload is kept as the backend
 * returns it (`TrackCreditsDetail`): groups retain their source-provided
 * headings, contributors retain every returned name and subrole, and a
 * contributor URLs are opened verbatim and never constructed from artist ids.
 */
export const credits = $state({
  open: false,
  loading: false,
  track: null,
  data: null,
  error: null,
});
let creditsSeq = 0;

/**
 * Credits shown *inside* the now-playing panel, as opposed to the modal.
 *
 * Separate state, one shared cache. The panel is an opt-in inspector, so this
 * only ever fetches while it is open, and each track is fetched at most once
 * per session; opening the full dialog for a track the panel already loaded
 * then paints from the cache with no second request.
 */
export const trackCredits = $state({ id: null, loading: false, data: null, error: null });
const creditsCache = new Map();
let panelCreditsSeq = 0;

export function loadTrackCredits(track) {
  const id = track?.id?.trim?.() ?? "";
  if (!id) {
    trackCredits.id = null;
    trackCredits.loading = false;
    trackCredits.data = null;
    trackCredits.error = null;
    return;
  }
  if (trackCredits.id === id && (trackCredits.data || trackCredits.loading)) return;
  panelCreditsSeq += 1;
  const seq = panelCreditsSeq;
  trackCredits.id = id;
  trackCredits.error = null;
  if (creditsCache.has(id)) {
    trackCredits.data = creditsCache.get(id);
    trackCredits.loading = false;
    return;
  }
  trackCredits.data = null;
  trackCredits.loading = true;
  api
    .browseTrackCredits(id)
    .then((data) => {
      if (seq !== panelCreditsSeq) return;
      creditsCache.set(id, data ?? null);
      trackCredits.data = data ?? null;
      trackCredits.loading = false;
    })
    .catch((error) => {
      if (seq !== panelCreditsSeq) return;
      trackCredits.loading = false;
      trackCredits.error = String(error || "Could not load credits.");
    });
}


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
  /* `submitted` means "a query is in flight or has answered", and it is set
     HERE rather than in the response handler. It used to be set only once
     results came back, which made the search view's loading state literally
     unreachable — the view checks `!submitted` first, so every search showed
     the "nothing searched yet" empty state for the whole round trip and then
     snapped straight to results. Existing results are deliberately NOT
     cleared: refining a query keeps the previous answer on screen until the
     new one lands, so only the first search of a session sees a skeleton. */
  search.submitted = true;
  searchTimer = setTimeout(() => {
    const seq = ++searchSeq;
    api
      .search(q)
      .then((result) => {
        if (seq !== searchSeq) return; // a newer query already answered
        search.results = result ?? null;
      })
      .catch(() => {})
      .finally(() => {
        if (seq === searchSeq) search.busy = false;
      });
  }, SEARCH_DEBOUNCE_MS);
}

/**
 * Opens credits only after an explicit row-menu action. A sequence token
 * prevents a slow response for a closed/replaced dialog from painting stale
 * contributors into the next track's surface.
 */
export function openCredits(track) {
  const id = track?.id?.trim?.() ?? "";
  creditsSeq += 1;
  const seq = creditsSeq;
  credits.open = true;
  credits.loading = true;
  credits.track = track ?? null;
  credits.data = null;
  credits.error = null;
  if (!id) {
    credits.loading = false;
    credits.error = "Credits are unavailable for this track.";
    return;
  }
  // Opened from the now-playing panel, the payload is usually already here.
  if (creditsCache.has(id)) {
    credits.data = creditsCache.get(id);
    credits.loading = false;
    return;
  }
  api
    .browseTrackCredits(id)
    .then((data) => {
      if (seq !== creditsSeq) return;
      creditsCache.set(id, data ?? null);
      credits.data = data ?? null;
      credits.loading = false;
    })
    .catch((error) => {
      if (seq !== creditsSeq) return;
      credits.loading = false;
      credits.error = String(error || "Could not load credits.");
    });
}

export function closeCredits() {
  creditsSeq += 1;
  credits.open = false;
  credits.loading = false;
  credits.track = null;
  credits.data = null;
  credits.error = null;
}


export function setLibrary(playlists) {
  library.length = 0;
  library.push(...(playlists ?? []));
  // Any answer at all, including an empty one, ends the loading state.
  libraryState.loaded = true;
}
/**
 * Optimistically promotes the playlist that supplied a play action.
 *
 * The backend persists the timestamp through `touch_playlist`, but it does
 * not emit a library snapshot for this small local ordering change. Moving
 * the existing object in the reactive array keeps the sidebar responsive
 * without inferring ownership from a track URI (the same track can be in
 * several playlists).
 */
export function promotePlaylist(id) {
  if (!id) return false;
  const index = library.findIndex((playlist) => playlist?.id === id);
  if (index < 0) return false;
  if (index === 0) return true;
  const [playlist] = library.splice(index, 1);
  library.unshift(playlist);
  return true;
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
 * Disk-backed cache usage returned by `get_cache_stats`. The backend memoises
 * its filesystem walk, so Settings can refresh on mount/reopen without
 * repeatedly enumerating the cache directories.
 */
export const cacheStats = $state({
  audio: null,
  covers: null,
  loading: false,
  error: null,
  updatedAt: 0,
});
let cacheStatsRequest = null;

export function refreshCacheStats() {
  if (cacheStatsRequest) return cacheStatsRequest;
  cacheStats.loading = true;
  cacheStats.error = null;
  const request = api
    .getCacheStats()
    .then((payload) => {
      cacheStats.audio = payload?.audio ?? null;
      cacheStats.covers = payload?.covers ?? null;
      cacheStats.updatedAt = Date.now();
      return payload;
    })
    .catch((error) => {
      cacheStats.error = String(error || "Could not measure caches.");
      throw error;
    })
    .finally(() => {
      cacheStats.loading = false;
      cacheStatsRequest = null;
    });
  cacheStatsRequest = request;
  return request;
}

export async function clearCache(kind) {
  const payload = await api.clearCache(kind);
  cacheStats.audio = payload?.audio ?? null;
  cacheStats.covers = payload?.covers ?? null;
  cacheStats.updatedAt = Date.now();
  cacheStats.error = null;
  if (kind === "covers") {
    coverCache.clear();
    coverPending.clear();
    stats.coversResolved = 0;
  }
  return payload;
}

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
  next: async () => {
    if (lazyQueue.source && playback.current_index >= playback.queue.length - 1) {
      await backfillLazyQueue(true);
    }
    return invoke("next");
  },
  previous: () => invoke("previous"),
  seek: (ms) => {
    const target = Math.max(0, Math.round(ms));
    const wasPlaying = playback.playing;
    const previousPosition = positionMs();
    // Anchor optimistically: without this the knob snaps back to the last
    // engine sync until the next heartbeat lands, then jumps forward again.
    // Seeking never changes `playing`; the engine preserves this intent on
    // both the paused and playing paths.
    anchorPlayhead(target);
    return invoke("seek", { positionMs: target }).catch((error) => {
      // Keep the optimistic projection honest if the command is rejected,
      // especially for a paused seek where an error must not look like play.
      playback.playing = wasPlaying;
      anchorPlayhead(previousPosition);
      throw error;
    });
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
  playQueue: (queue, index) => {
    clearLazyQueue();
    return invoke("play_queue", { queue, index });
  },
  playQueueIndex: (index) => invoke("play_queue_index", { index }),
  addQueue: (track) => invoke("add_queue", { track }),
  addQueueBatch: (tracks) => invoke("add_queue_batch", { tracks }),
  removeQueue: (index) => {
    clearLazyQueue();
    return invoke("remove_queue", { index });
  },
  moveQueue: (from, to) => {
    clearLazyQueue();
    return invoke("move_queue", { from, to });
  },
  /**
   * `limit` applies to every section the server returns, not just the three we
   * parse, and it dominates search latency: measured medians are 743ms at 10
   * against 1080ms at 40, over a ~580ms irreducible server floor. It also sets
   * how many covers the results reference (tracks + albums + artists), so 10
   * asks for ~30 instead of ~120. Both halves of the win come from this number.
   */
  search: (query, limit = SEARCH_LIMIT) => invoke("search", { query, limit }),
  browseArtistCatalogue: (id, releaseTypes = ["albums", "singles"], offset = 0, limit = 4) =>
    invoke("browse_artist_catalogue", { id, releaseTypes, offset, limit }),
  touchPlaylist: (id) => invoke("touch_playlist", { id }),
  browseTrackCredits: (id) => invoke("browse_track_credits", { id }),
  getCacheStats: () => invoke("get_cache_stats"),
  clearCache: (kind) => invoke("clear_cache", { kind }),
  getAppSettings: () => invoke("get_app_settings"),
  setAudioCacheLimit: (mb) => invoke("set_audio_cache_limit", { mb }),
  browsePlaylists: () => invoke("browse_playlists"),
  browseLikedSongs: (cursor = null) => invoke("browse_liked_songs", { cursor }),
  browsePlaylist: (id) => invoke("browse_playlist", { id }),
  browseAlbum: (id) => invoke("browse_album", { id }),
  browseArtist: (id) => invoke("browse_artist", { id }),
  browseArtistReleases: (id, releaseTypes = [], offset = 0, limit = 20) =>
    invoke("browse_artist_releases", { id, releaseTypes, offset, limit }),
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
  syncPlayheadTicker(playback.playing);
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

  // Pull initial state. The engine may not be ready yet, so the cached
  // library snapshot (hydrated by the Rust side at startup) is applied here
  // too for an instant paint; the command response replaces it when fresh.
  api
    .getState()
    .then((payload) => {
      applyPlayback(payload);
      if (payload && Array.isArray(payload.playlists)) setLibrary(payload.playlists);
    })
    .catch(() => {});
  api
    .browsePlaylists()
    .then((playlists) => setLibrary(playlists))
    // A failed fetch is still an answer: leaving the rail in its loading frame
    // forever is worse than saying there is nothing there.
    .catch(() => (libraryState.loaded = true));

}
