/* =====================================================================
   CONTENT COLOUR

   The palette owns the chrome; the artwork owns the content. Everything in
   this module exists to answer one question — "what colour is this record?" —
   and to answer it in a form the interface can actually use.

   Two rules make that safe:

     1. Only the HUE comes from the picture. Lightness and chroma are imposed
        here, at fixed targets, so a white sleeve cannot produce a white header
        and a neon one cannot produce a neon header. The app stays as dark as
        it was designed to be, and every page is a different colour anyway.

     2. Nothing here is allowed to fail loudly. A cross-origin refusal, a
        decode error, a cover that never arrives — each one falls through to a
        deterministic hue derived from the entity id, which is also what the
        generated identity tile uses, so a coverless playlist still has a
        colour and it is the same colour as its tile.

   Why this and not a palette tint: a translucent pastel over near-black is
   always a dark low-chroma smear — that is arithmetic, not taste, and it is
   exactly how the old 20%-rose header wash turned brown. A colour picked at a
   fixed dark lightness and a real chroma never does that.
   ===================================================================== */

import { resolveCoverUrl } from "./state.svelte.js";

/* --- Oklab. Small enough to inline; the alternative is a dependency for
       twelve lines of matrix arithmetic. -------------------------------- */

const toLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const toSrgb = (c) => {
  const v = c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
  return Math.round(Math.min(1, Math.max(0, v)) * 255);
};

/** sRGB 0-255 → Oklab. */
function oklab(r8, g8, b8) {
  const r = toLinear(r8 / 255);
  const g = toLinear(g8 / 255);
  const b = toLinear(b8 / 255);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** Oklch → `#rrggbb`, gamut-clipped by channel (good enough at these chromas). */
function hex(L, C, hueDeg) {
  const h = (hueDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const r = toSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s);
  const g = toSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s);
  const bl = toSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s);
  return `#${((1 << 24) | (r << 16) | (g << 8) | bl).toString(16).slice(1)}`;
}

/* --- The clamp ------------------------------------------------------- */

/**
 * One hue, four jobs. These lightnesses are the whole safety story: the header
 * sits at L 0.35 whatever the sleeve does, so the pane never brightens and the
 * white type on top never has to be re-checked.
 */
function palette(hue, chroma) {
  const c = Math.min(0.115, Math.max(0, chroma));
  /**
   * The one hue-dependent correction, and it is the brown fix.
   *
   * Every other hue at L 0.35 is a recognisable dark version of itself: 200°
   * is teal, 300° is aubergine, 14° is oxblood. Amber is the exception —
   * there is no dark amber, there is only brown, because "amber" IS a light
   * yellow. A gold sleeve rendered at the same lightness as everything else
   * comes out #5e2b00, which is the exact colour this design has been told
   * twice to stay away from. Lifting the band around 68° by five points of
   * lightness buys back enough to read as tobacco rather than as mud, and
   * white type still clears 5:1 against it.
   */
  const amber = Math.max(0, 1 - Math.abs(hue - 68) / 32);
  const lift = 0.055 * amber;
  return {
    hue,
    chroma: c,
    /** Header wash, top stop. Dark enough for 56px white type over it. */
    wash: hex(0.35 + lift, c, hue),
    /** Deeper stop, so the fade has a shape instead of a straight ramp. */
    washDeep: hex(0.26 + lift * 0.7, c * 0.82, hue),
    /** Coloured light: card hover shadows, artwork glow, panel tint. */
    glow: hex(0.6, Math.min(0.14, c * 1.25), hue),
    /** Generated identity tile, light corner → dark corner. */
    tileA: hex(0.68, Math.min(0.14, c * 1.3), hue),
    tileB: hex(0.32, Math.min(0.1, c * 0.9), hue),
  };
}

