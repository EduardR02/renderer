/* =====================================================================
   THE VU METER — the playing row's four bars, at 60 frames a second

   The bars animate scaleY only, so they are compositor animations and the
   main thread does nothing while they run (app.css, VU meter). But a
   smooth curve changes the transform on every vsync — 240 times a second
   on a 240 Hz display — and every change is a draw of the window, with
   every live backdrop-filter over the bars redone. So the curve is baked
   here, one keyframe per sixtieth of a second, each held by `step-end`:
   the same curve, and the compositor only draws when a bar has changed.

   What keeps that at 60 rather than four bars' worth of 60: every bar's
   ticks are the same instants. Each duration and delay is a whole number
   of ticks — of three ticks, 50 ms, so it is also a whole number of
   microseconds, which is what the compositor counts in, and the bars'
   grids never drift apart — and the four bars of a row start, pause and
   resume in the same frame, so their grids share an origin. A curve that
   stepped at any other points (a `steps()` timing function on the
   keyframes, or an effect easing, which is not composited at all) would
   not.
   ===================================================================== */

/** One tick, in ms. */
const TICK = 1000 / 60;

/** The curve: [offset, scaleY], eased in and out between the points. */
const SHAPE = [[0, 0.28], [0.2, 0.9], [0.35, 0.45], [0.5, 1], [0.65, 0.38], [0.8, 0.72], [1, 0.28]];

/** Each bar's [duration, delay] in ms, multiples of 50: four cadences and
    phases that never visibly re-sync into a single pulse. */
export const BARS = [[1600, -300], [1300, -550], [1850, -1100], [1450, -250]];

/* `ease-in-out`, cubic-bezier(0.42, 0, 0.58, 1): x solved for t by bisection. */
const bezier = (p1, p2, t) => 3 * p1 * t * (1 - t) ** 2 + 3 * p2 * t * t * (1 - t) + t ** 3;
function easeInOut(x) {
  let lo = 0, hi = 1;
  for (let i = 0; i < 40; i++) {
    const mid = (lo + hi) / 2;
    if (bezier(0.42, 0.58, mid) < x) lo = mid;
    else hi = mid;
  }
  return bezier(0, 1, (lo + hi) / 2);
}

/** The curve's value at `f` (0..1) of an iteration. */
export function curveAt(f) {
  let j = 0;
  while (j < SHAPE.length - 2 && f >= SHAPE[j + 1][0]) j++;
  const [o0, v0] = SHAPE[j], [o1, v1] = SHAPE[j + 1];
  return v0 + (v1 - v0) * easeInOut((f - o0) / (o1 - o0));
}

/** The bars' keyframes and timings, as a stylesheet. */
export function vuMeterStyle() {
  let css = "";
  BARS.forEach(([ms, delay], i) => {
    const ticks = Math.round(ms / TICK);
    let frames = "";
    for (let t = 0; t <= ticks; t++) frames += `${((100 * t) / ticks).toFixed(5)}%{transform:scaleY(${curveAt(t / ticks).toFixed(4)})}`;
    css += `@keyframes vu-${i + 1}{${frames}}`;
    css += `.eq i:nth-child(${i + 1}){animation-name:vu-${i + 1};animation-duration:${ms}ms;animation-delay:${delay}ms}`;
  });
  /* Reduced motion keeps the bars still (app.css), so there is nothing to name. */
  return `@media (prefers-reduced-motion: no-preference){${css}}`;
}
