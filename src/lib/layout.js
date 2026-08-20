/**
 * Sizes that depend on how much room the reading pane actually has.
 *
 * Kept out of the stylesheet because the pane's width is not the window's:
 * subtract the rail, the inspector when it is open, and three gutters. A
 * media query cannot see any of that, and the two that tried to derive it by
 * hand disagreed with each other. `ui.paneWidth` is the one measurement.
 */

/**
 * The cover on a detail header — playlist, album, Liked Songs.
 *
 * 184px is right when the title beside it has room to be 56px type. When the
 * pane is narrow the artwork is the thing that gives way: a record you have
 * already opened does not need to be recognised from across the room, but its
 * name and its controls still need to fit on one line.
 */
export function detailArtSize(pane) {
  const width = pane || 1200;
  if (width >= 760) return 184;
  if (width >= 560) return 144;
  return 104;
}
