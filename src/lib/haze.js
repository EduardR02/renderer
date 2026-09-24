/* =====================================================================
   THE HAZE — arithmetic

   One picture of what is playing sits behind every panel, and the panels
   are glass over it (Ambient.svelte, haze.worker.js). This module is the
   part of that which is arithmetic: where each pixel of the haze takes its
   light from, how bright it may be, and how it is quantised. Pure
   functions, so the worker and the tests share them.

   The haze is the picture itself, seen through soft glass — its shapes,
   edges and gradients, not a few blobs of its colour. The source is read
   at a real resolution and GRADED first, pixel by pixel in Oklab: keyed low
   against its own median, so a light picture keeps its light and shade and
   a dark one its mood, with chroma kept and the fine detail laid back over
   the compressed range (local contrast). Then it is LAID OUT: under the
   video exactly as the video shows it, and to its left as its own mirror
   image — continuous at the video's edge — magnified with distance, so the
   window is one large view of the picture rather than repeated copies.
   Then it is blurred lightly, once, and held under the ceilings.

   Blur happens in LINEAR light, as a lens does it: a convex mix of linear
   colours is never brighter than its brightest input, so the ceilings hold
   through it; the chroma the mix cancels is given back. Quantisation to 8
   bits is dithered, which is what keeps a dark gradient stretched over a
   window from breaking into bands.
   ===================================================================== */

/**
 * The ceilings, in linear (WCAG relative) luminance.
 *
 * PEAK bounds every pixel of the haze and MEAN bounds its average. Both are
 * needed: a colourful video has a few bright regions and a dark body, and
 * only the peak constrains it; a white video is ALL peak, and a peak-only
 * ceiling would light the whole window to it. 0.10 is the grey #595959,
 * 0.045 is #3c3c3c. PEAK holds for every output pixel AFTER dithering, and
 * the glass tints are calibrated against it: any sRGB colour with relative
 * luminance at most PEAK can be behind any glass surface.
 */
export const CEILING = { peak: 0.1, mean: 0.045 };
/** The panel-closed glow is a hint of the record, not a light source. */
export const RESTRAINED = 0.5;
/** The long side of the source as the worker reads it, in pixels: enough
    for the picture's shapes to survive a light lens at window size. */
export const PROBE = 360;

const lin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const enc = (y) => (y <= 0.0031308 ? y * 12.92 : 1.055 * y ** (1 / 2.4) - 0.055);
const clamp01 = (v) => Math.min(1, Math.max(0, v));

