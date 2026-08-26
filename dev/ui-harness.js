/**
 * Copy-only browser harness for UI work without the Tauri shell.
 *
 * The bridge is installed before the real app modules load. Calls are retained
 * in window.__calls so playlist drags can be inspected as add-only actions.
 */

const calls = [];
window.__calls = calls;
window.__clearCalls = () => {
  calls.length = 0;
};

let callbackId = 1;
let listenerId = 1;
const callbacks = new Map();
const callbackOnce = new Set();
const listeners = new Map();

function transformCallback(callback, once = false) {
  const id = callbackId++;
  callbacks.set(id, callback);
  if (once) callbackOnce.add(id);
  return id;
}

function unregisterCallback(id) {
  callbacks.delete(id);
  callbackOnce.delete(id);
  for (const [eventId, listener] of listeners) {
    if (listener.callbackId === id) listeners.delete(eventId);
  }
}

function runCallback(id, payload) {
  const callback = callbacks.get(id);
  if (typeof callback !== "function") return false;
  callback(payload);
  if (callbackOnce.has(id)) unregisterCallback(id);
  return true;
}

function registerListener(event, handlerId) {
  const id = listenerId++;
  listeners.set(id, { event, callbackId: handlerId });
  return id;
}

function unregisterListener(event, eventId, handlerId) {
  const listener = listeners.get(eventId);
  if (listener && (!event || listener.event === event)) listeners.delete(eventId);
  if (handlerId != null) unregisterCallback(handlerId);
  return null;
}

function emit(event, payload) {
  let delivered = 0;
  for (const [id, listener] of listeners) {
    if (listener.event === event && runCallback(listener.callbackId, { event, id, payload })) {
      delivered += 1;
    }
  }
  return delivered;
}

window.__emit = emit;

function makeTracks(n, offset = 0) {
  return Array.from({ length: n }, (_, i) => {
    const n2 = i + offset;
    return {
      id: `t${n2}`,
      uri: `spotify:track:t${n2}`,
      name: `Song ${n2}`,
      artist_names: [`Artist ${n2 % 4}`],
      artist_ids: [`a${n2 % 4}`],
      artist_id: `a${n2 % 4}`,
      album_name: `Album ${n2 % 5}`,
      album_id: `al${n2 % 5}`,
      duration_ms: 180000 + n2 * 7000,
      added_at: Date.now() - n2 * 86400000,
      unavailable: false,
    };
  });
}

const fixtures = {
  playlists: [
    { id: "p1", name: "Road Trip", owner_id: "eduard", tracks_total: 12 },
    { id: "p2", name: "Deep Focus", owner_id: "eduard", tracks_total: 3 },
    { id: "p3", name: "Night Drive", owner_id: "eduard", tracks_total: 0 },
  ],
};
fixtures.playlistDetail = {
  id: "p1",
  name: "Road Trip",
  owner_id: "eduard",
  snapshot_id: "snap-1",
  tracks: makeTracks(12),
};
fixtures.longTrack = {
  ...makeTracks(1, 99)[0],
  name: "A Deliberately Long Track Title for Truncation and Overflow Checks",
  artist_names: ["An Artist Name Long Enough to Exercise Every Narrow Layout"],
  album_name: "An Equally Long Album Name for Dense Player Surfaces",
  duration_ms: 900000,
};

const now = Date.now();
const H = 3600_000;
fixtures.history = makeTracks(14).map((track, i) => ({
  track_id: track.id,
  started_at: now - i * (37 * 60_000) - H / 3,
  ms_played: i % 5 === 0 ? Math.floor(track.duration_ms * 0.4) : track.duration_ms,
  completed: i % 5 !== 0,
  context: ["playlist:p1", "album:al1", "search", "liked", "radio:r1"][i % 5],
  track,
}));

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function findTrack(value) {
  const id = String(value ?? "").replace("spotify:track:", "");
  for (const tracks of playlistTracks.values()) {
    const found = tracks.find((track) => track.id === id || track.uri === value);
    if (found) return found;
  }
  if (fixtures.longTrack.id === id || fixtures.longTrack.uri === value) return fixtures.longTrack;
  return null;
}

function contextTracks(tracks) {
  return tracks.map((track) => ({ ...track, context: "playlist:p1" }));
}

