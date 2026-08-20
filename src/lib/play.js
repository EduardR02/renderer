import { api, promotePlaylist } from "./state.svelte.js";

/**
 * Playing a record you can only see the cover of.
 *
 * A card in a grid knows an id and a name and nothing else — the tracks live
 * behind a browse call. Every grid had the same problem and solved it
 * differently or, in the library's and search's case, not at all: their play
 * buttons were decorative `<span>`s that did nothing when clicked. One card
 * rule (the artwork opens, the button plays) needs one implementation of the
 * button, so it lives here rather than three times in three views.
 *
 * Both throw. The caller decides whether a failure is worth a message.
 */

export async function playAlbumById(id) {
  const album = await api.browseAlbum(id);
  const tracks = album?.tracks ?? [];
  if (!tracks.length) throw new Error("No playable tracks were returned for this release.");
  await api.playQueue(tracks, 0);
}

/**
 * Liked Songs from a card, without opening the page.
 *
 * The collection is paginated and the page owns its own cursor, so this plays
 * the first page and stops there — deliberately. Anything else would mean
 * walking an unbounded collection before a single note came out, which is the
 * exact cost the lazy queue exists to avoid.
 */
export async function playLikedSongs() {
  const page = await api.browseLikedSongs(null);
  const tracks = page?.tracks ?? [];
  if (!tracks.length) throw new Error("No liked songs came back.");
  await api.playQueue(tracks, 0);
}

export async function playPlaylistById(id) {
  const playlist = await api.browsePlaylist(id);
  const tracks = playlist?.tracks ?? [];
  if (!tracks.length) throw new Error("This playlist has no playable songs.");
  // Same bookkeeping the playlist page does: library order is most recently
  // *played*, and a card is as much a play action as the big button is.
  promotePlaylist(id);
  api.touchPlaylist(id).catch(() => {});
  await api.playQueue(tracks, 0);
}