/** Relative luminance of an encoded sRGB triple (0..1). */
export function luminance(r, g, b) {
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/* The decode, from tables: one for arbitrary encoded values (4096 steps is
   finer than the 8-bit values it is ever fed), one exact for bytes. */
const LUT = new Float32Array(4097);
for (let i = 0; i <= 4096; i++) LUT[i] = lin(i / 4096);
const linFast = (v) => LUT[v >= 1 ? 4096 : v <= 0 ? 0 : (v * 4096 + 0.5) | 0];
const LIN8 = new Float32Array(256);
for (let i = 0; i < 256; i++) LIN8[i] = lin(i / 255);
/* The encode, from a table: linear light (0..1) to sRGB, interpolated. */
const ENC = new Float32Array(16385);
for (let i = 0; i <= 16384; i++) ENC[i] = enc(i / 16384);
function encFast(y) {
  if (y <= 0) return 0;
  if (y >= 1) return 1;
  const f = y * 16384, i = f | 0;
  return ENC[i] + (ENC[i + 1] - ENC[i]) * (f - i);
}
const lum8 = (d, i) => 0.2126 * LIN8[d[i]] + 0.7152 * LIN8[d[i + 1]] + 0.0722 * LIN8[d[i + 2]];

/** `#rrggbb` → encoded [r, g, b] in 0..1. */
export function rgbOf(hex) {
  const n = parseInt(String(hex).replace("#", ""), 16);
  if (!Number.isFinite(n)) return [1, 1, 1];
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

/* --- Oklab ------------------------------------------------------------ */

/** LINEAR sRGB → Oklab, into `o`. */
function labOfLinear(lr, lg, lb, o) {
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  o[0] = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  o[1] = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  o[2] = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return o;
}

/** Oklab → LINEAR sRGB, unclamped (out of [0, 1] means out of gamut), into `o`. */
function linearOfLab(L, A, B, o) {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  o[0] = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
  o[1] = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
  o[2] = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
  return o;
}

/** Encoded sRGB (0..1) → Oklab [L, a, b]. */
export function toOklab(r, g, b) {
  return labOfLinear(linFast(r), linFast(g), linFast(b), [0, 0, 0]);
}

/** Oklab → encoded sRGB (0..1), clamped. */
export function fromOklab(L, A, B) {
  const [r, g, b] = linearOfLab(L, A, B, [0, 0, 0]);
  return [enc(clamp01(r)), enc(clamp01(g)), enc(clamp01(b))];
}

/* --- The fold ---------------------------------------------------------- */

/**
 * The arc with no dark form, and the two edges where colour comes back.
 *
 * Every other hue at L 0.35 is a recognisable dark version of itself: 200° is
 * teal, 300° is aubergine, 14° is oxblood. This arc is the exception, and not
 * because of a bad choice of lightness — "amber" IS a light yellow, so there
 * is no dark amber, there is only brown. The failure runs the whole way from
 * burnt orange through olive; at the identity chroma, 68° is #5a2e00, 118° is
 * #374000 and 130° is still #294400. Warm survives up to about 38 and green
 * comes back at about 138, and between those two numbers there is nothing
 * worth showing.
 */
const ARC_LO = 38;
const ARC_HI = 156;
const WARM_TIP = 54;
const GREEN_TOE = 138;
/** Pure yellow, and the middle of the arc: the one hue with no flank to prefer. */
const FOLD = (ARC_LO + ARC_HI) / 2;

/**
 * Vacate the arc by FOLDING it onto its own two edges.
 *
 * The thing to understand first is that no continuous monotonic remap can do
 * this. Push, squeeze, ease it however you like — if the map is continuous and
 * increasing, some input still lands in the middle, because that is what
 * continuity means.
 *
 * So the map folds. Below the fold hues run down the warm flank towards burnt
 * sienna; above it they run up the green flank towards pine. It is continuous
 * at 38 and at 156, which is the part that matters, because those are the
 * seams with the untouched rest of the wheel. The one discontinuity sits at
 * the fold itself, where a hue just below it opens burnt orange and one just
 * above it opens forest green.
 *
 * The cost is real and it is spread: 118° of input arrive on 34° of output, so
 * two gold sleeves that differ slightly now open the same page. That is the
 * trade being made on purpose — the arc had no variation worth keeping, only
 * different browns.
 */
export function warpHue(hue, fold = FOLD) {
  const h = ((hue % 360) + 360) % 360;
  if (h < ARC_LO || h > ARC_HI) return h;
  if (h < fold) return ARC_LO + ((h - ARC_LO) / (fold - ARC_LO)) * (WARM_TIP - ARC_LO);
  return GREEN_TOE + ((h - fold) / (ARC_HI - fold)) * (ARC_HI - GREEN_TOE);
}

/**
 * How far up the warm flank a WARPED hue landed, 0 at 38° and 1 at 54°. The
 * warm flank ends on a hue that still reads as brown until it is both
 * lighter and less saturated (see covertone's palette); this is the ramp
 * that correction is applied along, from zero at the seam with the rest of
 * the wheel. The green flank needs nothing: 138° dark is already a pine.
 */
export function clayOf(h) {
  return h > WARM_TIP ? 0 : Math.max(0, (h - ARC_LO) / (WARM_TIP - ARC_LO));
}

/* --- Tone ------------------------------------------------------------- */

/** Below this chroma (at mid lightness) a pixel has no hue of its own and
    takes the record's; darker pixels need less to count as coloured. */
const C_GREY = 0.04;
/** Warm light: the source chroma at which orange is only a tint (pale), and
    at which it is a colour in its own right (rich); and the chroma a pale
    warm source is left with. */
const WARM_PALE = 0.05;
const WARM_RICH = 0.13;
const WARM_BREATH = 0.018;
/** How far an output hue is in the warm band that darkens into brown: from
    orange-red (easing in from 22°, so pure red at 29° is barely touched)
    through the fold's warm flank, which ends at 54°. */
function warmth(hue) {
  const h = ((hue % 360) + 360) % 360;
  if (h < 22 || h > 64) return 0;
  return smooth(Math.min((h - 22) / 14, (64 - h) / 6));
}
/** Where the haze's fold pivots: pale yellow light is warm, and a hazy sky
    sitting on pure yellow would otherwise split into sienna and pine. */
const HAZE_FOLD = 125;
/** Every pixel is toned this far under the peak, so that dithering, which
    can add up to one 8-bit level per channel, never crosses it. */
const DITHER_ROOM = 0.97;

const scratchLin = [0, 0, 0];

/* The grade. The picture is keyed LOW against its own median: its shadows
   land on dark colour, its highlights reach up toward the luminous end,
   and its body sits in between. A light picture therefore keeps its light
   and shade instead of becoming one bright wash, and a dark one is not
   lifted into grey. */
const G_SHADOW = 0.13;
const G_MID = 0.3;
const G_LIGHT = 0.58;
const G_HI_GAMMA = 1.3;
/** Chroma keeps more of itself than shading would when a colour is darkened
    (sqrt of the lightness ratio, not the ratio), then this boost, then a cap. */
const G_CHROMA = 1.5;
const G_CMAX = 0.2;
/** Vibrance: a weak colour (a pastel) is lifted more than a strong one, up
    to this much more, so pastel light is still colour when darkened. */
const G_VIBRANCE = 0.7;
/** Mean chroma over what is seen: a picture that is one strong colour all
    over would be a wash of it; past this budget chroma is taken back. */
const C_MEAN = 0.07;
/** Perceived lightness: a saturated blue at a given L looks far brighter
    than a grey of it (Helmholtz–Kohlrausch). Capped per pixel, and in the
    mean over what is seen. */
const P_PEAK = 0.62;
const P_MEAN = 0.33;
/** Local contrast. The grade compresses the picture's whole range and
    would flatten its edges with it; detail finer than LOCAL_SIGMA (of the
    frame's long side) is given back at LOCAL_GAIN of the source's own. */
const LOCAL_SIGMA = 0.05;
const LOCAL_GAIN = 0.8;

/** Oklab L plus the glow of chroma, strongest in blue-violet (about 275°):
    0.2 C everywhere, and up to 0.2 C more by the cosine to that hue. */
const GLOW_A = Math.cos(4.8), GLOW_B = Math.sin(4.8);
function perceived(L, A, B) {
  return L + 0.2 * Math.sqrt(A * A + B * B) + 0.2 * Math.max(0, A * GLOW_A + B * GLOW_B);
}

/** The picture's lightness: its shadows, median and highlights. */
function exposureOf(lab, n) {
  const Ls = new Float32Array(n);
  for (let i = 0; i < n; i++) Ls[i] = lab[i * 3];
  Ls.sort();
  const q = (f) => Ls[Math.min(n - 1, Math.floor(f * n))];
  const p05 = q(0.05), p50 = q(0.5), p97 = q(0.97);
  return {
    lo: Math.min(p05, p50 - 0.1),
    mid: p50,
    hi: Math.max(p97 + 0.02, p50 + 0.15),
    /* A genuinely dark picture keeps a little of its mood. */
    M: G_MID - 0.05 * smooth((0.3 - p50) / 0.25),
  };
}

/** Source lightness → haze lightness: the median onto M, the shadows onto
    G_SHADOW, the highlights up toward G_LIGHT. */
function keyOf(L, ex) {
  if (L <= ex.mid) return G_SHADOW + (ex.M - G_SHADOW) * clamp01((L - ex.lo) / Math.max(1e-3, ex.mid - ex.lo));
  return ex.M + (G_LIGHT - ex.M) * clamp01((L - ex.mid) / Math.max(1e-3, ex.hi - ex.mid)) ** G_HI_GAMMA;
}

/**
 * One colour graded, into `o` as Oklab: what has no hue borrows the
 * record's; the arc that darkens only into brown is folded away; lightness
 * is keyed to the picture (ex); chroma is kept; pale warm light stays a
 * breath of warmth rather than cardboard; and the result is under the
 * peaks (capInto).
 */
function gradeInto(L, A, B, ex, tint, scale, ycap, o) {
  let C = Math.hypot(A, B);
  const source = C;
  const grey = C_GREY * Math.min(1, Math.max(0.3, L / 0.5));
  const tg = clamp01(1 - C / grey);
  if (tg > 0) {
    const tc = Math.hypot(tint[1], tint[2]);
    if (tc > 1e-4) {
      const target = Math.min(tc, 0.12);
      A += tg * ((tint[1] / tc) * target - A);
      B += tg * ((tint[2] / tc) * target - B);
      C = Math.hypot(A, B);
    }
  }
  /* Darkened, the arc from burnt orange to olive is only ever brown — a
     beige Canvas would become a khaki window. Fold it as the header wash
     does: onto burnt sienna or pine, lighter and softer at the warm tip. */
  let clay = 0;
  if (C > 1e-4) {
    const hue = warpHue((Math.atan2(B, A) * 180) / Math.PI, HAZE_FOLD);
    clay = clayOf(hue);
    const c = C * (1 - 0.22 * clay);
    A = c * Math.cos((hue * Math.PI) / 180);
    B = c * Math.sin((hue * Math.PI) / 180);
    C = c;
  }
  const s3 = Math.cbrt(scale);
  const Lo = (keyOf(L, ex) + 0.04 * clay) * s3;
  const vib = 1 + G_VIBRANCE * (1 - smooth(source / 0.12));
  const k = C > 1e-6 ? Math.min(C * G_CHROMA * vib * Math.sqrt(Lo / Math.max(L, 0.05)), G_CMAX) / C : 0;
  A *= k;
  B *= k;
  /* Warm light kept dark is only ever brown unless it is genuinely
     saturated: fire stays fire, but cream, sand and skin in soft light turn
     to a breath of warmth on graphite, not the colour of cardboard. */
  const warm = warmth((Math.atan2(B, A) * 180) / Math.PI);
  if (warm > 0) {
    const c = Math.hypot(A, B);
    const genuine = smooth((source - WARM_PALE) / (WARM_RICH - WARM_PALE));
    const kept = Math.min(c, WARM_BREATH + (c - WARM_BREATH) * genuine);
    const f = c > 1e-6 ? (c + warm * (kept - c)) / c : 1;
    A *= f;
    B *= f;
  }
  return capInto(Lo, A, B, s3, ycap, o);
}

/** Under the perceived peak (chroma gives way first, then lightness), in
    gamut, and under the luminance peak `ycap`; into `o` as Oklab. */
function capInto(Lo, A, B, s3, ycap, o) {
  const cap = P_PEAK * s3;
  const p = perceived(Lo, A, B);
  if (p > cap) {
    const c = Math.hypot(A, B);
    const kc = (p - Lo) / Math.max(c, 1e-6);
    const c2 = Math.max(c * 0.6, Math.min(c, (cap - Lo) / kc));
    A *= c2 / Math.max(c, 1e-6);
    B *= c2 / Math.max(c, 1e-6);
    Lo = Math.min(Lo, cap - kc * c2);
  }
  for (let i = 0; i < 16; i++) {
    const [r, g, b] = linearOfLab(Lo, A, B, scratchLin);
    if (r < -1e-4 || g < -1e-4 || b < -1e-4 || r > 1.0001 || g > 1.0001 || b > 1.0001) {
      A *= 0.88; // out of gamut at this lightness: give up chroma, keep hue
      B *= 0.88;
      continue;
    }
    const y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if (y > ycap) {
      Lo *= Math.cbrt(ycap / y) * 0.995;
      continue;
    }
    break;
  }
  o[0] = Lo;
  o[1] = A;
  o[2] = B;
  return o;
}

/** One colour as the haze grades it, alone (a flat picture of it), as Oklab. */
export function gradeLab(lab, tint, scale = 1) {
  const ex = exposureOf(Float32Array.from(lab), 1);
  return gradeInto(lab[0], lab[1], lab[2], ex, tint, scale, CEILING.peak * scale * DITHER_ROOM, [0, 0, 0]);
}

/* --- Reading the source ------------------------------------------------ */

/**
 * The picture inside any black frame: letterbox, pillarbox, or a Canvas
 * that is a small subject on black. Rows and columns are trimmed from each
 * edge of `rect` while nothing in them rises above near-black, up to 40% a
 * side, so a haze is made of the picture and not of its mount.
 * @param rect [x0, y0, x1, y1] to look inside, integers, end-exclusive
 * @returns [x0, y0, x1, y1] in pixels, end-exclusive
 */
export function cropBorders(data, w, h, rect = [0, 0, w, h]) {
  const BLACK = 20;
  const dark = (x, y) => {
    const i = (y * w + x) * 4;
    return data[i] < BLACK && data[i + 1] < BLACK && data[i + 2] < BLACK;
  };
  let [x0, y0, x1, y1] = rect;
  const rw = x1 - x0, rh = y1 - y0;
  const row = (y) => { for (let x = x0; x < x1; x++) if (!dark(x, y)) return false; return true; };
  const col = (x) => { for (let y = y0; y < y1; y++) if (!dark(x, y)) return false; return true; };
  while (y0 < rect[1] + rh * 0.4 && row(y0)) y0++;
  while (y1 > rect[3] - rh * 0.4 && row(y1 - 1)) y1--;
  while (x0 < rect[0] + rw * 0.4 && col(x0)) x0++;
  while (x1 > rect[2] - rw * 0.4 && col(x1 - 1)) x1--;
  return [x0, y0, x1, y1];
}

/**
 * The part of the source the panel frames, and the picture inside it.
 *
 * The picture's rect on screen (frameOf: the video as shown, or the open
 * panel for a cover's light) frames the source like `object-fit: cover`.
 * With the panel closed the whole source is the picture.
 * @returns { shown, rect } — both [x0, y0, x1, y1], integers
 */
export function framing(src, geo) {
  const { w, h } = src;
  const frame = frameOf(geo);
  const ratio = frame ? frame.w / frame.h : null;
  let shown = [0, 0, w, h];
  if (ratio) {
    if (w / h > ratio) {
      const cw = Math.max(1, Math.round(h * ratio));
      const x0 = Math.floor((w - cw) / 2);
      shown = [x0, 0, x0 + cw, h];
    } else {
      const ch = Math.max(1, Math.round(w / ratio));
      const y0 = Math.floor((h - ch) / 2);
      shown = [0, y0, w, y0 + ch];
    }
  }
  return { shown, rect: cropBorders(src.data, w, h, shown) };
}

/** A frame with nothing in it yet: a Canvas fading up from black. */
export function isBlank(src) {
  const d = src.data;
  for (let i = 0; i < d.length; i += 4) if (lum8(d, i) > 0.012) return false;
  return true;
}

/* Where the panel's type sits, as fractions of the panel's frame: the head
   and its tag across the top, the title, artists and meta across the lower
   part. */
const TOP_END = 0.18;
const BOTTOM_START = 0.58;

/**
 * How bright a picture is under the panel's type: the relative luminance
 * of the bright end (90th percentile) of the top and bottom bands of
 * `shown`, the rectangle the panel covers.
 */
export function lightOf(src, shown) {
  const [x0, y0, x1, y1] = shown;
  const rows = y1 - y0;
  const topEnd = y0 + Math.max(1, Math.round(rows * TOP_END));
  const lowStart = y0 + Math.round(rows * BOTTOM_START);
  const band = (from, to) => {
    const out = new Float32Array((to - from) * (x1 - x0));
    let n = 0;
    for (let y = from; y < to; y++) for (let x = x0; x < x1; x++) out[n++] = lum8(src.data, (y * src.w + x) * 4);
    out.sort();
    return out.length ? round3(out[Math.min(out.length - 1, Math.floor(out.length * 0.9))]) : 0;
  };
  return { top: band(y0, topEnd), bottom: band(lowStart, y1) };
}

const round3 = (v) => Math.round(v * 1000) / 1000;

/* --- The loop's representative frame ----------------------------------- */

const SIG = { x: 3, y: 5 };

/** A coarse picture of a frame's layout of colour: mean Oklab per cell. */
export function signatureOf(src, rect) {
  const [x0, y0, x1, y1] = rect;
  const out = new Float32Array(SIG.x * SIG.y * 3);
  const lab = [0, 0, 0];
  for (let gy = 0; gy < SIG.y; gy++) {
    for (let gx = 0; gx < SIG.x; gx++) {
      const xa = Math.floor(x0 + ((x1 - x0) * gx) / SIG.x), xb = Math.max(xa + 1, Math.floor(x0 + ((x1 - x0) * (gx + 1)) / SIG.x));
      const ya = Math.floor(y0 + ((y1 - y0) * gy) / SIG.y), yb = Math.max(ya + 1, Math.floor(y0 + ((y1 - y0) * (gy + 1)) / SIG.y));
      let r = 0, g = 0, b = 0, n = 0;
      for (let y = ya; y < yb; y++) {
        for (let x = xa; x < xb; x++) {
          const i = (y * src.w + x) * 4;
          r += LIN8[src.data[i]]; g += LIN8[src.data[i + 1]]; b += LIN8[src.data[i + 2]]; n++;
        }
      }
      labOfLinear(r / n, g / n, b / n, lab);
      out.set(lab, (gy * SIG.x + gx) * 3);
    }
  }
  return out;
}

/** Mean Oklab distance between two signatures' cells. */
export function sigDistance(a, b) {
  let sum = 0;
  for (let i = 0; i < a.length; i += 3) sum += Math.hypot(a[i] - b[i], a[i + 1] - b[i + 1], a[i + 2] - b[i + 2]);
  return sum / (a.length / 3);
}

/** The index of the signature most like all the others. */
export function medoid(sigs) {
  let best = 0, bestSum = Infinity;
  for (let i = 0; i < sigs.length; i++) {
    let sum = 0;
    for (let j = 0; j < sigs.length; j++) if (i !== j) sum += sigDistance(sigs[i], sigs[j]);
    if (sum < bestSum) { bestSum = sum; best = i; }
  }
  return best;
}

/** A frame this far from the loop's representative one shows a different
    picture, not the same picture a moment later. */
export const SETTLE_DISTANCE = 0.08;

/* --- Blur -------------------------------------------------------------- */

/* One pass of a box of radius r along rows or columns of a `ch`-channel
   image, edges clamped. Three passes each way are a close Gaussian,
   σ ≈ r + 0.5. */
function boxPass(src, dst, w, h, r, horizontal, ch) {
  const n = horizontal ? w : h;
  const lines = horizontal ? h : w;
  const stride = horizontal ? ch : w * ch;
  const lineStep = horizontal ? w * ch : ch;
  const inv = 1 / (2 * r + 1);
  for (let line = 0; line < lines; line++) {
    const base = line * lineStep;
    for (let c = 0; c < ch; c++) {
      let acc = 0;
      for (let k = -r; k <= r; k++) acc += src[base + Math.min(n - 1, Math.max(0, k)) * stride + c];
      for (let i = 0; i < n; i++) {
        dst[base + i * stride + c] = acc * inv;
        acc += src[base + Math.min(n - 1, i + r + 1) * stride + c] - src[base + Math.max(0, i - r) * stride + c];
      }
    }
  }
}

/** A Gaussian-like blur of σ ≈ `sigma` pixels of a `ch`-channel image, into a new buffer. */
function blur(img, w, h, sigma, ch = 3) {
  const r = Math.max(1, Math.round(sigma - 0.5));
  const a = Float32Array.from(img);
  const b = new Float32Array(img.length);
  for (let i = 0; i < 3; i++) {
    boxPass(a, b, w, h, r, true, ch);
    boxPass(b, a, w, h, r, false, ch);
  }
  return a;
}

/* --- Composition ------------------------------------------------------ */

/** The haze's own lens, σ in CSS pixels: soft enough to be light, sharp
    enough that the picture's shapes and edges read through the glass. The
    planes' frost (--frost-plane, app.css) softens it further under them. */
const LENS = 6;
/** Under a cover (not a Canvas) the haze round the tile is this much
    softer, so it reads as the light the tile sits in, not a second copy. */
const UNDER_COVER = 3;
/** Left of the picture: the magnification reached far from it, and over
    what share of the space to its left. */
const ZOOM = 2.4;
const ZOOM_REACH = 0.55;
/** How much darker (in L) the window's far edge is than the picture's. */
const FALLOFF = 0.2;
/** The seam. For SEAM CSS pixels left of the video's edge — the gap, the
    planes' rounded corners — and SEAM_IN inside it — the video's own
    rounded corners — the haze is the video's own light, at SEAM_DIM,
    fading into the graded haze. It is the video's BROAD light, softened
    by SEAM_SOFT (CSS px): the haze is one frame of a moving loop, and a
    sharp slice of it beside the video is a shape the video no longer has
    a moment later. */
const SEAM = 44;
const SEAM_IN = 22;
const SEAM_DIM = 0.9;
const SEAM_SOFT = 10;
/** The glass carries the seam's light in from its edge: the frost is made
    from the haze as it is for the first SPILL CSS px under the glass, and
    from the calibrated haze (no seam) from there on — so the light flows
    from the gap into the frost and is gone before any type can lie there
    (the pane's padding is at least 18px). GAP is the gutter (--gutter):
    where the glass begins, left of the video's edge. */
const GAP = 8;
const SPILL = 16;

const smooth = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t * t * (3 - 2 * t));
/** Mirror tiling: 0→1 over one copy, back 1→0 over the next (and the same
    below 0). */
const fold = (t) => {
  const m = Math.abs(t) % 2;
  return m <= 1 ? m : 2 - m;
};

/** Deterministic noise for the dither: the same haze twice is the same bytes. */
function noise(seed) {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13; s >>>= 0;
    s ^= s >>> 17;
    s ^= s << 5; s >>>= 0;
    return s / 4294967296;
  };
}

