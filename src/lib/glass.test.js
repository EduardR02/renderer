import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { CEILING, parseFrost } from "./haze.js";

/* THE GLASS against the haze's ceiling (app.css, GLASS; haze.js, CEILING).

   The planes' frost is drawn from the haze, and the haze may put any colour
   it can produce under any line of type: a lightly blurred picture has its
   brightest colour right under the text as often as not. So the contrast is
   held against the WORST colour the haze can produce — luminance at the
   peak, chroma within the grade's cap, perceived lightness within its cap —
   frosted exactly as CSS frosts (saturate, then brightness, each clamped,
   on encoded values), under the plane's tint, the sheen, and a hovered
   row. The type's greys must keep their targets over all of it. */

const css = readFileSync(new URL("../styles/app.css", import.meta.url), "utf8");
const token = (name) => {
  const m = css.match(new RegExp(`${name}:\\s*([^;]+);`));
  if (!m) throw new Error(`no ${name}`);
  return m[1].trim();
};
const hex = (v) => [1, 3, 5].map((i) => parseInt(v.slice(i, i + 2), 16) / 255);
const rgba = (v) => {
  const m = v.match(/rgba?\(\s*([\d.]+)[ ,]+([\d.]+)[ ,]+([\d.]+)\s*[/,]\s*([\d.]+)\s*\)/);
  return { rgb: [m[1], m[2], m[3]].map((c) => Number(c) / 255), a: Number(m[4]) };
};

const lin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const Y = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
const oklab = (r, g, b) => {
  const [lr, lg, lb] = [lin(r), lin(g), lin(b)];
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
};
/* The grade's caps (haze.js): chroma at most 0.2, perceived lightness at
   most 0.62 (L plus 0.2 C, plus up to 0.2 C more toward blue-violet). */
const reachable = (r, g, b) => {
  if (Y([r, g, b]) > CEILING.peak) return false;
  const [L, A, B] = oklab(r, g, b);
  const C = Math.hypot(A, B);
  return C <= 0.2 && L + 0.2 * C + 0.2 * Math.max(0, A * Math.cos(4.8) + B * Math.sin(4.8)) <= 0.62;
};

/** The brightest frost of any colour the haze can produce. */
function brightestFrost(frost) {
  const s = frost.saturate, k = frost.brightness;
  const m = [
    0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s,
    0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s,
    0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s,
  ];
  const cl = (v) => Math.min(1, Math.max(0, v));
  let worst = [0, 0, 0], wy = -1;
  const N = 63;
  for (let i = 0; i <= N; i++) for (let j = 0; j <= N; j++) for (let q = 0; q <= N; q++) {
    const r = i / N, g = j / N, b = q / N;
    if (!reachable(r, g, b)) continue;
    const f = [0, 1, 2].map((c) => cl(cl(m[c * 3] * r + m[c * 3 + 1] * g + m[c * 3 + 2] * b) * k));
    const y = Y(f);
    if (y > wy) {
      wy = y;
      worst = f;
    }
  }
  return worst;
}

const over = (bg, rgb, a) => bg.map((v, i) => v * (1 - a) + rgb[i] * a);
const contrast = (fg, bg) => (Y(fg) + 0.05) / (Y(bg) + 0.05);

test("type on the glass planes keeps its contrast over the brightest haze", () => {
  const frost = parseFrost(token("--frost-plane"), 1);
  const floor = brightestFrost(frost);
  const sheen = Number(token("--glass-sheen").match(/rgba\(255, 255, 255, ([\d.]+)\)/)[1]);
  const hover = rgba(token("--hover"));
  const type = { "--fg": 7, "--fg-2": 4.5, "--fg-3": 3 };
  for (const plane of ["--tint-chrome", "--tint-pane"]) {
    const tint = rgba(token(plane));
    const glass = over(floor, tint.rgb, tint.a);
    for (const [where, bg] of [
      ["plain", glass],
      ["sheen", over(glass, [1, 1, 1], sheen)],
      ["hovered row", over(over(glass, [1, 1, 1], sheen), hover.rgb, hover.a)],
    ]) {
      for (const [name, target] of Object.entries(type)) {
        const c = contrast(hex(token(name)), bg);
        if (c < target) throw new Error(`${name} on ${plane}, ${where}: ${c.toFixed(2)} under ${target}`);
        expect(c).toBeGreaterThanOrEqual(target);
      }
    }
  }
});