const playlistTracks = new Map([
  ["p1", fixtures.playlistDetail.tracks],
  ["p2", makeTracks(3, 12)],
  ["p3", []],
]);
const playlistNames = new Map(fixtures.playlists.map((playlist) => [playlist.id, playlist.name]));
const memberships = new Map();
for (const track of fixtures.playlistDetail.tracks) memberships.set(track.id, new Set(["p1"]));
memberships.get("t0").add("liked");

function editKey(trackId, playlistId = "p1") {
  return `${trackId}:${playlistId || "p1"}`;
}

function initialEdit(track) {
  return {
    definition: {
      track_id: track.id,
      duration_ms: track.duration_ms,
      cuts: [{ start_ms: 12000, end_ms: 18000 }],
      loop_range: { start_ms: 36000, end_ms: 54000, play_count: 3 },
    },
    enabled: true,
  };
}

const edits = new Map();
edits.set(editKey("t0"), initialEdit(fixtures.playlistDetail.tracks[0]));
edits.set(editKey(fixtures.longTrack.id), initialEdit(fixtures.longTrack));

function effectiveEdit(track, playlistId = "p1") {
  const saved = edits.get(editKey(track.id, playlistId));
  return saved && saved.enabled ? saved.definition : null;
}

function queueWithEdits(tracks) {
  return contextTracks(tracks).map((entry) => {
    const edit = effectiveEdit(entry);
    return edit ? { ...entry, effective_edit: clone(edit) } : entry;
  });
}

function detailFor(id) {
  const tracks = playlistTracks.get(id) || [];
  if (id === "p1") return fixtures.playlistDetail;
  return {
    id,
    name: playlistNames.get(id) || id,
    owner_id: "eduard",
    snapshot_id: `snap-${id}`,
    tracks,
  };
}

function syncPlaylistTotal(id) {
  const playlist = fixtures.playlists.find((item) => item.id === id);
  if (playlist) playlist.tracks_total = (playlistTracks.get(id) || []).length;
}

/** The refreshed library summary a real backend emits with `playlist_summary`. */
function playlistSummaryFor(id) {
  const playlist = fixtures.playlists.find((item) => item.id === id);
  return playlist
    ? { ...playlist, tracks_total: (playlistTracks.get(id) || []).length }
    : null;
}

function emitPlaylistSummary(id) {
  const summary = playlistSummaryFor(id);
  if (summary) emit("playlist_summary", clone(summary));
}

function refreshQueueEdits() {
  playback.queue = playback.queue.map((entry) => {
    const track = findTrack(entry.uri);
    const edit = track && effectiveEdit(track);
    if (edit) return { ...entry, effective_edit: clone(edit) };
    const { effective_edit: unused, ...withoutEdit } = entry;
    return withoutEdit;
  });
}

function waveformFor(trackId, durationMs) {
  const track = findTrack(trackId) || fixtures.longTrack;
  const requested = durationMs ?? track.duration_ms;
  const duration = Math.max(0, Math.ceil(Number(requested) || 0));
  const pairs = new Int16Array(duration * 2);
  for (let bin = 0; bin < duration; bin += 1) {
    const t = duration ? bin / duration : 0;
    const raw = (Math.sin(t * 61) * 0.3 + Math.sin(t * 17.3) * 0.45 + 0.55) * 30000;
    const high = Math.min(32767, Math.max(0, Math.round(raw)));
    const low = -Math.round(high * (0.55 + 0.4 * Math.abs(Math.sin(t * 7))));
    pairs[bin * 2] = low;
    pairs[bin * 2 + 1] = high;
  }
  const bytes = new Uint8Array(pairs.buffer);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return {
    track_id: trackId,
    duration_ms: duration,
    interval_ms: 1,
    bin_count: duration,
    peaks_base64: btoa(binary),
  };
}

function trackFromQueueItem(item) {
  if (typeof item === "object" && item) return item;
  const known = findTrack(item);
  if (known) return known;
  const id = String(item || "queued").replace("spotify:track:", "");
  return { ...makeTracks(1)[0], id, uri: `spotify:track:${id}`, name: `Queued ${id}` };
}

