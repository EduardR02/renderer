/**
 * Browser harness for UI work without the Tauri shell.
 *
 * Installs a fake `window.__TAURI_INTERNALS__` bridge BEFORE any app module
 * evaluates, then mounts the REAL `App.svelte` against fixture data. Every
 * command the app fires is recorded in `window.__calls`, so behaviour can be
 * asserted (e.g. that a cross-playlist drag produced add+remove, in that
 * order) rather than eyeballed.
 *
 * Serve with `bun run dev` and open http://localhost:1420/dev/ui-harness.html
 */

const calls = [];
window.__calls = calls;
window.__clearCalls = () => (calls.length = 0);

let cbId = 1;
const eventHandlers = new Map();

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

/* History fixture: a believable listening day. */
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

/* Waveform fixture: packed little-endian (min,max) i16 pairs at 10ms bins —
   the exact contract decodeTrackWaveform validates. A smooth pseudo-song so
   the canvas has structure to draw. */
fixtures.waveformFor = (trackId, durationMs) => {
  const bins = Math.floor(durationMs / 10);
  const pairs = new Int16Array(bins * 2);
  for (let b = 0; b < bins; b += 1) {
    const t = b / bins;
    const amp = (Math.sin(t * 61) * 0.3 + Math.sin(t * 17.3) * 0.45 + 0.55) * 30000;
    const lo = -Math.round(amp * (0.55 + 0.4 * Math.abs(Math.sin(t * 7))));
    pairs[b * 2] = lo;
    pairs[b * 2 + 1] = Math.round(amp);
  }
  const bytes = new Uint8Array(pairs.buffer);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 1) bin += String.fromCharCode(bytes[i]);
  return { track_id: trackId, duration_ms: durationMs, interval_ms: 1, bin_count: bins, peaks_base64: btoa(bin) };
};


/* Queue entries carry the source context the player bar and history read. */
function contextTracks(tracks) {
  return tracks.map((t) => ({ ...t, context: "playlist:p1" }));
}
window.__fixtures = fixtures;

window.__TAURI_INTERNALS__ = {
  transformCallback(cb) {
    const id = cbId++;
    eventHandlers.set(id, cb);
    return id;
  },
  async invoke(cmd, args = {}) {
    calls.push({ cmd, args });
    switch (cmd) {
      case "plugin:event|listen":
      case "plugin:event|unlisten":
        return cbId++;
      case "get_state":
        return {
          ready: true,
          auth_state: "ready",
          username: "eduard",
          playing: true,
          position_ms: 45000,
          duration_ms: fixtures.playlistDetail.tracks[0].duration_ms,
          volume: 70,
          shuffle: false,
          repeat: "off",
          playback_speed: 1,
          current_index: 0,
          current_uri: fixtures.playlistDetail.tracks[0].uri,
          queue: contextTracks(fixtures.playlistDetail.tracks),
          playlists: fixtures.playlists,
        };
      case "get_track_playlists":
        return [
          { id: "liked", name: "Liked Songs" },
          { id: "p1", name: "Road Trip" },
        ];
      case "browse_playlist":
        return JSON.parse(JSON.stringify(fixtures.playlistDetail));
      case "get_app_settings":
        return { animated_canvas: false };
      case "get_history":
        return JSON.parse(JSON.stringify(fixtures.history));
      case "get_track_waveform":
        return fixtures.waveformFor(args.trackId, 214000);
      case "get_track_edit":
        return { definition: null, enabled: false };
      case "save_track_edit":
        return {
          track_id: args.trackId,
          duration_ms: args.durationMs,
          cuts: args.cuts ?? [],
          loop_range: args.loopRange ?? null,
        };
    }
  },
};

/* App modules are imported AFTER the bridge exists. */
import "../src/styles/app.css";

const { mount } = await import("svelte");
const { default: App } = await import("../src/App.svelte");
const state = await import("../src/lib/state.svelte.js");

state.libraryState.loaded = true;
state.session.auth_state = "ready";
state.session.username = "eduard";
state.setLibrary(fixtures.playlists);

mount(App, { target: document.getElementById("app") });
state.navigate("playlist", "p1");

console.info("[harness] ready — commands log at window.__calls");
