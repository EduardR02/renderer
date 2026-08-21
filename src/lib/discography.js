/**
 * What the artist page's shelf and the discography reader agree on.
 *
 * The two surfaces show the same catalogue at two depths — one row of covers
 * on the artist page, every track of every record in the reader — and the
 * toggle above them is the SAME control. It carries across the navigation, so
 * its definition cannot live in either view.
 */

/**
 * `types` is the exact array the engine pages on, so a selection is not a
 * client-side filter over a fetched list — it is a different query, and
 * "Compilations" costs one page of compilations rather than a walk through
 * everything looking for them.
 */
export const GROUPS = [
  { id: "all", label: "All", types: ["albums", "singles", "compilations"] },
  { id: "albums", label: "Albums", types: ["albums"] },
  { id: "singles", label: "Singles & EPs", types: ["singles"] },
  { id: "compilations", label: "Compilations", types: ["compilations"] },
];

/** The three main-catalogue keys `release_counts` carries. Appears-on is
 * intentionally a separate artist surface, not a discography filter. */
export const RELEASE_KEYS = ["albums", "singles", "compilations"];

/**
 * The payload does not label a release, so the track count does — Spotify's
 * own rule, and it is right often enough to be useful. Summaries on the shelf
 * carry no track list at all, so there the group the release came from is the
 * better answer and this is only asked in the reader.
 */
export function releaseKind(release) {
  const count = release.tracks?.length ?? 0;
  if (count === 1) return "Single";
  if (count <= 6) return "EP";
  return "Album";
}