/** Bilinear read of a 3-channel image at (u, v) in 0..1, clamped, into out[o..o+2]. */
function sample3(img, iw, ih, u, v, out, o) {
  const sx = Math.min(iw - 1, Math.max(0, u * iw - 0.5));
  const sy = Math.min(ih - 1, Math.max(0, v * ih - 0.5));
  const x0 = Math.max(0, Math.min(iw - 2, Math.floor(sx))), y0 = Math.max(0, Math.min(ih - 2, Math.floor(sy)));
  const x1 = Math.min(iw - 1, x0 + 1), y1 = Math.min(ih - 1, y0 + 1);
  const tx = Math.min(1, Math.max(0, sx - x0)), ty = Math.min(1, Math.max(0, sy - y0));
  const i00 = (y0 * iw + x0) * 3, i10 = (y0 * iw + x1) * 3, i01 = (y1 * iw + x0) * 3, i11 = (y1 * iw + x1) * 3;
  for (let c = 0; c < 3; c++) {
    const top = img[i00 + c] + (img[i10 + c] - img[i00 + c]) * tx;
    const bot = img[i01 + c] + (img[i11 + c] - img[i01 + c]) * tx;
    out[o + c] = top + (bot - top) * ty;
  }
}

/** Bilinear read of a 1-channel image at (u, v) in 0..1, clamped. */
function sample1(img, iw, ih, u, v) {
  const sx = Math.min(iw - 1, Math.max(0, u * iw - 0.5));
  const sy = Math.min(ih - 1, Math.max(0, v * ih - 0.5));
  const x0 = Math.max(0, Math.min(iw - 2, Math.floor(sx))), y0 = Math.max(0, Math.min(ih - 2, Math.floor(sy)));
  const x1 = Math.min(iw - 1, x0 + 1), y1 = Math.min(ih - 1, y0 + 1);
  const tx = Math.min(1, Math.max(0, sx - x0)), ty = Math.min(1, Math.max(0, sy - y0));
  const top = img[y0 * iw + x0] + (img[y0 * iw + x1] - img[y0 * iw + x0]) * tx;
  const bot = img[y1 * iw + x0] + (img[y1 * iw + x1] - img[y1 * iw + x0]) * tx;
  return top + (bot - top) * ty;
}

