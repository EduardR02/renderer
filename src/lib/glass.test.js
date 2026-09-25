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
  const lift = rgba(token("--lift-card"));
  const type = { "--fg": 7, "--fg-2": 4.5, "--fg-3": 3 };
  for (const plane of ["--tint-chrome", "--tint-pane"]) {
    const tint = rgba(token(plane));
    const glass = over(floor, tint.rgb, tint.a);
    for (const [where, bg] of [
      ["plain", glass],
      ["sheen", over(glass, [1, 1, 1], sheen)],
      ["hovered row", over(over(glass, [1, 1, 1], sheen), hover.rgb, hover.a)],
      // A card is the plane's glass with light added; the now-playing
      // album card lights to the hover under the pointer (instead of its
      // lift).
      ["card", over(over(glass, lift.rgb, lift.a), [1, 1, 1], sheen)],
      ["hovered card", over(over(glass, hover.rgb, hover.a), [1, 1, 1], sheen)],
    ]) {
      for (const [name, target] of Object.entries(type)) {
        const c = contrast(hex(token(name)), bg);
        if (c < target) throw new Error(`${name} on ${plane}, ${where}: ${c.toFixed(2)} under ${target}`);
        expect(c).toBeGreaterThanOrEqual(target);
      }
    }
  }
});

/** The body of the rule whose selector list is exactly `selector`. */
const rule = (selector) => {
  const text = css.replace(/\r\n/g, "\n");
  const at = text.indexOf(`\n${selector} {`);
  if (at < 0) throw new Error(`no rule ${selector}`);
  return text.slice(text.indexOf("{", at) + 1, text.indexOf("}", at));
};
/** `color-mix(in srgb, var(--hue) N%, transparent)` → the hue at alpha N. */
const wash = (value) => {
  const m = value.match(/var\((--[\w-]+)\)\s+([\d.]+)%/);
  return { rgb: hex(token(m[1])), a: Number(m[2]) / 100 };
};
const sheenOf = () => Number(token("--glass-sheen").match(/rgba\(255, 255, 255, ([\d.]+)\)/)[1]);

test("a selected row in the rail keeps its contrast over the brightest haze", () => {
  const floor = brightestFrost(parseFrost(token("--frost-plane"), 1));
  const tint = rgba(token("--tint-chrome"));
  const chrome = over(over(floor, tint.rgb, tint.a), [1, 1, 1], sheenOf());
  const dense = rgba(token("--tint-active"));
  const raise = rgba(token("--raise-2"));
  const type = { "--fg": 7, "--fg-2": 4.5, "--fg-3": 3 };
  for (const [selector, hue] of [
    [".nav-item.active", "--accent-wash"],
    [".lib-row.active", "--accent-wash"],
    [".lib-row.liked-row.active", "--saved-wash"],
  ]) {
    // The rule is what the numbers below model: the wash, over the raise,
    // over the denser glass.
    const body = rule(selector).replace(/\s+/g, " ");
    expect(body).toContain(`var(${hue})`);
    expect(body).toContain("linear-gradient(var(--raise-2) 0 0), var(--tint-active)");
    // At the wash's full strength, where it starts at the row's left edge.
    const w = wash(token(hue));
    const bg = over(over(over(chrome, dense.rgb, dense.a), raise.rgb, raise.a), w.rgb, w.a);
    for (const [name, target] of Object.entries(type)) {
      const c = contrast(hex(token(name)), bg);
      if (c < target) throw new Error(`${name} on ${selector}: ${c.toFixed(2)} under ${target}`);
      expect(c).toBeGreaterThanOrEqual(target);
    }
  }
});

const TYPE = { "--fg": 7, "--fg-2": 4.5, "--fg-3": 3 };
/** Every grey of the type over each ground, at its target. */
function holds(surface, grounds) {
  for (const [where, bg] of grounds) {
    for (const [name, target] of Object.entries(TYPE)) {
      const c = contrast(hex(token(name)), bg);
      if (c < target) throw new Error(`${name} on ${surface}, ${where}: ${c.toFixed(2)} under ${target}`);
      expect(c).toBeGreaterThanOrEqual(target);
    }
  }
}

