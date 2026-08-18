/* ------------------------------------------------------------------ */
/* App state — updated ONLY by Tauri events + command responses.       */
/* All command wrappers use the exact Tauri contract names.            */
/* ------------------------------------------------------------------ */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

/* ---------------- Navigation ---------------- */

export const route = $state({ name: "library", id: null });

export function navigate(name, id = null) {
  const same = route.name === name && route.id === id;
  route.name = name;
  route.id = id;
  if (!same) {
    // Clear stale detail so the target view shows its loading state.
    if (name === "playlist") detail.playlist = null;
    if (name === "album") detail.album = null;
    if (name === "artist") detail.artist = null;
  }
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

export function applyPlayback(payload) {
  if (!payload) return;
  for (const key of Object.keys(playback)) {
    if (key in payload) playback[key] = payload[key];
  }
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
  return playback.auth_state === "logged_out" || session.auth_state === "logged_out";
}

/* ---------------- Browse data ---------------- */

export const library = $state([]);
export const detail = $state({ playlist: null, album: null, artist: null });
export const search = $state({ query: "", results: null, submitted: false });

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

export async function resolveCoverUrl(url) {
  if (!url) return null;
  if (url.startsWith("cover://")) return url;
  const hit = coverCache.get(url);
  if (hit) return hit;
  const inflight = coverPending.get(url);
  if (inflight) return inflight;
  const p = invoke("get_cover", { url })
    .then((u) => {
      if (u) coverCache.set(url, u);
      return u ?? null;
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
  seek: (positionMs) =>
    invoke("seek", { positionMs: Math.max(0, Math.round(positionMs)) }),
  setVolume: (percent) =>
    invoke("set_volume", { percent: Math.min(100, Math.max(0, Math.round(percent))) }),
  setShuffle: (enabled) => invoke("set_shuffle", { enabled: !!enabled }),
  setRepeat: (mode) => invoke("set_repeat", { mode }),
  playQueue: (queue, index) => invoke("play_queue", { queue, index }),
  addQueue: (track) => invoke("add_queue", { track }),
  removeQueue: (index) => invoke("remove_queue", { index }),
  moveQueue: (from, to) => invoke("move_queue", { from, to }),
  search: (query, limit = 40) => invoke("search", { query, limit }),
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
  playback.playing = !playback.playing;
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
}