const lumOf = (r, g, b) => 0.2126 * r + 0.7152 * g + 0.0722 * b;

/** The columns from x0 on of a `ch`-channel image, as their own image. */
function cropCols(img, w, h, x0, ch, x1 = w) {
  const cw = x1 - x0, out = new Float32Array(cw * h * ch);
  for (let y = 0; y < h; y++) out.set(img.subarray((y * w + x0) * ch, (y * w + x1) * ch), y * cw * ch);
  return out;
}

/**
 * Render the haze.
 *
 * With the panel open, the picture lies under the video exactly as the
 * video shows it (geo.video), or under the panel as a cover's light (no
 * video); to its left it carries on as its own mirror image — continuous
 * at the edge — magnified with distance up to ZOOM and falling off a
 * little in lightness. With the panel closed the whole picture covers the
 * window, and a restrained glow rises from the player bar, bottom left.
 *
 * @param src   { data, w, h } — RGBA bytes of the whole source
 * @param geo   { w, h, px, left, video, immersive } — the haze's size in
 *              pixels (the window's aspect) and CSS pixels per haze pixel;
 *              the panel's left edge as a fraction of the window (null:
 *              closed); the video's rect on screen, in haze pixels (null:
 *              no video); and whether a Canvas covers the panel
 * @param tint  encoded [r, g, b] of the record's colour, for what has none
 * @param frost the glass planes' frost (parseFrost), or null for none
 * @returns { rgba, frost, frostW, frostH, gain, veil, grey, light } — the
 *          haze as 8-bit RGBA; the same haze frosted (or null), at half its
 *          size (frostW x frostH); the gain the mean
 *          ceilings took; "r g b" of the haze just left of the picture; the
 *          share of pixels that took the record's tint; and how bright the
 *          panel's picture is under its type (see lightOf)
 */
