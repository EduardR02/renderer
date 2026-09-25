import { expect, test } from "bun:test";
import { BARS, curveAt, vuMeterStyle } from "./vu-meter.js";

const TICK = 1000 / 60;

test("every bar ticks on the same 60 Hz grid, in whole microseconds", () => {
  for (const [ms, delay] of BARS) {
    // 50 ms is three ticks and a whole number of microseconds.
    expect(Math.abs(ms % 50)).toBe(0);
    expect(Math.abs(delay % 50)).toBe(0);
  }
  const css = vuMeterStyle();
  BARS.forEach(([ms], i) => {
    const frames = css.match(new RegExp(`@keyframes vu-${i + 1}\\{(.*?)\\}\\}`))[1];
    const offsets = [...frames.matchAll(/([\d.]+)%\{/g)].map((m) => Number(m[1]) / 100);
    expect(offsets.length).toBe(Math.round(ms / TICK) + 1);
    for (const o of offsets) {
      const ticks = (o * ms) / TICK;
      expect(Math.abs(ticks - Math.round(ticks))).toBeLessThan(1e-3);
    }
  });
});

test("the baked curve is the eased shape, through its points", () => {
  for (const [f, v] of [[0, 0.28], [0.2, 0.9], [0.35, 0.45], [0.5, 1], [0.65, 0.38], [0.8, 0.72], [1, 0.28]]) {
    expect(curveAt(f)).toBeCloseTo(v, 6);
  }
  // Eased: a quarter of the way through a segment is well short of a quarter of it.
  const quarter = (curveAt(0.05) - 0.28) / (0.9 - 0.28);
  expect(quarter).toBeGreaterThan(0.08);
  expect(quarter).toBeLessThan(0.2);
});

test("reduced motion names no animation at all", () => {
  expect(vuMeterStyle().startsWith("@media (prefers-reduced-motion: no-preference){")).toBe(true);
});
