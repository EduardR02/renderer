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
          playing: false,
          position_ms: 0,
          volume: 70,
          shuffle: false,
          repeat: "off",
          playback_speed: 1,
          queue: [],
          playlists: fixtures.playlists,
        };
      case "browse_playlists":
        return JSON.parse(JSON.stringify(fixtures.playlists));
      case "browse_playlist":
        return JSON.parse(JSON.stringify(fixtures.playlistDetail));
      case "get_app_settings":
        return { animated_canvas: false };
      default:
        return null; // every edit command "succeeds"
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