function setCurrent(index, resetPosition = true) {
  if (!playback.queue.length) {
    playback.current_index = 0;
    playback.current_uri = null;
    playback.duration_ms = 0;
    if (resetPosition) playback.position_ms = 0;
    return;
  }
  const length = playback.queue.length;
  playback.current_index = ((index % length) + length) % length;
  const current = playback.queue[playback.current_index];
  playback.current_uri = current.uri;
  const source = findTrack(current.uri);
  playback.duration_ms = source?.duration_ms ?? current.duration_ms ?? 0;
  if (resetPosition) playback.position_ms = 0;
}

const playback = {
  ready: true,
  auth_state: "ready",
  auth_url: "",
  playing: true,
  preview: false,
  username: "eduard",
  position_ms: 45000,
  duration_ms: fixtures.playlistDetail.tracks[0].duration_ms,
  volume: 70,
  shuffle: false,
  repeat: "off",
  playback_speed: 1,
  current_index: 0,
  current_uri: fixtures.playlistDetail.tracks[0].uri,
  queue: queueWithEdits(fixtures.playlistDetail.tracks),
  error: "",
};

function emitState() {
  emit("state", clone(playback));
}

function playlistIdFrom(args) {
  return args.playlistId ?? args.playlist_id ?? args.id ?? "p1";
}

function addPlaylistTracks(id, uris) {
  const target = playlistTracks.get(id) || [];
  if (!playlistTracks.has(id)) playlistTracks.set(id, target);
  let changed = false;
  for (const uri of uris) {
    const track = findTrack(uri);
    if (!track || target.some((item) => item.uri === track.uri)) continue;
    target.push(track);
    const set = memberships.get(track.id) || new Set();
    set.add(id);
    memberships.set(track.id, set);
    changed = true;
  }
  syncPlaylistTotal(id);
  if (changed) {
    emit("memberships_changed", null);
    emit("playlist", clone(detailFor(id)));
  }
  emitPlaylistSummary(id);
}

function removePlaylistTracks(id, uris) {
  const target = playlistTracks.get(id) || [];
  const wanted = new Set(uris);
  const kept = target.filter((track) => !wanted.has(track.uri) && !wanted.has(track.id));
  if (kept.length === target.length) return;
  playlistTracks.set(id, kept);
  for (const uri of uris) {
    const track = findTrack(uri);
    const set = track && memberships.get(track.id);
    if (set) set.delete(id);
  }
  if (id === "p1") fixtures.playlistDetail.tracks = kept;
  emit("memberships_changed", null);
  emit("playlist", clone(detailFor(id)));
  emitPlaylistSummary(id);
}

function updateMemberships(args, add) {
  const id = playlistIdFrom(args);
  const values = args.uris ?? args.trackUris ?? args.track_uris ?? [];
  const uris = Array.isArray(values) ? values : [values];
  if (add) addPlaylistTracks(id, uris);
  else removePlaylistTracks(id, uris);
}