export function renderHaze(src, geo, tint, { scale = 1, seed = 1, frost = null } = {}) {
  const { w, h } = geo;
  const px = geo.px ?? 5;
  const frame = frameOf(geo);
  const video = frame && geo.video && geo.immersive ? frame : null;
  const { shown, rect } = framing(src, geo);
  const [rx0, ry0, rx1, ry1] = rect;
  const rw = rx1 - rx0, rh = ry1 - ry0, n = rw * rh;
  const S = src.data;
  const s3 = Math.cbrt(scale);
  const ycap = CEILING.peak * scale * DITHER_ROOM;
  const one = [0, 0, 0], rgb = [0, 0, 0];

  /* Read the picture: Oklab, and (for the seam) its own linear light. */
  const lab = new Float32Array(n * 3);
  const raw = video ? new Float32Array(n * 3) : null;
  let greys = 0;
  for (let y = 0; y < rh; y++) {
    for (let x = 0; x < rw; x++) {
      const i = ((ry0 + y) * src.w + rx0 + x) * 4, o = (y * rw + x) * 3;
      const r = LIN8[S[i]], g = LIN8[S[i + 1]], b = LIN8[S[i + 2]];
      labOfLinear(r, g, b, one);
      if (Math.hypot(one[1], one[2]) < C_GREY * 0.5) greys++;
      lab[o] = one[0]; lab[o + 1] = one[1]; lab[o + 2] = one[2];
      if (raw) { raw[o] = r; raw[o + 1] = g; raw[o + 2] = b; }
    }
  }

  /* Grade it, pixel by pixel, BEFORE the blur: the fold is a hue
     discontinuity by design, and the blur smooths it. Then lay the
     source's fine detail back over the graded broad light. */
  const ex = exposureOf(lab, n);
  const tintLab = toOklab(...tint);
  const glab = new Float32Array(n * 3);
  const pair = new Float32Array(n * 3);
  for (let p = 0; p < n; p++) {
    const o = p * 3;
    gradeInto(lab[o], lab[o + 1], lab[o + 2], ex, tintLab, scale, ycap, one);
    glab[o] = one[0]; glab[o + 1] = one[1]; glab[o + 2] = one[2];
    pair[o] = lab[o];
    pair[o + 1] = one[0];
  }
  const base = blur(pair, rw, rh, LOCAL_SIGMA * Math.max(rw, rh));
  const graded = new Float32Array(n * 3), gradedC = new Float32Array(n);
  for (let p = 0; p < n; p++) {
    const o = p * 3;
    const Ln = Math.max(0.03, base[o + 1] + LOCAL_GAIN * (lab[o] - base[o]));
    const k = Math.min(1.4, Math.max(0.6, Math.sqrt(Ln / Math.max(glab[o], 0.03))));
    capInto(Ln, glab[o + 1] * k, glab[o + 2] * k, s3, ycap, one);
    gradedC[p] = Math.hypot(one[1], one[2]);
    linearOfLab(one[0], one[1], one[2], rgb);
    graded[o] = clamp01(rgb[0]);
    graded[o + 1] = clamp01(rgb[1]);
    graded[o + 2] = clamp01(rgb[2]);
  }

  /* Lay it out. Per column: where in the picture it reads (u), how much it
     is magnified (s), how much it falls off (f), and whether it is on the
     picture itself. The magnification is integrated, so it grows smoothly
     from none at the edge. */
  const colU = new Float32Array(w), colS = new Float32Array(w).fill(1), colF = new Float32Array(w).fill(1), colFc = new Float32Array(w).fill(1);
  const onPicture = new Uint8Array(w);
  if (frame) {
    const reach = ZOOM_REACH * Math.max(1, frame.x);
    let u = 0, prev = 0;
    for (let x = w - 1; x >= 0; x--) {
      const d = frame.x - (x + 0.5);
      if (d <= 0) {
        colU[x] = -d / frame.w;
        onPicture[x] = 1;
        continue;
      }
      u += (d - prev) / (frame.w * (1 + (ZOOM - 1) * smooth((d + prev) / 2 / reach)));
      prev = d;
      colU[x] = u;
      colS[x] = 1 + (ZOOM - 1) * smooth(d / reach);
      colF[x] = 1 - FALLOFF * (d / Math.max(1, frame.x)) ** 1.2;
      colFc[x] = colF[x] ** 0.6;
    }
  }
  const laid = new Float32Array(w * h * 3), laidC = new Float32Array(w * h);
  const F = new Float32Array(w * h), Fc = new Float32Array(w * h);
  const k = Math.max(w / rw, h / rh), ox = (w - rw * k) / 2, oy = (h - rh * k) / 2;
  const cx = w * 0.06, radius = w * 1.05;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const p = y * w + x;
      let u, v;
      if (frame) {
        u = fold(colU[x]);
        v = fold(0.5 + ((y + 0.5 - frame.y) / frame.h - 0.5) / colS[x]);
        F[p] = colF[x];
        Fc[p] = colFc[x];
      } else {
        u = (x + 0.5 - ox) / (rw * k);
        v = (y + 0.5 - oy) / (rh * k);
        const r = Math.hypot(x + 0.5 - cx, y + 0.5 - h) / radius;
        F[p] = 1 - (r < 0.5 ? r : 0.5 + (r - 0.5) * 0.72);
        Fc[p] = F[p] ** 0.6;
      }
      sample3(graded, rw, rh, u, v, laid, p * 3);
      laidC[p] = sample1(gradedC, rw, rh, u, v);
    }
  }

  /* The lens, in linear light, with the chroma it averages away given back:
     orange and blue glass blurred together keep the dominant hue at the
     parts' mean chroma rather than turning grey-maroon. Then the fall-off,
     as colour (lightness by f, chroma by f^0.6). The result is kept as
     Oklab, in place. */
  const sigma = LENS / px;
  const lensed = blur(laid, w, h, sigma), lensedC = blur(laidC, w, h, sigma, 1);
  const cover = frame && !geo.immersive;
  /* The cover's softer lens is only ever read on the picture, so only the
     columns from its edge (and the blur's reach) are blurred. */
  const sx0 = cover ? Math.max(0, Math.floor(frame.x - 3 * sigma * UNDER_COVER)) : 0;
  const cw = w - sx0;
  const softer = cover ? blur(cropCols(laid, w, h, sx0, 3), cw, h, sigma * UNDER_COVER) : null;
  const softerC = cover ? blur(cropCols(laidC, w, h, sx0, 1), cw, h, sigma * UNDER_COVER, 1) : null;
  const measureTo = frame && geo.immersive ? Math.max(1, Math.floor(frame.x)) : w;
  let sumC = 0, nC = 0;
  for (let p = 0; p < w * h; p++) {
    const x = p % w, o = p * 3;
    const soft = softer && onPicture[x];
    const q = soft ? (p / w | 0) * cw + x - sx0 : p;
    const img = soft ? softer : lensed;
    labOfLinear(img[q * 3], img[q * 3 + 1], img[q * 3 + 2], one);
    const c = Math.sqrt(one[1] * one[1] + one[2] * one[2]);
    const kr = c > 1e-5 ? Math.min(2.2, Math.max(1, ((soft ? softerC : lensedC)[q] * 0.85) / c)) : 1;
    const f = F[p], fc = kr * Fc[p];
    lensed[o] = one[0] * f;
    lensed[o + 1] = one[1] * fc;
    lensed[o + 2] = one[2] * fc;
    if (x < measureTo) {
      sumC += c * fc;
      nC++;
    }
  }

  /* The ceilings: every pixel under the peak; the mean chroma under its
     budget; and the mean of what is seen under both mean ceilings,
     luminance and perceived lightness — a gain that only ever dims. */
  const meanC = sumC / Math.max(1, nC);
  const kc = meanC > C_MEAN * s3 ? ((C_MEAN * s3) / meanC) ** 0.9 : 1;
  const out = laid;
  let sumY = 0, sumP = 0, nY = 0;
  for (let p = 0; p < w * h; p++) {
    const o = p * 3;
    const L = lensed[o], A = lensed[o + 1] * kc, B = lensed[o + 2] * kc;
    linearOfLab(L, A, B, rgb);
    let r = clamp01(rgb[0]), g = clamp01(rgb[1]), b = clamp01(rgb[2]);
    let y = lumOf(r, g, b), t = 1;
    if (y > ycap) {
      const q = ycap / y;
      r *= q; g *= q; b *= q; y = ycap;
      t = Math.cbrt(q);
    }
    out[o] = r; out[o + 1] = g; out[o + 2] = b;
    if (p % w < measureTo) {
      sumY += y;
      sumP += perceived(L, A, B) * t;
      nY++;
    }
  }
  const meanY = sumY / Math.max(1, nY), meanP = sumP / Math.max(1, nY);
  const gain = Math.min(1, (CEILING.mean * scale) / Math.max(1e-6, meanY), ((P_MEAN * s3) / Math.max(1e-6, meanP)) ** 3);

  /* The seam: the video's own light, carried out of its edge. Only the
     source's columns the mirror reads are softened. */
  let seam = null;
  if (video) {
    const x0 = Math.max(0, Math.floor(frame.x - SEAM / px));
    const x1 = Math.min(w, Math.ceil(frame.x + (1.5 * SEAM_IN) / px));
    const sw = Math.max(0, x1 - x0);
    const soft = SEAM_SOFT / ((frame.w * px) / rw);
    const ew = Math.min(rw, Math.ceil(((SEAM + 1.5 * SEAM_IN) / px / frame.w) * rw + 3 * soft) + 2);
    const edge = blur(cropCols(raw, rw, rh, 0, 3, ew), ew, rh, soft);
    const light = new Float32Array(sw * h * 3), alpha = new Float32Array(sw * h);
    for (let y = 0; y < h; y++) {
      const v = fold((y + 0.5 - frame.y) / frame.h);
      for (let x = x0; x < x1; x++) {
        const d = (frame.x - (x + 0.5)) * px;
        const a = d > 0 ? 1 - smooth(d / SEAM) : 1 - smooth((-d - 0.5 * SEAM_IN) / SEAM_IN);
        if (a <= 0) continue;
        const q = y * sw + (x - x0);
        sample3(edge, ew, rh, ((Math.abs(d) / px / frame.w) * rw) / ew, v, light, q * 3);
        light[q * 3] *= SEAM_DIM; light[q * 3 + 1] *= SEAM_DIM; light[q * 3 + 2] *= SEAM_DIM;
        alpha[q] = a;
      }
    }
    seam = { x0, w: sw, light, alpha };
  }
  return emit(out, gain, { src, shown, geo, frame, seed, frost, grey: greys / n, seam });
}

