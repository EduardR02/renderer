/**
 * The only Tauri commands the harness may ever forward to the owner's running
 * app. Shared by the bridge (dev/real-bridge.js), which enforces it, and by the
 * harness (dev/ui-harness-real.js), which uses it to decide what to ask for.
 *
 * Each entry was checked against src-tauri/src/commands.rs and the engine: none
 * of them touches playback, the queue, the Spotify account, or a setting. The
 * worst any of them does is refresh a local cache, noted per entry.
 *
 * Deliberately NOT here, although they read:
 *   browse_playlists       replaces the app's in-memory library and persists it;
 *                          on failure it starts a retry chain that re-emits the
 *                          library and walks every owned playlist. Nothing in
 *                          src/ calls it; the harness answers it from get_state.
 *   get_cover              downloads into the app's cover cache and returns a
 *                          cover:// path this browser cannot load; the harness
 *                          hands the remote https URL straight to <img>.
 *   status                 makes the engine re-broadcast state to the real window.
 *   cancel_track_waveform  cancels engine work.
 * Everything that mutates (play/pause/seek/queue/playlist edits/track edits/
 * settings/login/logout/touch_playlist*) stays on the harness mock.
 */
export const READ_COMMANDS = new Set([
  // In-memory snapshot of playback, library and me_id. No I/O.
  "get_state",
  // Reads settings.json.
  "get_app_settings",
  // In-memory membership lookup. No I/O.
  "get_track_playlists",
  // Engine: one window of the listening archive. No writes.
  "get_history",
  // Engine: the saved edit for a track. No writes.
  "get_track_edit",
  // Engine: fetches the audio and computes peaks; caches the result under
  // the engine's waveforms/ directory. Heavy, so it is never warmed.
  "get_track_waveform",
  // Engine: pathfinder searchDesktop. Does not touch recent searches.
  "search",
  // Engine fetch; the shell refreshes its bounded tracks cache and membership
  // index and emits display-only `playlist`/`playlist_summary` events.
  "browse_playlist",
  // Engine browse reads with no shell side effects. Canvas answers are
  // memoised in the engine's memory.
  "browse_radio",
  "browse_playlist_recommendations",
  "browse_track",
  "browse_album",
  "browse_artist",
  "browse_artist_songwriter",
  "browse_artist_catalogue",
  "browse_liked_songs",
  "browse_track_credits",
  "browse_canvas",
  "browse_followed_artists",
  // Walks the cache directories, memoised for a minute.
  "get_cache_stats",
]);