window.__fixtures = fixtures;
window.__TAURI_INTERNALS__ = {
  callbacks,
  transformCallback,
  runCallback,
  unregisterCallback,
  invoke: async (cmd, args = {}) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "plugin:event|listen": {
        const handlerId = args.handler ?? args.handlerId;
        return registerListener(args.event, handlerId);
      }
      case "plugin:event|unlisten":
      case "plugin:event|remove_listener":
        return unregisterListener(args.event, args.eventId, args.handler ?? args.handlerId);
      case "get_state":
        return { playback: clone(playback), playlists: clone(fixtures.playlists), me_id: "eduard" };
      case "get_track_playlists": {
        const track = findTrack(args.uri ?? args.trackId);
        const ids = track ? [...(memberships.get(track.id) || [])] : [];
        return ids.map((id) => ({ id, name: id === "liked" ? "Liked Songs" : playlistNames.get(id) || id }));
      }
      case "browse_playlist":
        return clone(detailFor(args.id ?? args.playlistId));
      case "get_app_settings":
      case "get_settings":
        return clone(settings);
      case "set_app_settings":
      case "set_settings":
        Object.assign(settings, args.settings ?? args);
        emit("settings", clone(settings));
        return null;
      case "set_animated_canvas":
        settings.animated_canvas = Boolean(args.enabled ?? args.value);
        emit("settings", clone(settings));
        return null;
      case "get_history":
        return clone(fixtures.history);
      case "get_track_waveform":
        return waveformFor(args.trackId ?? args.track_id, args.durationMs ?? args.duration_ms);
      case "get_track_edit": {
        const saved = edits.get(editKey(args.trackId ?? args.track_id, args.playlistId ?? args.playlist_id));
        return saved ? clone(saved) : { definition: null, enabled: false };
      }
      case "save_track_edit": {
        const trackId = args.trackId ?? args.track_id;
        const playlistId = args.playlistId ?? args.playlist_id ?? "p1";
        const track = findTrack(trackId);
        const definition = {
          track_id: trackId,
          duration_ms: args.durationMs ?? args.duration_ms ?? track?.duration_ms ?? 0,
          cuts: args.cuts ?? [],
          loop_range: args.loopRange ?? args.loop_range ?? null,
        };
        const current = edits.get(editKey(trackId, playlistId));
        edits.set(editKey(trackId, playlistId), { definition, enabled: current?.enabled ?? true });
        refreshQueueEdits();
        return clone(definition);
      }
      case "delete_track_edit":
        edits.delete(editKey(args.trackId ?? args.track_id, args.playlistId ?? args.playlist_id));
        refreshQueueEdits();
        return null;
      case "set_playlist_track_edit_enabled":
      case "toggle_track_edit": {
        const trackId = args.trackId ?? args.track_id;
        const key = editKey(trackId, args.playlistId ?? args.playlist_id);
        const current = edits.get(key) || { definition: null, enabled: false };
        current.enabled = args.enabled == null ? !current.enabled : Boolean(args.enabled);
        edits.set(key, current);
        refreshQueueEdits();
        return null;
      }
      case "preview_track_edit": {
        const track = findTrack(args.trackId ?? args.track_id);
        if (track) {
          playback.preview = true;
          playback.playing = true;
          playback.current_uri = track.uri;
          playback.duration_ms = track.duration_ms;
          playback.position_ms = 0;
        }
        emitState();
        return null;
      }
      case "add_playlist_tracks":
        updateMemberships(args, true);
        return null;
      case "remove_playlist_tracks":
        updateMemberships(args, false);
        return null;
      case "touch_playlist_activity":
        emit("playlist", clone(detailFor(args.id ?? args.playlistId)));
        return null;
      case "reorder_playlist_tracks":
      case "reorder_playlist": {
        const id = playlistIdFrom(args);
        const tracks = playlistTracks.get(id) || [];
        const from = Number(args.from ?? args.fromIndex);
        const to = Number(args.to ?? args.toIndex);
        if (Number.isInteger(from) && Number.isInteger(to) && tracks[from] && to >= 0 && to < tracks.length) {
          const [track] = tracks.splice(from, 1);
          tracks.splice(to, 0, track);
          emit("playlist", clone(detailFor(id)));
          emitPlaylistSummary(id);
        }
        return null;
      }
      case "play":
        playback.playing = true;
        playback.preview = false;
        emitState();
        return null;
      case "pause":
        playback.playing = false;
        emitState();
        return null;
      case "next":
      case "play_next":
        setCurrent(playback.current_index + 1);
        playback.playing = true;
        emitState();
        return null;
      case "previous":
      case "play_previous":
        setCurrent(playback.current_index - 1);
        playback.playing = true;
        emitState();
        return null;
      case "seek":
      case "set_position":
        playback.position_ms = Number(args.positionMs ?? args.position_ms ?? args.position ?? 0);
        emitState();
        return null;
      case "set_volume":
        playback.volume = Math.max(0, Math.min(100, Number(args.percent ?? args.volume ?? 0)));
        emitState();
        return null;
      case "set_shuffle":
        playback.shuffle = Boolean(args.enabled ?? args.shuffle);
        emitState();
        return null;
      case "set_repeat":
        playback.repeat = args.mode ?? args.repeat ?? "off";
        emitState();
        return null;
      case "set_playback_speed":
        playback.playback_speed = Number(args.speed ?? 1);
        emitState();
        return null;
      case "play_queue": {
        const input = args.queue ?? args.tracks ?? args.uris;
        if (Array.isArray(input)) playback.queue = queueWithEdits(input.map(trackFromQueueItem));
        setCurrent(Number(args.index ?? args.startIndex ?? 0));
        playback.playing = true;
        playback.preview = false;
        emitState();
        return null;
      }
      case "play_queue_index":
        setCurrent(Number(args.index ?? args.queueIndex ?? 0));
        playback.playing = true;
        emitState();
        return null;
      case "add_to_queue":
      case "add_queue":
      case "add_queue_item": {
        const item = args.uri ?? args.trackUri ?? args.track_uri ?? args.track;
        playback.queue.push(...queueWithEdits([trackFromQueueItem(item)]));
        emitState();
        return null;
      }
      case "remove_queue": {
        const index = args.index ?? args.queueIndex;
        const uri = args.uri ?? args.trackUri;
        const at = index == null ? playback.queue.findIndex((item) => item.uri === uri) : Number(index);
        if (at >= 0 && at < playback.queue.length) {
          playback.queue.splice(at, 1);
          if (at < playback.current_index) playback.current_index -= 1;
          if (at === playback.current_index) setCurrent(Math.min(at, playback.queue.length - 1), false);
        }
        emitState();
        return null;
      }
      case "move_queue":
      case "reorder_queue": {
        const from = Number(args.from ?? args.fromIndex);
        const to = Number(args.to ?? args.toIndex);
        if (Number.isInteger(from) && Number.isInteger(to) && playback.queue[from] && to >= 0 && to < playback.queue.length) {
          const [entry] = playback.queue.splice(from, 1);
          playback.queue.splice(to, 0, entry);
          setCurrent(to, false);
        }
        emitState();
        return null;
      }
      case "clear_queue":
        playback.queue = [];
        setCurrent(0);
        emitState();
        return null;
      case "login":
      case "start_auth":
        playback.auth_state = "ready";
        playback.username = "eduard";
        emit("session", { auth_state: "ready", username: "eduard", error: "" });
        emitState();
        return null;
      case "logout":
      case "sign_out":
        playback.auth_state = "logged_out";
        playback.username = "";
        playback.playing = false;
        emit("session", { auth_state: "logged_out", username: "", error: "" });
        emitState();
        return null;
      default:
        return null;
    }
  },
};

