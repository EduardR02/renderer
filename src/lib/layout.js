import { flushSync } from "svelte";

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

const reducedMotion =
  typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia("(prefers-reduced-motion: reduce)")
    : null;

/**
 * Change the shell's layout ONCE and let the compositor animate the result.
 *
 * Opening or closing the now-playing panel moves every column in the
 * window. Animating the grid would lay the whole app out on every frame of
 * the motion. A view transition lays it out once: the browser snapshots the
 * named surfaces (see THE LAYOUT MORPH in app.css), applies `update`
 * synchronously, and animates the snapshots on the compositor.
 *
 * The names exist only while `.morphing` is on the root, so at rest none of
 * the surfaces carries the stacking context a name implies.
 */
let morphs = 0;

export function morphLayout(update) {
  const root = document.documentElement;
  if (!document.startViewTransition || reducedMotion?.matches || document.hidden) {
    update();
    return;
  }
  morphs += 1;
  root.classList.add("morphing");
  const transition = document.startViewTransition(() => {
    update();
    flushSync();
  });
  /* A skipped morph — another one started mid-flight, or no frame came to
     capture — still applies `update`; only its animation is lost, so its
     rejection is not an error. The names stay while any morph needs them. */
  transition.ready.catch(() => {});
  transition.finished
    .catch(() => {})
    .finally(() => {
      morphs -= 1;
      if (!morphs) root.classList.remove("morphing");
    });
}
