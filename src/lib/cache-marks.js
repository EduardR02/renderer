/** Apply additive download IDs, or null for an explicit audio-cache clear.
 * Rows and their order are never replaced; IDs need not occur in the queue.
 */
export function applyCacheMarks(tracks, ids) {
  for (const track of tracks ?? []) {
    if (ids === null) track.cached = false;
    else if (ids.has(track.id)) track.cached = true;
  }
}