/** The picture's rect in haze pixels: the video as shown, the open panel
    (a cover's light), or null (closed: the whole window). */
function frameOf(geo) {
  if (geo.left == null) return null;
  if (geo.video) return geo.video;
  const x = geo.left * geo.w;
  return { x, y: 0, w: geo.w - x, h: geo.h };
}

/** Quantise a finished haze (linear, under the peak) with its gain and its
    seam, and everything measured on it: the frost (of the haze without the
    seam, except at the glass's edge: see SPILL), the veil, the type's light. */
function emit(out, gain, { src, shown, geo, frame, seed, frost, grey, seam }) {
  const { w, h } = geo;
  const px = geo.px ?? 5;
  /* Quantise, dithered: each channel rounds up with the probability of its
     fraction, so a gradient's average is exact and its steps dissolve. */
  const rand = noise(seed);
  const rgba = new Uint8ClampedArray(w * h * 4);
  const encoded = frost ? new Float32Array(w * h * 3) : null;
  /* The columns whose frost the seam reaches, from SPILL under the glass
     (and the frost's blur beyond it) to the seam's end: the haze as it is
     there, seam included. Even-aligned, for the frost's half resolution. */
  const reach = frost ? Math.ceil(3 * frost.sigma) + 2 : 0;
  const lx0 = frost && seam ? Math.max(0, Math.floor(frame.x - (GAP + SPILL) / px - reach) & ~1) : 0;
  const lx1 = frost && seam ? Math.min(w, seam.x0 + seam.w + reach) : 0;
  const lw = lx1 - lx0;
  const lit = lw > 0 ? new Float32Array(lw * h * 3) : null;
  const vx1 = frame ? Math.min(w, Math.max(1, Math.floor(frame.x))) : w;
  const vx0 = Math.max(0, vx1 - 2);
  const veil = [0, 0, 0];
  let nv = 0;
  for (let p = 0; p < w * h; p++) {
    const o = p * 3, q = p * 4, x = p % w;
    let r = out[o] * gain, g = out[o + 1] * gain, b = out[o + 2] * gain;
    const er = encFast(r), eg = encFast(g), eb = encFast(b);
    if (encoded) {
      encoded[o] = er; encoded[o + 1] = eg; encoded[o + 2] = eb;
    }
    if (x >= vx0 && x < vx1) {
      veil[0] += er; veil[1] += eg; veil[2] += eb;
      nv++;
    }
    const l = lit && x >= lx0 && x < lx1 ? ((p / w | 0) * lw + x - lx0) * 3 : -1;
    const s = seam && x >= seam.x0 && x < seam.x0 + seam.w ? (p / w | 0) * seam.w + (x - seam.x0) : -1;
    const a = s >= 0 ? seam.alpha[s] : 0;
    if (a > 0) {
      r += (seam.light[s * 3] - r) * a;
      g += (seam.light[s * 3 + 1] - g) * a;
      b += (seam.light[s * 3 + 2] - b) * a;
      const sr = encFast(r), sg = encFast(g), sb = encFast(b);
      if (l >= 0) {
        lit[l] = sr; lit[l + 1] = sg; lit[l + 2] = sb;
      }
      rgba[q] = Math.floor(sr * 255 + rand());
      rgba[q + 1] = Math.floor(sg * 255 + rand());
      rgba[q + 2] = Math.floor(sb * 255 + rand());
    } else {
      if (l >= 0) {
        lit[l] = er; lit[l + 1] = eg; lit[l + 2] = eb;
      }
      rgba[q] = Math.floor(er * 255 + rand());
      rgba[q + 1] = Math.floor(eg * 255 + rand());
      rgba[q + 2] = Math.floor(eb * 255 + rand());
    }
    rgba[q + 3] = 255;
  }
  /* The type's light: over a Canvas it is the picture; over a cover (or
     nothing) the type in the panel sits on the haze itself. */
  const light = frame && !geo.immersive
    ? lightOf({ data: rgba, w, h }, [Math.min(w - 1, Math.ceil(frame.x)), 0, w, h])
    : lightOf(src, shown);
  /* The frost is at half the haze's resolution: its blur is many times
     coarser than two haze pixels, so nothing is lost, and it is a quarter
     of the work and of the texture. */
  const fw = Math.ceil(w / 2), fh = Math.ceil(h / 2);
  const half = frost ? { ...frost, sigma: frost.sigma / 2 } : null;
  const frosted = frost ? frostOf(halve(encoded, w, h), fw, fh, half, rand) : null;
  if (lit) {
    /* The glass's edge: the frost of the haze as it is, handing over to
       the calibrated frost across SPILL. */
    const hw = Math.ceil(lw / 2), fx0 = lx0 / 2;
    const edge = frostOf(halve(lit, lw, h), hw, fh, half, rand);
    for (let x = 0; x < hw && fx0 + x < fw; x++) {
      const d = (frame.x - 2 * (fx0 + x + 0.5)) * px - GAP;
      const k = 1 - smooth(d / SPILL);
      if (k <= 0) continue;
      for (let y = 0; y < fh; y++) {
        const q = (y * fw + fx0 + x) * 4, e = (y * hw + x) * 4;
        for (let c = 0; c < 3; c++) frosted[q + c] = Math.round(frosted[q + c] + (edge[e + c] - frosted[q + c]) * k);
      }
    }
  }
  return {
    rgba,
    frost: frosted,
    frostW: fw,
    frostH: fh,
    gain,
    veil: veil.map((v) => Math.round((v / Math.max(1, nv)) * 255)).join(" "),
    grey,
    light,
  };
}

