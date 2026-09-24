/* =====================================================================
   THE HAZE — arithmetic

   One blurred picture of what is playing sits behind every panel, and the
   panels are glass over it (Ambient.svelte, haze.worker.js). This module is
   the part of that which is arithmetic: where each pixel of the haze takes
   its light from, how bright it may be, and how it is quantised. Pure
   functions, so the worker and the tests share them.

   The haze is the picture itself, not a summary of it. The source is read
   at a real resolution, laid out so that the panel's picture carries on
   leftward out of the panel (mirrored at the panel's edge, so the colour
   there is continuous), blurred like an out-of-focus lens — lightly at the
   panel's edge, heavily far from it — and only then toned, pixel by pixel,
   in Oklab: lightness is compressed under a cap, hue is kept, and chroma is
   kept (boosted a little, since blur spends it). Rich dark colour gives the
   haze presence without luminance, which is what keeps type on the glass
   legible.

   Blur happens in LINEAR light, as a lens does it: a convex mix of linear
   colours is never brighter than its brightest input, so the ceilings hold
   through it. Quantisation to 8 bits is dithered, which is what keeps a
   dark gradient stretched over a window from breaking into bands.
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
/** The long side of the source as the worker reads it, in pixels. */
export const PROBE = 160;

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

/* Lightness is compressed toward a cap with the darks lifted off a floor, so
   shadow still reads as dark colour rather than as a hole. The curve is only
   gently compressive: the haze's detail is lightness as much as hue, and a
   curve that pressed every value to the cap would flatten it back into a
   wash. As a pixel is lifted it keeps its saturation (chroma grows with
   lightness), and chroma is boosted a little besides, because blur spends
   it. L_MAX only lets a blue, which carries little luminance, sit lighter
   than a yellow before the luminance ceiling trims it. */
const L_MAX = 0.54;
const L_FLOOR = 0.09;
const KAPPA = 2.4;
const C_BOOST = 1.3;
const C_MAX = 0.2;
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

/**
 * One colour, made safe: toned, under the ceiling `ycap`, inside sRGB.
 * Writes Oklab into `o` and returns it.
 */
