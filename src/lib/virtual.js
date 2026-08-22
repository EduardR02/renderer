/**
 * The index range a fixed-row list has to render, given where its body sits in
 * the shared pane scroller.
 *
 * Both long lists in the app — the track table and the listening history — are
 * windowed the same way and for the same reason: a body of the full height,
 * with only the rows near the viewport actually in the DOM, translated into
 * place. Neither of them is its own scroller (the pane's `.scroll` is), so the
 * offset has to be measured rather than read off a local `scrollTop`, and it is
 * measured from rects rather than cached because the header above the list
 * changes height as a page loads.
 *
 * Only the arithmetic is shared. The two lists differ in what a new range
 * *means* — the table resets its window when the playlist changes identity, the
 * history asks the engine for the pages the range lands in — and that belongs
 * with the list, not here.
 *
 * @param body      the full-height element the rows are positioned inside
 * @param scroller  the scrolling ancestor
 * @param rowH      uniform row height in CSS px
 * @param overscan  rows to keep rendered beyond each edge
 * @param length    total rows in the list
 * @returns `{ first, last }`, both clamped into the list and never crossed
 */
export function rowWindow(body, scroller, rowH, overscan, length) {
  // Layout is clean during scroll, so these reads are cheap.
  const above = scroller.getBoundingClientRect().top - body.getBoundingClientRect().top;
  const first = Math.min(Math.max(0, Math.floor(above / rowH) - overscan), length);
  const last = Math.min(
    length,
    Math.max(first, Math.ceil((above + scroller.clientHeight) / rowH) + overscan),
  );
  return { first, last };
}