/* --- The frost --------------------------------------------------------- */

/** A 3-channel image at half its size, each pixel the mean of its 2x2 block
    (the last row and column clamped). */
function halve(img, w, h) {
  const hw = Math.ceil(w / 2), hh = Math.ceil(h / 2), out = new Float32Array(hw * hh * 3);
  for (let y = 0; y < hh; y++) {
    const y0 = 2 * y, y1 = Math.min(h - 1, y0 + 1);
    for (let x = 0; x < hw; x++) {
      const x0 = 2 * x, x1 = Math.min(w - 1, x0 + 1), o = (y * hw + x) * 3;
      for (let c = 0; c < 3; c++) {
        out[o + c] = (img[(y0 * w + x0) * 3 + c] + img[(y0 * w + x1) * 3 + c] + img[(y1 * w + x0) * 3 + c] + img[(y1 * w + x1) * 3 + c]) / 4;
      }
    }
  }
  return out;
}

/**
 * Read the glass's frost token — `blur(36px) saturate(1.45) brightness(0.7)`
 * — as the numbers frostOf needs. Any function missing is the identity; a
 * token with no blur is no frost at all.
 * @param value      the token's computed value
 * @param pxPerHaze  CSS pixels per haze pixel
 */
export function parseFrost(value, pxPerHaze) {
  const num = (name) => {
    const m = String(value ?? "").match(new RegExp(name + String.raw`\(\s*([\d.]+)(px|%)?\s*\)`));
    if (!m) return null;
    return m[2] === "%" ? Number(m[1]) / 100 : Number(m[1]);
  };
  const blurPx = num("blur");
  if (!blurPx) return null;
  return { sigma: blurPx / pxPerHaze, saturate: num("saturate") ?? 1, brightness: num("brightness") ?? 1 };
}

