/**
 * "Is this sticky bar currently covering content?"
 *
 * Two bars in the app need the answer — the column heads in TrackList and the
 * queue's own hand-written twin — and both need it for the same reason: a
 * sticky bar should be TYPE ON THE PAGE at rest and only take a material once
 * rows are actually passing underneath it. A bar that carries its fill at
 * every scroll position reads as a slab laid across the page.
 *
 * The mechanism is deliberate and is the only one allowed here. Not
 * `animation-timeline: scroll()` — a scroll-driven animation needs a
 * main-thread animation update per frame when it cannot be composited, and
 * that is the confirmed cause of the repaint stall this app was debugged out
 * of. Not a scroll listener either, because answering the question that way
 * means a layout read on every frame. An IntersectionObserver on a zero-size
 * sentinel parked exactly where the bar begins to stick fires twice — once
 * going down, once coming back — and costs nothing in between.
 *
 * @param sentinel  a zero-height element immediately before the sticky bar
 * @param onChange  called with `true` while the bar is stuck
 * @returns cleanup, or undefined when there is no scroll ancestor
 */
export function observeStuck(sentinel, onChange) {
  const scroller = sentinel?.closest(".scroll");
  if (!scroller) return undefined;
  /* Read the offset from the token rather than copying the number, once, at
     setup — the bar sticks under the topbar and both must agree. */
  const topbar =
    parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--topbar-h")) || 52;
  const observer = new IntersectionObserver(([entry]) => onChange(!entry.isIntersecting), {
    root: scroller,
    rootMargin: `-${topbar + 1}px 0px 0px 0px`,
  });
  observer.observe(sentinel);
  return () => observer.disconnect();
}