/* Glass over arbitrary content has no ceiling under it: the worst it can
   frost is white, and white frosted is its brightness() (saturate leaves
   it white). */
const frostedWhite = (name) => [1, 1, 1].map((v) => v * parseFrost(token(name), 1).brightness);

test("overlays keep their contrast over a white cover", () => {
  const tint = rgba(token("--tint-overlay"));
  const base = over(frostedWhite("--frost-overlay"), tint.rgb, tint.a);
  // The material as .glass-overlay paints it: the sheen over the tint over
  // the frost, and nothing else.
  const body = rule(".glass-overlay").replace(/\s+/g, " ");
  expect(body).toContain("background: var(--glass-sheen), var(--tint-overlay);");
  expect(body).toContain("backdrop-filter: var(--frost-overlay);");
  const sheened = over(base, [1, 1, 1], sheenOf());
  holds("an overlay", [["centre", base], ["sheen", sheened]]);
  // A lit item (menus, the speed presets, the listbox, the saved-in rows) is
  // white 0.08, and its label goes to --fg.
  const lit = 0.08;
  for (const [where, bg] of [["centre", base], ["sheen", sheened]]) {
    const c = contrast(hex(token("--fg")), over(bg, [1, 1, 1], lit));
    if (c < 7) throw new Error(`--fg on a lit item, ${where}: ${c.toFixed(2)} under 7`);
    expect(c).toBeGreaterThanOrEqual(7);
  }
});

test("the now-playing details keep their contrast over a white frame of the Canvas", () => {
  // Over a cover the details are the pane's glass over the haze, which the
  // planes' test holds; over a Canvas, the tint over a live frost of the
  // video.
  const pane = rule(".np-details::before").replace(/\s+/g, " ");
  expect(pane).toContain("background: var(--tint-pane);");
  expect(pane).toContain("backdrop-filter: var(--frost-plane);");
  const body = rule(".np-panel.video .np-details::before").replace(/\s+/g, " ");
  expect(body).toContain("background: var(--tint-video);");
  expect(body).toContain("backdrop-filter: var(--frost-video);");
  const tint = rgba(token("--tint-video"));
  const glass = over(frostedWhite("--frost-video"), tint.rgb, tint.a);
  // The type is on glass cards: their light and the sheen, and the album
  // card lit to the hover under the pointer.
  const lift = rgba(token("--lift-card"));
  const hoverRule = rule(".np-album:hover:not(:disabled)").replace(/\s+/g, " ");
  expect(hoverRule).toContain("background: var(--glass-sheen), var(--hover);");
  const hover = rgba(token("--hover"));
  holds("the details", [
    ["card", over(over(glass, lift.rgb, lift.a), [1, 1, 1], sheenOf())],
    ["hovered card", over(over(glass, hover.rgb, hover.a), [1, 1, 1], sheenOf())],
  ]);
});

test("a modal sheet keeps its contrast over its own dimmed page", () => {
  const frost = parseFrost(token("--frost-sheet"), 1);
  const dim = rgba(token("--dim-modal"));
  const page = over([1, 1, 1], dim.rgb, dim.a).map((v) => v * frost.brightness);
  const tint = rgba(token("--tint-sheet"));
  const bg = over(over(page, tint.rgb, tint.a), [1, 1, 1], sheenOf());
  for (const [name, target] of Object.entries({ "--fg": 7, "--fg-2": 4.5, "--fg-3": 3 })) {
    const c = contrast(hex(token(name)), bg);
    if (c < target) throw new Error(`${name} on a sheet: ${c.toFixed(2)} under ${target}`);
    expect(c).toBeGreaterThanOrEqual(target);
  }
});
