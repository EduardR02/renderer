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

/** The artist routes that read one and the same `detail.artist` payload. */
const ARTIST_ROUTES = new Set([
  "artist",
  "discography",
  "fans-also-like",
  "appears-on",
  "artist-playlists",
  "discovered-on",
]);

/**
 * Clears stale detail so the target view shows its loading state.
 *
 * Artist detail and every auxiliary artist route are presentations of one
 * payload. Moving among them for the same artist must keep `detail.artist`;
 * blanking it would turn a data-free route change into another page load.
 * Called before the route is applied, so `route` still describes where we are
 * coming FROM.
 */
function clearStaleDetail(entry) {
  if (entry.name === "playlist") detail.playlist = null;
  if (entry.name === "radio") detail.radio = null;
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
const CATALOGUE_PAGE_CACHE_MAX = 64;
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
  const cached = cataloguePageCache.get(key);
  if (cached) {
    // Pages contain complete track lists, so keep this in-memory cache bounded
    // while retaining recently visited catalogue and Appears On pages.
    cataloguePageCache.delete(key);
    cataloguePageCache.set(key, cached);
    return Promise.resolve(cached);
  }
  if (cataloguePagePending.has(key)) return cataloguePagePending.get(key);
  const request = api.browseArtistCatalogue(id, releaseTypes, offset, limit)
    .then((page) => {
      cataloguePageCache.set(key, page);
      while (cataloguePageCache.size > CATALOGUE_PAGE_CACHE_MAX) {
        cataloguePageCache.delete(cataloguePageCache.keys().next().value);
      }
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
  const loggedOut = observeSearchSession(payload);
  const wasPlaying = playback.playing;
  for (const key of Object.keys(playback)) {
    if (key in payload) playback[key] = payload[key];
  }
  if (loggedOut) {
    playback.username = null;
    session.username = null;
  }
  if ("position_ms" in payload) anchorPlayhead(payload.position_ms);
  if (playback.playing !== wasPlaying) syncPlayheadTicker(playback.playing);
  if ("queue" in payload) propagateCachedMarks(payload.queue);
  maybeStartDeferredSearch();
}

/**
 * Carries download marks from the queue into whatever list is on screen.
 *
 * The engine re-checks the playing track as it finishes caching (see
 * `refresh_cached_marks`) and that reaches us on the queue. But a playlist view
 * renders `detail.playlist.tracks`, a different array holding different objects
 * for the same songs, so without this the mark would appear in the queue and
 * nowhere the reader is looking.
 *
 * One-way and set-only: a track that has become cached stays marked for as long
 * as the list is open. Un-marking would mean re-deriving the whole list from a
 * payload that only speaks about two tracks, and inferring "not cached" from
 * "not mentioned" is exactly the wrong reading.
 */
function propagateCachedMarks(queue) {
  const cachedIds = new Set(
    (queue ?? []).filter((track) => track?.cached && track.id).map((track) => track.id),
  );
  if (!cachedIds.size) return;
  for (const view of [detail.playlist, detail.album, detail.artist, detail.radio]) {
    for (const track of view?.tracks ?? []) {
      if (!track.cached && cachedIds.has(track.id)) track.cached = true;
    }
    for (const track of view?.top_tracks ?? []) {
      if (!track.cached && cachedIds.has(track.id)) track.cached = true;
    }
  }
}

export function applySession(payload) {
  if (!payload) return;
  const loggedOut = observeSearchSession(payload);
  if (payload.auth_state != null) {
    session.auth_state = payload.auth_state;
    playback.auth_state = payload.auth_state;
  }
  if (loggedOut) {
    session.username = null;
    playback.username = null;
  } else if (payload.username != null) {
    session.username = payload.username;
    playback.username = payload.username;
  }
  if (payload.error != null) session.error = payload.error;
  maybeStartDeferredSearch();
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
export const detail = $state({ playlist: null, radio: null, album: null, artist: null, error: "" });

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
  // The artist page and its auxiliary/discography views share one payload;
  // arriving at any of them with it already loaded must not refetch.
  if (ARTIST_ROUTES.has(name) && detail.artist?.id === id) return;
  const seq = ++browseSeq;
  detail.error = "";
  const settle = (key) => (payload) => {
    if (seq === browseSeq) detail[key] = payload ?? null;
  };
  const fail = (reason) => {
    if (seq === browseSeq) detail.error = String(reason || "Nothing came back from the engine.");
  };
  if (name === "playlist") api.browsePlaylist(id).then(settle("playlist")).catch(fail);
  else if (name === "radio") api.browseRadio(id).then(settle("radio")).catch(fail);
  else if (name === "album") api.browseAlbum(id).then(settle("album")).catch(fail);
  else if (ARTIST_ROUTES.has(name)) {
    api.browseArtist(id).then(settle("artist")).catch(fail);
  }
}

/** The same request the route made, for a failed page's one action. */
export function retryDetail() {
  loadDetail(route.name, route.id);
}
export const search = $state({ query: "", results: null, submitted: false, busy: false, error: null });

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

const SEARCH_WAITING_MESSAGE = "Search is waiting for Spotify to connect.";
const SEARCH_FAILURE_MESSAGE = "Search could not load. Try again.";

let searchTimer = null;
let searchSeq = 0;
let searchSessionEpoch = 0;
let currentSearch = null;
let deferredSearch = null;
const searchRequests = new Map();
const LOGGED_OUT_AUTH_STATES = new Set(["logged_out", "needs_login"]);
let observedSearchSession = {
  known: false,
  authState: null,
  username: "",
  loggedOut: false,
};

function searchRequestKey(epoch, seq) {
  return `${epoch}:${seq}`;
}

function resetSearchForSession() {
  clearTimeout(searchTimer);
  searchTimer = null;
  searchSeq += 1;
  currentSearch = null;
  deferredSearch = null;
  searchRequests.clear();
  search.query = "";
  search.results = null;
  search.submitted = false;
  search.busy = false;
  search.error = null;
}

/**
 * Session events arrive separately from playback state events. Observe both,
 * but only invalidate once for a real logout or an identity change. The first
 * username after startup is not a transition: a query typed while the engine
 * was still authenticating must survive until that first session becomes ready.
 */
function observeSearchSession(payload) {
  if (!payload) return false;
  const hasAuthState = Object.prototype.hasOwnProperty.call(payload, "auth_state");
  const hasUsername = Object.prototype.hasOwnProperty.call(payload, "username");
  const authState = hasAuthState ? payload.auth_state ?? null : observedSearchSession.authState;
  const rawUsername = hasUsername
    ? String(payload.username ?? "").trim()
    : observedSearchSession.username;
  // A partial session event carrying the first username is itself evidence
  // that authentication completed; do not let a prior needs_login snapshot
  // erase the deferred query before the matching ready event arrives.
  const loggedOut = hasAuthState
    ? LOGGED_OUT_AUTH_STATES.has(authState)
    : hasUsername
      ? !rawUsername && !!observedSearchSession.username
      : observedSearchSession.loggedOut;
  const username = loggedOut ? "" : rawUsername;
  const previous = observedSearchSession;

  if (previous.known) {
    const actualLogout =
      loggedOut &&
      !previous.loggedOut &&
      (!!previous.username || previous.authState === "ready");
    const accountTransition =
      !!username &&
      !!previous.username &&
      username !== previous.username;
    if (actualLogout || accountTransition) {
      searchSessionEpoch += 1;
      resetSearchForSession();
      resetPersonalizedDiscoveryForSession();
      clearPlaylistRecommendationsCache();
    }
  }

  observedSearchSession = { known: true, authState, username, loggedOut };
  return loggedOut;
}

function isSearchReady() {
  return playback.ready === true && playback.auth_state === "ready";
}

function searchErrorMessage(reason) {
  if (typeof reason === "string" && reason.trim()) return reason;
  if (reason?.message) return reason.message;
  return SEARCH_FAILURE_MESSAGE;
}

function searchIsCurrent(query, seq, epoch) {
  return (
    currentSearch?.q === query &&
    currentSearch?.seq === seq &&
    currentSearch?.epoch === epoch &&
    epoch === searchSessionEpoch
  );
}

function requestIsCurrent(request) {
  return (
    request.epoch === searchSessionEpoch &&
    searchIsCurrent(request.q, request.seq, request.epoch) &&
    searchRequests.get(request.key) === request
  );
}

function runSearch(query, seq, epoch) {
  if (!searchIsCurrent(query, seq, epoch)) return;
  if (!isSearchReady()) {
    deferredSearch = { q: query, seq, epoch };
    search.busy = false;
    search.error = SEARCH_WAITING_MESSAGE;
    return;
  }

  const key = searchRequestKey(epoch, seq);
  const existing = searchRequests.get(key);
  if (existing) {
    // A readiness event or an explicit submit can arrive more than once. Keep
    // the one request and leave any deferred token intact until it settles.
    search.busy = true;
    search.error = null;
    return;
  }

  const request = { q: query, seq, epoch, key };
  searchRequests.set(key, request);
  search.busy = true;
  search.error = null;

  let promise;
  try {
    promise = api.search(query);
  } catch (reason) {
    promise = Promise.reject(reason);
  }

  Promise.resolve(promise)
    .then((result) => {
      if (!requestIsCurrent(request)) return;
      if (result == null) {
        search.error = SEARCH_FAILURE_MESSAGE;
        return;
      }
      search.results = result;
      search.error = null;
    })
    .catch((reason) => {
      if (!requestIsCurrent(request)) return;
      if (!isSearchReady()) {
        deferredSearch = { q: query, seq, epoch };
        search.error = SEARCH_WAITING_MESSAGE;
      } else {
        search.error = searchErrorMessage(reason);
      }
    })
    .finally(() => {
      if (searchRequests.get(request.key) === request) searchRequests.delete(request.key);
      if (searchIsCurrent(request.q, request.seq, request.epoch)) {
        search.busy = false;
      }
      // If readiness returned while this request was still cleaning up,
      // retry only after the old entry has been removed. This also keeps a
      // deferred q1 alive when q1→q2→q1 creates a fresh request token.
      if (isSearchReady()) maybeStartDeferredSearch();
    });
}

function maybeStartDeferredSearch() {
  const pending = deferredSearch;
  if (!pending) return;
  if (!searchIsCurrent(pending.q, pending.seq, pending.epoch)) {
    deferredSearch = null;
    return;
  }
  if (!isSearchReady()) return;
  // Keep the deferred token until the prior request's finally has removed its
  // epoch+sequence entry. Clearing it here would lose the readiness retry.
  if (searchRequests.has(searchRequestKey(pending.epoch, pending.seq))) return;
  deferredSearch = null;
  runSearch(pending.q, pending.seq, pending.epoch);
}

function enqueueSearch(query, force = false) {
  const q = String(query ?? "").trim();
  const current = currentSearch;

  // Input events may call this for the same value more than once. A retry for
  // an errored query is the only same-value call that should start over.
  if (q && current?.q === q && current.epoch === searchSessionEpoch && !force) {
    const key = searchRequestKey(current.epoch, current.seq);
    if (
      searchTimer !== null ||
      (deferredSearch?.q === q && deferredSearch?.seq === current.seq) ||
      searchRequests.has(key)
    ) {
      return;
    }
    if (!search.error) return;
    force = true;
  }
  if (q && current?.q === q && current.epoch === searchSessionEpoch && force) {
    const key = searchRequestKey(current.epoch, current.seq);
    if (
      searchTimer !== null ||
      (deferredSearch?.q === q && deferredSearch?.seq === current.seq) ||
      searchRequests.has(key)
    ) {
      return;
    }
  }

  clearTimeout(searchTimer);
  searchTimer = null;
  if (!q) {
    searchSeq += 1;
    currentSearch = null;
    deferredSearch = null;
    search.submitted = false;
    search.results = null;
    search.busy = false;
    search.error = null;
    return;
  }

  const seq = ++searchSeq;
  const epoch = searchSessionEpoch;
  currentSearch = { q, seq, epoch };
  deferredSearch = null;
  search.submitted = true;
  search.busy = true;
  search.error = null;
  /* `submitted` is set HERE rather than in the response handler. Otherwise
     the view checks `!submitted` first and the loading state is unreachable.
     Existing results deliberately stay visible while a refinement is pending. */
  searchTimer = setTimeout(() => {
    searchTimer = null;
    runSearch(q, seq, epoch);
  }, SEARCH_DEBOUNCE_MS);
}

export function queueSearch(query) {
  enqueueSearch(query);
}

/** Submit skips the remaining debounce without duplicating an in-flight call. */
export function submitSearch(query) {
  const raw = String(query ?? "");
  search.query = raw;
  const q = raw.trim();
  if (!q) {
    enqueueSearch("");
    return;
  }
  if (
    !currentSearch ||
    currentSearch.q !== q ||
    currentSearch.epoch !== searchSessionEpoch
  ) {
    enqueueSearch(q);
  }
  clearTimeout(searchTimer);
  searchTimer = null;
  const current = currentSearch;
  if (!current) return;
  const key = searchRequestKey(current.epoch, current.seq);
  if (searchRequests.has(key)) {
    search.busy = true;
    search.error = null;
    return;
  }
  runSearch(current.q, current.seq, current.epoch);
}

export function retrySearch() {
  const q = search.query.trim();
  if (q) enqueueSearch(q, true);
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


/* ------------------------------------------------------------------ */
/* Library activity                                                   */
/* ------------------------------------------------------------------ */

/**
 * Names Spotify uses for its algorithmic/personalized playlists. This is
 * deliberately local and explicit: Home must classify the already-loaded
 * rootlist without making a recommendation request or treating every
 * playlist as personalized by accident.
 */
const MADE_FOR_YOU_RULES = Object.freeze([
  ["discover-weekly", /^discover weekly$/],
  ["release-radar", /^release radar$/],
  ["daily-mix", /^daily mix \d+$/],
  ["on-repeat", /^on repeat$/],
  ["repeat-rewind", /^repeat rewind$/],
  ["time-capsule", /^time capsule$/],
  ["daylist", /\bdaylist\b/],
  ["your-top-songs", /^your top songs \d{4}$/],
]);

/**
 * Returns the canonical personalized-playlist kind, or `null` for an ordinary
 * library playlist. Callers pass either a Playlist object or a name string.
 */
export function classifyLibraryPlaylist(playlist) {
  const raw = typeof playlist === "string" ? playlist : playlist?.name;
  const name = String(raw ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  if (!name) return null;
  return MADE_FOR_YOU_RULES.find(([, pattern]) => pattern.test(name))?.[0] ?? null;
}

export function isMadeForYouPlaylist(playlist) {
  return classifyLibraryPlaylist(playlist) !== null;
}
/**
 * Stable Home ordering for the personalized playlists that are already in the
 * authenticated rootlist. Spotify can add more named algorithmic playlists
 * over time, so known-but-unprioritized kinds remain visible after the three
 * shelves the Home surface promises.
 *
 * The return value is a sort key, not a display label. `null` means that the
 * playlist is not a known personalized item and should stay in the library
 * grid. Daily Mixes 1-6 get their own ordered band; later mixes are treated as
 * other personalized items rather than displacing the requested six.
 */
export function personalizedPlaylistRank(playlist) {
  const kind = classifyLibraryPlaylist(playlist);
  if (!kind) return null;
  if (kind === "release-radar") return 0;
  if (kind === "daily-mix") {
    const name = String(typeof playlist === "string" ? playlist : playlist?.name ?? "")
      .trim()
      .replace(/\s+/g, " ")
      .toLowerCase();
    const number = Number(name.match(/^daily mix (\d+)$/)?.[1]);
    if (Number.isInteger(number) && number >= 1 && number <= 6) return number;
    return 200 + (Number.isInteger(number) ? number : 99);
  }
  if (kind === "discover-weekly") return 100;
  return 300;
}

/** The DJ shelf is not a user playlist recommendation and never belongs on Home. */
export function isDjPlaylist(playlist) {
  const raw = typeof playlist === "string" ? playlist : playlist?.name;
  const name = String(raw ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  return /^dj(?:\b|$)/.test(name);
}

/* ------------------------------------------------------------------ */
/* Personalized discovery                                             */
/* ------------------------------------------------------------------ */

/**
 * Home's recommendation band is deliberately assembled from the same bounded
 * search surface users can invoke themselves. The cache belongs to the
 * authenticated session, not to a component instance: leaving and returning
 * to Home must never fan out another set of searches.
 */
const PERSONALIZED_DISCOVERY_BACKOFF_MS = 5_000;
const PERSONALIZED_DISCOVERY_QUERIES = Object.freeze(["Daily Mix", "Release Radar"]);
const PERSONALIZED_SEARCH_LIMIT = 20;
const PERSONALIZED_FAILURE_MESSAGE = "Personalized playlists could not load.";

export const personalizedDiscovery = $state({
  playlists: [],
  status: "idle",
  error: "",
  retryAfter: 0,
  sessionEpoch: searchSessionEpoch,
});

let personalizedDiscoveryPromise = null;

function resetPersonalizedDiscoveryForSession() {
  personalizedDiscoveryPromise = null;
  personalizedDiscovery.playlists = [];
  personalizedDiscovery.status = "idle";
  personalizedDiscovery.error = "";
  personalizedDiscovery.retryAfter = 0;
  personalizedDiscovery.sessionEpoch = searchSessionEpoch;
}

function normalizedPlaylistName(value) {
  return String(value ?? "").trim().replace(/\s+/g, " ").toLowerCase();
}

function personalizedSearchName(name) {
  return name.includes("daily") ||
    name === "release radar" ||
    name === "discover weekly";
}

function playlistId(value) {
  const direct = String(value?.id ?? "").trim();
  if (direct) return direct;
  const uri = String(value?.uri ?? "").trim();
  return uri.match(/^spotify:playlist:([^:]+)$/)?.[1] ?? "";
}

function ownerValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function isSpotifyOwner(playlist) {
  const identities = [
    ["id", ownerValue(playlist?.owner_id)],
    ["name", ownerValue(playlist?.owner_name)],
    ["uri", ownerValue(playlist?.owner_uri)],
    ["display", ownerValue(playlist?.owner)],
  ].filter(([, value]) => value);
  if (!identities.length) return false;
  for (const [kind, raw] of identities) {
    const value = raw.toLowerCase().replace(/\s+/g, " ").trim();
    if (kind === "uri") {
      if (!/(?:^|:)spotify$/.test(value)) return false;
    } else if (!/^spotify(?:$|[\s_-])/.test(value)) {
      return false;
    }
  }
  return true;
}

function normalizeSearchPlaylist(value) {
  if (!value || typeof value !== "object") return null;
  const id = playlistId(value);
  const name = String(value.name ?? "").trim().replace(/\s+/g, " ");
  if (!id || !name || !personalizedSearchName(normalizedPlaylistName(name))) return null;
  if (!isSpotifyOwner(value)) return null;
  const ownerId = ownerValue(value.owner_id);
  const ownerName = ownerValue(value.owner_name);
  const owner = ownerValue(value.owner) || ownerName || ownerId;
  const coverUrl = ownerValue(value.cover_url);
  const coverUrls = Array.isArray(value.cover_urls)
    ? value.cover_urls.filter((url) => typeof url === "string" && url.trim())
    : [];
  const count = Number(value.tracks_total ?? value.track_count);
  return {
    ...value,
    id,
    uri: ownerValue(value.uri) || `spotify:playlist:${id}`,
    name,
    owner,
    owner_id: ownerId,
    cover_url: coverUrl,
    cover_urls: coverUrls,
    tracks_total: Number.isFinite(count) && count > 0 ? Math.round(count) : 0,
    last_played: value.last_played ?? null,
    last_activity: value.last_activity ?? null,
  };
}

function extractPersonalizedSearchPlaylists(result) {
  const values = Array.isArray(result?.playlists) ? result.playlists : [];
  return values.map(normalizeSearchPlaylist).filter(Boolean);
}

function dedupePlaylists(playlists) {
  const seen = new Set();
  const result = [];
  for (const playlist of playlists) {
    if (!playlist?.id || seen.has(playlist.id)) continue;
    seen.add(playlist.id);
    result.push(playlist);
  }
  return result;
}

function mergePlaylistMetadata(primary, supplement) {
  return {
    ...supplement,
    ...primary,
    uri: primary.uri || supplement.uri,
    owner: primary.owner || supplement.owner,
    owner_id: primary.owner_id || supplement.owner_id,
    cover_url: primary.cover_url || supplement.cover_url,
    cover_urls: primary.cover_urls?.length ? primary.cover_urls : supplement.cover_urls ?? [],
    tracks_total: Number(primary.tracks_total) > 0
      ? primary.tracks_total
      : supplement.tracks_total ?? 0,
  };
}

/**
 * Merges the authenticated rootlist with the session's search answer. Rootlist
 * metadata wins when it exists (it carries local activity), while search
 * artwork/owner/count fields fill the gaps in a rootlist reference.
 */
export function mergePersonalizedPlaylists(rootlist = library) {
  const byId = new Map();
  for (const playlist of rootlist ?? []) {
    if (
      !playlist?.id ||
      isDjPlaylist(playlist) ||
      !isMadeForYouPlaylist(playlist)
    ) {
      continue;
    }
    byId.set(playlist.id, playlist);
  }
  for (const playlist of personalizedDiscovery.playlists) {
    if (!playlist?.id || isDjPlaylist(playlist)) continue;
    const existing = byId.get(playlist.id);
    byId.set(
      playlist.id,
      existing ? mergePlaylistMetadata(existing, playlist) : playlist,
    );
  }
  return [...byId.values()].sort((left, right) => {
    const leftRank = personalizedPlaylistRank(left) ?? Number.MAX_SAFE_INTEGER;
    const rightRank = personalizedPlaylistRank(right) ?? Number.MAX_SAFE_INTEGER;
    if (leftRank !== rightRank) return leftRank - rightRank;
    return String(left.name ?? "").localeCompare(String(right.name ?? ""));
  });
}

/**
 * Starts the one Home discovery pass for the current account. Calls made
 * while this pass is running receive the same promise; a successful answer is
 * retained until the auth epoch changes. A failed pass keeps the rootlist
 * visible and can only be retried by a later Home visit after the backoff.
 */
export function ensurePersonalizedDiscovery(rootlist = library) {
  if (!isSearchReady()) return Promise.resolve(personalizedDiscovery.playlists);
  if (personalizedDiscovery.sessionEpoch !== searchSessionEpoch) {
    resetPersonalizedDiscoveryForSession();
  }
  if (personalizedDiscoveryPromise) return personalizedDiscoveryPromise;
  if (personalizedDiscovery.status === "ready") {
    return Promise.resolve(personalizedDiscovery.playlists);
  }
  if (
    personalizedDiscovery.status === "error" &&
    Date.now() < personalizedDiscovery.retryAfter
  ) {
    return Promise.resolve(personalizedDiscovery.playlists);
  }

  const epoch = searchSessionEpoch;
  const root = Array.isArray(rootlist) ? rootlist : [];
  const hasDiscoverWeekly = root.some(
    (playlist) => normalizedPlaylistName(playlist?.name) === "discover weekly",
  );
  const queries = hasDiscoverWeekly
    ? PERSONALIZED_DISCOVERY_QUERIES
    : [...PERSONALIZED_DISCOVERY_QUERIES, "Discover Weekly"];
  personalizedDiscovery.sessionEpoch = epoch;
  personalizedDiscovery.status = "loading";
  personalizedDiscovery.error = "";
  personalizedDiscovery.retryAfter = 0;

  const pass = Promise.allSettled(
    queries.map((query) =>
      Promise.resolve().then(() => api.search(query, PERSONALIZED_SEARCH_LIMIT)),
    ),
  ).then((settled) => {
    if (epoch !== searchSessionEpoch) return personalizedDiscovery.playlists;
    const found = [];
    let failures = 0;
    for (const result of settled) {
      if (result.status === "fulfilled") found.push(...extractPersonalizedSearchPlaylists(result.value));
      else failures += 1;
    }
    if (found.length) {
      personalizedDiscovery.playlists = dedupePlaylists(found);
    }
    if (failures) {
      personalizedDiscovery.status = "error";
      personalizedDiscovery.error = PERSONALIZED_FAILURE_MESSAGE;
      personalizedDiscovery.retryAfter = Date.now() + PERSONALIZED_DISCOVERY_BACKOFF_MS;
    } else {
      personalizedDiscovery.status = "ready";
      personalizedDiscovery.error = "";
      personalizedDiscovery.retryAfter = 0;
    }
    return personalizedDiscovery.playlists;
  }).finally(() => {
    if (personalizedDiscoveryPromise === pass) personalizedDiscoveryPromise = null;
  });
  personalizedDiscoveryPromise = pass;
  return pass;
}

function activityNow() {
  return Math.floor(Date.now() / 1000);
}

function activityValue(playlist) {
  const value = Number(playlist?.last_activity);
  return Number.isFinite(value) ? value : Number.NEGATIVE_INFINITY;
}

export function setLibrary(playlists) {
  const ordered = [...(playlists ?? [])];
  ordered.sort((left, right) => {
    const a = activityValue(left);
    const b = activityValue(right);
    return a === b ? 0 : b - a;
  });
  library.length = 0;
  library.push(...ordered);
  // Any answer at all, including an empty one, ends the loading state.
  // Sidebar and all library-derived views now share activity order even when
  // the first payload came from an older on-disk snapshot.
  libraryState.loaded = true;
}

/**
 * Optimistically promotes a playlist after a successful local action.
 *
 * `played` is true only for a completed play-queue command. Every other
 * promotion is library activity only, so adding/editing a playlist can never
 * create a fake Home listening-history entry.
 */
export function promotePlaylist(id, { played = false } = {}) {
  if (!id) return false;
  const index = library.findIndex((playlist) => playlist?.id === id);
  if (index < 0) return false;
  const playlist = library[index];
  const at = activityNow();
  playlist.last_activity = at;
  if (played) playlist.last_played = at;
  if (index === 0) return true;
  library.splice(index, 1);
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
  touchPlaylistActivity: (id) => invoke("touch_playlist_activity", { id }),
  browseTrackCredits: (id) => invoke("browse_track_credits", { id }),
  getCacheStats: () => invoke("get_cache_stats"),
  clearCache: (kind) => invoke("clear_cache", { kind }),
  getAppSettings: () => invoke("get_app_settings"),
  setAudioCacheLimit: (mb) => invoke("set_audio_cache_limit", { mb }),
  browsePlaylists: () => invoke("browse_playlists"),
  browseLikedSongs: (cursor = null) => invoke("browse_liked_songs", { cursor }),
  browsePlaylist: (id) => invoke("browse_playlist", { id }),
  browseRadio: (id) => invoke("browse_radio", { id }),
  browsePlaylistRecommendations: (id) => invoke("browse_playlist_recommendations", { id }),
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
/**
 * Session-scoped recommendation cache.
 *
 * Recommendations are tied to the playlist revision returned by
 * `browse_playlist`; a revision change must never reuse tracks from the old
 * snapshot. Entries keep the resolved tracks and, while a request is pending,
 * its promise so separate consumers share one server call. Rejections remove
 * the entry instead of turning a transient failure into a permanent miss.
 */
const RECOMMENDATIONS_CACHE_MAX = 64;
const playlistRecommendationsCache = new Map();

function playlistRecommendationsKey(id, revision) {
  return `${id}\u0000${revision ?? ""}`;
}

function trimRecommendationCache() {
  while (playlistRecommendationsCache.size > RECOMMENDATIONS_CACHE_MAX) {
    playlistRecommendationsCache.delete(playlistRecommendationsCache.keys().next().value);
  }
}

function storeRecommendationCacheEntry(key, playlistId, revision, entry) {
  // A playlist has one current snapshot in the cache. Removing older
  // revisions also drops their pending promises, so a slow old response
  // cannot repopulate data after a newer browse payload arrived.
  for (const [candidateKey, candidate] of playlistRecommendationsCache) {
    if (candidate.playlistId === playlistId && candidateKey !== key) {
      playlistRecommendationsCache.delete(candidateKey);
    }
  }
  playlistRecommendationsCache.delete(key);
  playlistRecommendationsCache.set(key, { playlistId, revision, ...entry });
  trimRecommendationCache();
}

function touchRecommendationCache(key, entry) {
  playlistRecommendationsCache.delete(key);
  playlistRecommendationsCache.set(key, entry);
}

function clearPlaylistRecommendationsCache() {
  playlistRecommendationsCache.clear();
}

export function loadPlaylistRecommendations(id, revision = "", { force = false } = {}) {
  const playlistId = String(id ?? "").trim();
  if (!playlistId) return Promise.resolve([]);

  const snapshot = String(revision ?? "");
  const key = playlistRecommendationsKey(playlistId, snapshot);
  const cached = playlistRecommendationsCache.get(key);

  // A refresh invalidates resolved data, but never duplicates an already
  // running request. The latter is important when an explicit refresh lands
  // at the same time as the near-footer observer.
  if (cached?.promise) {
    touchRecommendationCache(key, cached);
    return cached.promise;
  }
  if (cached && !force) {
    touchRecommendationCache(key, cached);
    return Promise.resolve(cached.tracks);
  }
  if (force) playlistRecommendationsCache.delete(key);

  let response;
  try {
    response = api.browsePlaylistRecommendations(playlistId);
  } catch (error) {
    return Promise.reject(error);
  }

  const request = Promise.resolve(response)
    .then((payload) => {
      const tracks = Array.isArray(payload?.tracks) ? payload.tracks : [];
      const current = playlistRecommendationsCache.get(key);
      if (current?.promise === request) {
        storeRecommendationCacheEntry(key, playlistId, snapshot, { tracks });
      }
      return tracks;
    })
    .catch((error) => {
      // Errors are deliberately not cached: a later near-footer demand or
      // explicit refresh gets a chance to recover from startup/transient
      // engine failures.
      if (playlistRecommendationsCache.get(key)?.promise === request) {
        playlistRecommendationsCache.delete(key);
      }
      throw error;
    });

  storeRecommendationCacheEntry(key, playlistId, snapshot, { promise: request });
  return request;
}

/** Remove an URI after a successful Add so revisiting the route cannot restore it. */
export function removePlaylistRecommendation(id, revision = "", uri) {
  const playlistId = String(id ?? "").trim();
  const recommendationUri = String(uri ?? "").trim();
  if (!playlistId || !recommendationUri) return false;
  const snapshot = String(revision ?? "");
  const key = playlistRecommendationsKey(playlistId, snapshot);
  const cached = playlistRecommendationsCache.get(key);
  if (!cached || cached.promise || !Array.isArray(cached.tracks)) return false;
  const tracks = cached.tracks.filter((track) => track?.uri !== recommendationUri);
  if (tracks.length === cached.tracks.length) return false;
  storeRecommendationCacheEntry(key, playlistId, snapshot, { tracks });
  return true;
}


/** Optimistic play/pause flip; reconciled by the next `state` event. */
export function togglePlay() {
  if (!playback.queue.length) return;
  if (playback.current_index < 0 || playback.queue[playback.current_index]?.unavailable) return;
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
  /**
   * A playlist opened from cache is served instantly and refreshed behind it.
   * This is that refresh landing.
   *
   * It is applied only if the playlist is still the one on screen, because the
   * fetch outlives the navigation that started it — and it is MERGED rather
   * than assigned. `TrackList` treats a new array as a new list and resets the
   * shared pane scroller to the top, so swapping `tracks` wholesale would yank
   * the reader back to row one a second after they opened the page and started
   * scrolling. Same rows in the same order means patch the fields in place;
   * only a genuinely different list earns a replacement, where starting at the
   * top is the honest thing to do anyway.
   */
  listen("playlist", (e) => {
    const fresh = e.payload;
    const id = fresh?.playlist?.id;
    if (!id || route.name !== "playlist" || route.id !== id) return;
    const open = detail.playlist;
    if (!open) return;

    open.playlist = fresh.playlist;

    const current = open.tracks ?? [];
    const incoming = fresh.tracks ?? [];
    const sameRows =
      current.length === incoming.length &&
      current.every((track, i) => track.id === incoming[i]?.id);

    if (!sameRows) {
      open.tracks = incoming;
      return;
    }
    /* The fields a refresh can legitimately move. Assigning only on a real
       difference keeps this from waking every row's subscribers each time a
       background refresh finds nothing new. */
    for (let i = 0; i < current.length; i++) {
      const from = incoming[i];
      const into = current[i];
      if (into.cached !== from.cached) into.cached = from.cached;
      if (into.play_count !== from.play_count) into.play_count = from.play_count;
      if (into.unavailable !== from.unavailable) into.unavailable = from.unavailable;
      if (into.unavailable_reason !== from.unavailable_reason) {
        into.unavailable_reason = from.unavailable_reason;
      }
      if (into.added_at !== from.added_at) into.added_at = from.added_at;
    }
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