function toneInto(L, A, B, tint, scale, ycap, o) {
  let C = Math.hypot(A, B);
  const source = C;
  /* A white or grey source becomes a dim, TINTED haze, never a grey one:
     what has no hue borrows the record's, at the record's own chroma. */
  const grey = C_GREY * Math.min(1, Math.max(0.3, L / 0.5));
  const t = clamp01(1 - C / grey);
  if (t > 0) {
    const tc = Math.hypot(tint[1], tint[2]);
    if (tc > 1e-4) {
      const target = Math.min(tc, 0.12);
      A += t * ((tint[1] / tc) * target - A);
      B += t * ((tint[2] / tc) * target - B);
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
  const lmax = L_MAX * s3;
  const floor = L_FLOOR * s3;
  let Lo = floor + 0.06 * clay + ((lmax - floor) * (1 - Math.exp(-KAPPA * L))) / (1 - Math.exp(-KAPPA));
  const lifted = Math.max(1, Lo / Math.max(L, 0.06)) ** 0.85;
  /* Proportional, with no floor: per pixel, a floor would turn the faint
     cast of noise and grey debris into specks of saturated colour. */
  const k = C > 1e-6 ? Math.min(C * C_BOOST * lifted, C_MAX) / C : 0;
  A *= k;
  B *= k;
  /* Warm light kept dark is only ever brown unless it is genuinely
     saturated: fire stays fire, but cream, sand, skin in soft light and a
     warm record colour borrowed by grey all turn sienna — the whole window
     the colour of cardboard. So orange and its folded neighbours carry
     chroma only in proportion to how saturated the SOURCE is there, down
     to a breath of warmth on graphite. Reds, magentas, blues, teals and
     greens are untouched. */
  const warm = warmth((Math.atan2(B, A) * 180) / Math.PI);
  if (warm > 0) {
    const c = Math.hypot(A, B);
    const genuine = smooth((source - WARM_PALE) / (WARM_RICH - WARM_PALE));
    const kept = Math.min(c, WARM_BREATH + (c - WARM_BREATH) * genuine);
    const f = c > 1e-6 ? (c + warm * (kept - c)) / c : 1;
    A *= f;
    B *= f;
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

/**
 * One colour toned for the haze, as Oklab [L, a, b].
 * @param lab   Oklab of the source colour
 * @param tint  Oklab of the record's colour, for what has none
 */
export function toneLab(lab, tint, scale = 1) {
  return toneInto(lab[0], lab[1], lab[2], tint, scale, CEILING.peak * scale * DITHER_ROOM, [0, 0, 0]);
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

/** The open panel's aspect (width / height) in a haze of this geometry. */
function panelAspect(geo) {
  return geo.left != null ? ((1 - geo.left) * geo.w) / geo.h : null;
}

/**
 * The part of the source the panel frames, and the picture inside it.
 *
 * The open panel is a full-height column, and it frames its picture like
 * `object-fit: cover`: a Canvas fills it exactly so, and a cover is laid
 * under it the same way, as the light the smaller tile in the panel sits
 * in. With the panel closed the whole source is the picture.
 * @returns { shown, rect } — both [x0, y0, x1, y1], integers
 */
export function framing(src, geo) {
  const { w, h } = src;
  const ratio = panelAspect(geo);
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

/* One pass of a box of radius r along rows or columns of a 3-channel image,
   edges clamped. Three passes each way are a close Gaussian, σ ≈ r + 0.5. */
function boxPass(src, dst, w, h, r, horizontal) {
  const n = horizontal ? w : h;
  const lines = horizontal ? h : w;
  const stride = horizontal ? 3 : w * 3;
  const lineStep = horizontal ? w * 3 : 3;
  const inv = 1 / (2 * r + 1);
  for (let line = 0; line < lines; line++) {
    const base = line * lineStep;
    for (let c = 0; c < 3; c++) {
      let acc = 0;
      for (let k = -r; k <= r; k++) acc += src[base + Math.min(n - 1, Math.max(0, k)) * stride + c];
      for (let i = 0; i < n; i++) {
        dst[base + i * stride + c] = acc * inv;
        acc += src[base + Math.min(n - 1, i + r + 1) * stride + c] - src[base + Math.max(0, i - r) * stride + c];
      }
    }
  }
}

/** A Gaussian-like blur of σ ≈ `sigma` pixels, into a new buffer. */
function blur(img, w, h, sigma) {
  const r = Math.max(1, Math.round(sigma - 0.5));
  const a = Float32Array.from(img);
  const b = new Float32Array(img.length);
  for (let i = 0; i < 3; i++) {
    boxPass(a, b, w, h, r, true);
    boxPass(b, a, w, h, r, false);
  }
  return a;
}

/* --- Composition ------------------------------------------------------ */

/* The lens, as fractions of the haze's width: σ at the panel's edge, and
   far from it. The edge is sharp enough that the picture visibly carries
   on out of the panel; the far side is soft enough to be light, not an
   image. The glass over most of the window softens it further; what is
   seen raw is the gutters between the planes and the panel's edge. */
const BLUR_NEAR = 0.009;
const BLUR_FAR = 0.026;
/** Under a panel showing a cover (not a Canvas) the haze is the light the
    smaller tile sits in: partway to the far lens, so it reads as the
    cover's colour and not as a second, blurred copy of the cover. */
const UNDER_COVER = 0.6;
/** How far (as a fraction of the space left of the panel) the lens takes
    to go from near to far. Short: only the panel's dissolving edge needs
    the picture recognisable, and a mirrored picture further out would be
    recognisable as a mirror — a face would become a Rorschach blot. */
const BLUR_RAMP = 0.22;
/** How much darker the far edge of the window is than the panel's edge. */
const FALLOFF = 0.32;
/** Chroma given back after the blur, which mixes neighbouring hues. */
const C_REGAIN = 1.25;
/** At most this much horizontal stretch for the picture's continuation. */
const STRETCH = 1.5;

const smooth = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t * t * (3 - 2 * t));
/** Mirror tiling: 0→1 over one copy, back 1→0 over the next. */
const fold = (t) => {
  const m = t % 2;
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

/**
 * Render the haze.
 *
 * With the panel open, the panel's picture is laid out under the panel
 * exactly as the panel frames it, and carries on leftward from the panel's
 * edge as its own mirror image — so the colour at the edge is continuous —
 * a little stretched and mirrored again if it runs out, falling off in
 * lightness (and a little in chroma) toward the far edge, so it darkens as
 * colour rather than greying out. With the panel
 * closed the record is in the player bar, bottom left, and a restrained
 * glow rises from there.
 *
 * @param src   { data, w, h } — RGBA bytes of the whole source
 * @param geo   { w, h, left, immersive } — the haze's size in pixels (the
 *              window's aspect), the panel's left edge as a fraction of
 *              the window (null: closed), and whether a Canvas covers the
 *              panel (the haze under it is then only seen at its edge)
 * @param tint  encoded [r, g, b] of the record's colour, for what has none
 * @param frost the glass planes' frost (parseFrost), or null for none
 * @returns { rgba, frost, gain, veil, grey, light } — the haze as 8-bit
 *          RGBA; the same haze frosted (or null); the gain the mean
 *          ceiling took; "r g b" of the haze just left of the
 *          panel; the share of pixels that took the record's tint; and how
 *          bright the panel's picture is under its type (see lightOf)
 */
export function renderHaze(src, geo, tint, { scale = 1, seed = 1, frost = null } = {}) {
  const { w, h } = geo;
  const open = geo.left != null;
  const { shown, rect } = framing(src, geo);
  const [rx0, ry0, rx1, ry1] = rect;
  const rw = rx1 - rx0, rh = ry1 - ry0;
  const S = src.data;

  /* Tone the picture first, pixel by pixel, into linear light. The fold is
     a hue discontinuity by design: done before the blur, the blur smooths
     it, where done after it would cut every gradient that passes through
     the arc (green into red, say) into a hard edge. */
  const tintLab = toOklab(...tint);
  const ycap = CEILING.peak * scale * DITHER_ROOM;
  const toned = new Float32Array(rw * rh * 3);
  const lab = [0, 0, 0];
  const rgb = [0, 0, 0];
  let greys = 0;
  for (let y = 0; y < rh; y++) {
    for (let x = 0; x < rw; x++) {
      const i = ((ry0 + y) * src.w + rx0 + x) * 4;
      labOfLinear(LIN8[S[i]], LIN8[S[i + 1]], LIN8[S[i + 2]], lab);
      if (Math.hypot(lab[1], lab[2]) < C_GREY * 0.5) greys++;
      toneInto(lab[0], lab[1], lab[2], tintLab, scale, ycap, lab);
      linearOfLab(lab[0], lab[1], lab[2], rgb);
      const o = (y * rw + x) * 3;
      toned[o] = clamp01(rgb[0]);
      toned[o + 1] = clamp01(rgb[1]);
      toned[o + 2] = clamp01(rgb[2]);
    }
  }

  /* Where each pixel takes its light from, how far it falls off, and how
     soft its lens is (0 near, 1 far). */
  const laid = new Float32Array(w * h * 3);
  const fall = new Float32Array(w * h);
  const soft = new Float32Array(w * h);
  const xs = open ? geo.left * w : 0;
  const panelW = w - xs;
  const stretch = open ? Math.min(STRETCH, Math.max(1, xs / panelW)) : 1;
  const t0 = open && !geo.immersive ? UNDER_COVER : 0;
  const cx = w * 0.06;
  const radius = w * 1.05;
  for (let y = 0; y < h; y++) {
    const py = y + 0.5;
    const v = py / h;
    for (let x = 0; x < w; x++) {
      const px = x + 0.5;
      const p = y * w + x;
      let u, f, t;
      if (open) {
        const dx = px - xs;
        if (dx >= 0) {
          u = fold(dx / panelW);
          f = 1;
          t = t0;
        } else {
          const d = -dx;
          u = fold(d / (panelW * stretch));
          f = 1 - FALLOFF * (d / Math.max(1, xs)) ** 1.2;
          t = t0 + (1 - t0) * smooth(d / Math.max(1, xs * BLUR_RAMP));
        }
      } else {
        u = px / w;
        const r = Math.hypot(px - cx, py - h) / radius;
        f = 1 - (r < 0.5 ? r : 0.5 + (r - 0.5) * 0.72);
        t = 1;
      }
      fall[p] = f;
      soft[p] = t;
      /* Bilinear over the toned picture, clamped to it. */
      const sx = Math.min(rw - 1, Math.max(0, u * rw - 0.5));
      const sy = Math.min(rh - 1, Math.max(0, v * rh - 0.5));
      const x0i = Math.max(0, Math.min(rw - 2, Math.floor(sx))), y0i = Math.max(0, Math.min(rh - 2, Math.floor(sy)));
      const x1i = Math.min(rw - 1, x0i + 1), y1i = Math.min(rh - 1, y0i + 1);
      const tx = Math.min(1, Math.max(0, sx - x0i)), ty = Math.min(1, Math.max(0, sy - y0i));
      const i00 = (y0i * rw + x0i) * 3, i10 = (y0i * rw + x1i) * 3;
      const i01 = (y1i * rw + x0i) * 3, i11 = (y1i * rw + x1i) * 3;
      for (let c = 0; c < 3; c++) {
        const top = toned[i00 + c] + (toned[i10 + c] - toned[i00 + c]) * tx;
        const bot = toned[i01 + c] + (toned[i11 + c] - toned[i01 + c]) * tx;
        laid[p * 3 + c] = top + (bot - top) * ty;
      }
    }
  }

  const near = blur(laid, w, h, BLUR_NEAR * w);
  const far = blur(laid, w, h, BLUR_FAR * w);

  /* Fall off, give back some of the chroma the blur spent, and keep every
     pixel under the peak — all continuous, so nothing here can make an
     edge the blur did not. */
  const out = new Float32Array(w * h * 3);
  const measureTo = open && geo.immersive ? Math.max(1, Math.floor(xs)) : w;
  let sumY = 0, nY = 0;
  for (let p = 0; p < w * h; p++) {
    const t = soft[p];
    const o = p * 3;
    labOfLinear(
      near[o] + (far[o] - near[o]) * t,
      near[o + 1] + (far[o + 1] - near[o + 1]) * t,
      near[o + 2] + (far[o + 2] - near[o + 2]) * t,
      lab,
    );
    const f = fall[p];
    const fc = f ** 0.6 * C_REGAIN;
    linearOfLab(lab[0] * f, lab[1] * fc, lab[2] * fc, rgb);
    let r = clamp01(rgb[0]), g = clamp01(rgb[1]), b = clamp01(rgb[2]);
    /* Clipping a channel that went out of gamut can add a little light. */
    const y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if (y > ycap) {
      const k = ycap / y;
      r *= k; g *= k; b *= k;
    }
    out[o] = r; out[o + 1] = g; out[o + 2] = b;
    if (p % w < measureTo) {
      sumY += 0.2126 * r + 0.7152 * g + 0.0722 * b;
      nY++;
    }
  }

  /* The mean ceiling: only ever dims, and only a haze bright all over. In
     linear light a gain scales luminance exactly, so there is no search. */
  const gain = Math.min(1, (CEILING.mean * scale) / Math.max(1e-6, sumY / Math.max(1, nY)));

  /* Quantise, dithered: each channel rounds up with the probability of its
     fraction, so a gradient's average is exact and its steps dissolve. */
  const rand = noise(seed);
  const rgba = new Uint8ClampedArray(w * h * 4);
  const encoded = frost ? new Float32Array(w * h * 3) : null;
  for (let p = 0; p < w * h; p++) {
    const o = p * 3, q = p * 4;
    for (let c = 0; c < 3; c++) {
      const e = enc(out[o + c] * gain);
      if (encoded) encoded[o + c] = e;
      rgba[q + c] = Math.floor(e * 255 + rand());
    }
    rgba[q + 3] = 255;
  }

  /* The veil: the haze in the two columns just left of the panel. */
  const vx1 = open ? Math.max(1, Math.floor(xs)) : w;
  const vx0 = Math.max(0, vx1 - 2);
  const sum = [0, 0, 0];
  for (let y = 0; y < h; y++) for (let x = vx0; x < vx1; x++) for (let c = 0; c < 3; c++) sum[c] += rgba[(y * w + x) * 4 + c];
  const veil = sum.map((v) => Math.round(v / (h * (vx1 - vx0)))).join(" ");

  /* The type's light: over a Canvas it is the picture; over a cover (or
     nothing) the type in the panel sits on the haze itself. */
  const light = open && !geo.immersive
    ? lightOf({ data: rgba, w, h }, [Math.min(w - 1, Math.ceil(xs)), 0, w, h])
    : lightOf(src, shown);
  return {
    rgba,
    frost: frost ? frostOf(encoded, w, h, frost, rand) : null,
    gain,
    veil,
    grey: greys / (rw * rh),
    light,
  };
}

/* --- The frost --------------------------------------------------------- */

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
