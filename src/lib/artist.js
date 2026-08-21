/**
 * Projects the two playlist shelves from one artist overview. Dedupe order is
 * part of the UI contract: Artist Pick wins, then Artist playlists, then
 * Discovered on. Both the overview and See-all views use this exact projection.
 */
export function artistPlaylistCollections(overview) {
  const seen = new Set();
  if (overview?.artist_pick?.id) seen.add(overview.artist_pick.id);

  function unique(items) {
    const result = [];
    for (const playlist of items ?? []) {
      if (!playlist?.id || seen.has(playlist.id)) continue;
      seen.add(playlist.id);
      result.push(playlist);
    }
    return result;
  }

  return {
    artist: unique(overview?.artist_playlists),
    discovered: unique(overview?.discovered_on),
  };
}

export function artistPlaylistSubtitle(playlist) {
  const description = String(playlist?.description ?? "")
    .replace(/<[^>]*>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (description) return description;
  if (playlist?.owner) return `By ${playlist.owner}`;
  if (playlist?.tracks_total) return `${playlist.tracks_total} songs`;
  return "Playlist";
}