const settings = { animated_canvas: false };
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: (event, eventId, handlerId) => unregisterListener(event, eventId, handlerId),
};
window.__harness = {
  calls,
  fixtures,
  callbacks,
  listeners,
  transformCallback,
  runCallback,
  unregisterCallback,
  listen: (event, handler, once = false) => {
    const handlerId = transformCallback(handler, once);
    const eventId = registerListener(event, handlerId);
    return Promise.resolve(() => unregisterListener(event, eventId, handlerId));
  },
  unlisten: unregisterListener,
  emit,
  invoke: (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args),
  getState: () => clone(playback),
};

await import("../src/styles/app.css");
const { mount } = await import("svelte");
const { default: App } = await import("../src/App.svelte");
const state = await import("../src/lib/state.svelte.js");
Object.assign(window.__harness, {
  navigate: (name, id = null, param = null) => state.navigate(name, id, param),
  openEditor: (trackId = playback.current_uri, playlistId = "p1") => {
    const track = findTrack(trackId);
    state.openTrackEditor(track, playlistId);
  },
  selectLongContent: () => {
    addPlaylistTracks("p1", [fixtures.longTrack.uri]);
    const playlist = fixtures.playlists.find((item) => item.id === "p1");
    playlist.name = "Road Trip — A Deliberately Long Playlist Name for Layout Checks";
    fixtures.playlistDetail.name = playlist.name;
    emit("library", clone(fixtures.playlists));
    emit("playlist", clone(detailFor("p1")));
    playback.queue = queueWithEdits(playlistTracks.get("p1"));
    setCurrent(playback.queue.length - 1);
    emitState();
  },
  selectEdited: () => {
    playback.queue = queueWithEdits(playlistTracks.get("p1"));
    setCurrent(0, false);
    emitState();
  },
  selectHistory: () => state.navigate("history"),
});


state.libraryState.loaded = true;
state.session.auth_state = "ready";
state.session.username = "eduard";
state.setLibrary(fixtures.playlists);

mount(App, { target: document.getElementById("app") });
state.navigate("playlist", "p1");
document.body.dataset.harnessReady = "true";

console.info("[harness] ready — commands log at window.__calls");