/**
 * The identity hue ring: what a record looks like when it has no artwork.
 *
 * Deliberately NOT the semantic accents. The old version drew generated tiles
 * from foam and rose only, which meant half of everything coverless was the
 * same pale warm and — once that same pick drove the header wash — half of all
 * playlists opened brown.
 *
 * These eight are spread around the wheel and skip 45°–135° entirely, because
 * every one of them has to survive being rendered at L 0.35 for the header
 * wash and that whole arc turns to mud down there. 70° gives #533400, which is
 * brown; 118° gives #374000, which is army olive; 130° is still #294400. Green
 * only becomes green again at about 140 (#1b4610), and warm only stays a
 * colour below about 40 (#632316, a burnt red). Those are the two edges, and
 * the ring lives outside them.
 */
const IDENTITY_HUES = [14, 38, 142, 172, 198, 228, 268, 316];

/** FNV-1a, unchanged from the one the artwork tiles have always used. */
function fnv(seed) {
  let h = 0x811c9dc5;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h;
}

/**
 * A tone built from a hue the app already owns.
 *
 * Liked Songs is the one page whose colour is a decision rather than a
 * measurement — it is the rose page by definition — so it asks for rose's hue
 * (21° in Oklab) and gets it clamped to exactly the same dark as every
 * extracted header. That is why it can be deep rose without being brown:
 * `#ebbcba` faded to 20% is a warm grey, but rose's *hue* rebuilt at a real
 * chroma is a red.
 */
export const paletteFor = palette;

/** Synchronous, network-free, stable across restarts. */
export function identityTone(seed, chroma = 0.095) {
  return palette(IDENTITY_HUES[fnv(String(seed ?? "")) % IDENTITY_HUES.length], chroma);
}

/* --- Extraction ------------------------------------------------------ */

/**
 * 16x16 is not a compromise, it is the point: the browser's own downscaler
 * does the averaging in native code, and 256 samples is far more than enough
 * to find which hue a sleeve is mostly made of. A full-size read would cost
 * ~90,000 times the pixels for the same answer.
 */
const SAMPLE = 16;
const BUCKETS = 24; // 15° each

/** Pixels that cannot tell us anything about hue. */
const MIN_ALPHA = 200;
const MIN_L = 0.16; // black borders, drop shadows, letterboxing
const MAX_L = 0.93; // paper-white sleeves, blown highlights
const MIN_C = 0.035; // greys, and the near-greys that would average to mud

let canvas = null;
function scratch() {
  if (!canvas) {
    canvas = document.createElement("canvas");
    canvas.width = SAMPLE;
    canvas.height = SAMPLE;
  }
  return canvas;
}

/**
 * Reads the dominant hue out of an image.
 *
 * The image is loaded on its own `Image`, never on the one the interface is
 * showing: the displayed `<img>` must not carry `crossorigin`, because if the
 * header were ever missing that attribute would cost the ARTWORK rather than
 * just the colour. Here a refusal costs nothing but a `null`.
 */