/**
 * The haze as the glass planes show it: what `backdrop-filter: blur()
 * saturate() brightness()` would make of it, computed once per haze
 * instead of by the compositor on every frame anything moves.
 *
 * The same arithmetic as CSS, so a plane over this looks as it did over
 * the live filter and the glass's calibration holds: the blur, the
 * saturate matrix and the brightness all act on ENCODED values, each
 * clamped to [0, 1] as the filter chain clamps between its steps. Then
 * dithered like the haze, which the live filter could not be.
 *
 * @param encoded  Float32Array(w * h * 3) of the haze, encoded, 0..1
 * @param frost    { sigma (haze px), saturate, brightness } — see parseFrost
 */
export function frostOf(encoded, w, h, frost, rand = noise(7)) {
  const b = blur(encoded, w, h, frost.sigma);
  const s = frost.saturate, k = frost.brightness;
  const m = [
    0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s,
    0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s,
    0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s,
  ];
  const out = new Uint8ClampedArray(w * h * 4);
  for (let p = 0; p < w * h; p++) {
    const o = p * 3, q = p * 4;
    const r = b[o], g = b[o + 1], bl = b[o + 2];
    for (let c = 0; c < 3; c++) {
      const v = clamp01(m[c * 3] * r + m[c * 3 + 1] * g + m[c * 3 + 2] * bl) * k;
      out[q + c] = Math.floor(clamp01(v) * 255 + rand());
    }
    out[q + 3] = 255;
  }
  return out;
}

/** A source of one colour: what stands in for a picture that cannot be read. */
export function solidSource(rgb) {
  const data = new Uint8ClampedArray(4 * 4 * 4);
  for (let i = 0; i < data.length; i += 4) {
    data[i] = Math.round(rgb[0] * 255);
    data[i + 1] = Math.round(rgb[1] * 255);
    data[i + 2] = Math.round(rgb[2] * 255);
    data[i + 3] = 255;
  }
  return { data, w: 4, h: 4 };
}
