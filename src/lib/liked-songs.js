/** Merge a cursor page or newly saved rows without replacing the rendered array. */
export function mergeLikedRows(rows, incoming, removed = new Set(), prepend = false) {
  const seen = new Set(rows.map((row) => row.uri));
  const additions = [];
  for (const row of incoming ?? []) {
    if (!row?.uri || removed.has(row.uri) || seen.has(row.uri)) continue;
    seen.add(row.uri);
    additions.push(row);
  }
  if (prepend) rows.unshift(...additions);
  else rows.push(...additions);
}

/** Reconcile a refreshed loaded prefix while retaining existing row objects. */
export function reconcileLikedRows(rows, incoming) {
  const existing = new Map(rows.map((row) => [row.uri, row]));
  const seen = new Set();
  const next = [];
  for (const row of incoming) {
    if (!row?.uri || seen.has(row.uri)) continue;
    seen.add(row.uri);
    const retained = existing.get(row.uri);
    if (retained) Object.assign(retained, row);
    next.push(retained ?? row);
  }
  rows.splice(0, rows.length, ...next);
}

/** A landed page advances only its opaque cursor, not the rendered row identity. */
export function mergeLikedPage(collection, page) {
  mergeLikedRows(collection.tracks, page?.tracks, collection.removed);
  collection.loadedPages++;
  collection.nextCursor = page?.next_cursor ?? null;
}

/** Local membership changes never invalidate loaded pages or their cursor. */
export function applyLikedDelta(collection, uris, saved, additions = []) {
  if (saved) {
    for (const uri of uris) collection.removed.delete(uri);
    mergeLikedRows(collection.tracks, additions, collection.removed, true);
  } else {
    for (const uri of uris) collection.removed.add(uri);
    for (let i = collection.tracks.length - 1; i >= 0; i--) {
      if (collection.removed.has(collection.tracks[i].uri)) collection.tracks.splice(i, 1);
    }
  }
}