function readHue(url) {
  return new Promise((resolve) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.decoding = "async";
    img.onerror = () => resolve(null);
    img.onload = () => {
      try {
        const c = scratch();
        const ctx = c.getContext("2d", { willReadFrequently: true });
        if (!ctx) return resolve(null);
        ctx.clearRect(0, 0, SAMPLE, SAMPLE);
        ctx.drawImage(img, 0, 0, SAMPLE, SAMPLE);
        const { data } = ctx.getImageData(0, 0, SAMPLE, SAMPLE);

        /* Sum a and b per hue bucket, weighted. Summing the OPPONENT axes and
           not the angles is what keeps 359° and 1° from averaging to cyan. */
        const sumA = new Float64Array(BUCKETS);
        const sumB = new Float64Array(BUCKETS);
        const weight = new Float64Array(BUCKETS);
        let best = -1;
        let bestWeight = 0;

        for (let i = 0; i < data.length; i += 4) {
          if (data[i + 3] < MIN_ALPHA) continue;
          const [L, a, b] = oklab(data[i], data[i + 1], data[i + 2]);
          if (L < MIN_L || L > MAX_L) continue;
          const C = Math.hypot(a, b);
          if (C < MIN_C) continue;
          /* Weight by chroma ABOVE the threshold, not by chroma: a sleeve of
             warm-white paper is thousands of pixels each a hair over the line,
             and weighting them by their absolute chroma let them out-vote the
             handful of genuinely coloured ones. Measured from the threshold, a
             near-grey contributes near-nothing, which is the truth about it.
             Mid lightnesses are preferred for the same reason — a record's
             identity is its body colour, not its shadow or its specular. */
          const w = (C - MIN_C) * (1 - Math.min(1, Math.abs(L - 0.55) / 0.55) * 0.7);
          let angle = (Math.atan2(b, a) * 180) / Math.PI;
          if (angle < 0) angle += 360;
          const k = Math.floor(angle / (360 / BUCKETS)) % BUCKETS;
          sumA[k] += a * w;
          sumB[k] += b * w;
          weight[k] += w;
        }

        /* A bucket plus its two neighbours, so a hue that straddles a boundary
           is not beaten by a narrower one that happens to sit in the middle. */
        for (let k = 0; k < BUCKETS; k++) {
          const total =
            weight[k] +
            weight[(k + 1) % BUCKETS] * 0.5 +
            weight[(k + BUCKETS - 1) % BUCKETS] * 0.5;
          if (total > bestWeight) {
            bestWeight = total;
            best = k;
          }
        }
        /* Read the image fine, found no colour in it: a black-and-white
           sleeve, a black square, a scan of typed paper. Distinct from
           `null`, which means we never got to look — see the caller. */
        if (best < 0 || bestWeight <= 0) return resolve("mono");

        const a = sumA[best];
        const b = sumB[best];
        let angle = (Math.atan2(b, a) * 180) / Math.PI;
        if (angle < 0) angle += 360;
        /* How much of the image agreed, 0..1, and it is what stops a greyscale
           sleeve from getting a confidently coloured page. One wide field of
           colour earns full chroma; a muted or monochrome cover earns almost
           none and its header comes out a near-neutral dark — which is the
           honest answer, because it does not HAVE a colour. */
        const agreement = Math.min(1, bestWeight / (SAMPLE * SAMPLE * 0.03));
        resolve({ hue: angle, chroma: 0.028 + 0.087 * agreement });
      } catch {
        /* Tainted canvas: the cover protocol did not send CORS headers. The
           artwork is fine, we simply do not get to know its colour. */
        resolve(null);
      }
    };
    img.src = url;
  });
}

/* --- Store ----------------------------------------------------------- */

/** key → palette. Reactive so a late extraction repaints the header. */
const tones = $state({});
const inflight = new Set();

/**
 * The tone for a piece of content, available synchronously.
 *
 * Returns the identity fallback immediately and upgrades to the extracted
 * colour when it lands, which means a page never waits on a canvas read to
 * paint and never flashes a neutral header first.
 */
export function coverTone(coverUrl, seed = "") {
  const key = coverUrl || `seed:${seed}`;
  const hit = tones[key];
  if (hit) return hit;

  if (coverUrl && !inflight.has(key)) {
    inflight.add(key);
    resolveCoverUrl(coverUrl)
      .then((local) => (local ? readHue(local) : null))
      .then((found) => {
        /* Cache the miss as well, as the fallback: one failed read per cover.
           A cover that HAS no colour — a black-and-white sleeve, a black
           square — gets the identity hue at a chroma so low it is effectively
           a neutral dark. That is deliberate and it is the honest answer: a
           monochrome record should not open a blue page. It also keeps the
           scale continuous, because a nearly-monochrome sleeve lands on almost
           exactly the same tone through the measured path. */
        tones[key] = found ? palette(found.hue, found.chroma) : identityTone(seed || key, 0.022);
      })
      .catch(() => {
        tones[key] = identityTone(seed || key);
      })
      .finally(() => inflight.delete(key));
  }
  return identityTone(seed || key);
}
