/* =====================================================================
   THE HAZE — the renderer, off the main thread

   Renders a haze picture when asked, and otherwise does nothing. It reads
   the source once (a GPU downscale, then one small readback), keeps those
   pixels so a change of layout or colour can be rendered again without a
   new read, and hands back each finished picture as an ImageBitmap, which
   the page shows with a compositor crossfade (Ambient.svelte). No timers,
   no loop: every message in is one piece of work, and there are only a
   handful per track.

   Messages in:
     render { seq, key, fade, source?, video?, allowBlank?, geo, tint, scale, frost }
            render the haze for `key`, and its frosted twin when `frost`
            (parseFrost) is given. `source` (a VideoFrame or an
            ImageBitmap, transferred) replaces the kept pixels; without it
            the kept pixels are laid out again. null means unreadable.
     sample { key, frame, geo }
            one more frame of a playing Canvas's first loop, kept for
            `settle` and measured for the type's light.
     settle { seq, key, fade, geo, tint, scale, frost }
            the loop has been seen: if its representative frame is a
            different picture from the one the haze was made of, render it.
   Messages out (seq, key and fade are echoed):
     haze   { seq, key, fade, bitmap, frost, veil, grey, light, distance?, ms }
                                                     a picture to show
     blank  { seq }                                  that frame was black
     light  { key, light }                           the type's light
     done   { seq, key, light, distance }            settle had nothing to do
   ===================================================================== */

import {
  renderHaze,
  solidSource,
  framing,
  isBlank,
  lightOf,
  signatureOf,
  sigDistance,
  medoid,
  PROBE,
  SETTLE_DISTANCE,
} from "./haze.js";

/* Downscale on the GPU, read through a CPU canvas: drawing a video frame
   straight into a CPU canvas would pull all 720x1280 pixels across. */
const probe = new OffscreenCanvas(PROBE, PROBE);
const probeCtx = probe.getContext("2d");
probeCtx.imageSmoothingQuality = "high";
const read = new OffscreenCanvas(PROBE, PROBE).getContext("2d", { willReadFrequently: true });
const stage = new OffscreenCanvas(1, 1);
const stageCtx = stage.getContext("2d");

/** The kept source: { key, src } — src null when it could not be read. */
let kept = null;
/** A playing Canvas's first loop: { key, frames: [src], light }. */
let loop = null;
let seed = 1;

/** The whole source at PROBE on its long side, as RGBA bytes. */
function readSource(source) {
  const sw = source.displayWidth || source.width;
  const sh = source.displayHeight || source.height;
  if (!sw || !sh) return undefined;
  const k = PROBE / Math.max(sw, sh);
  const w = Math.max(8, Math.round(sw * k));
  const h = Math.max(8, Math.round(sh * k));
  probeCtx.clearRect(0, 0, PROBE, PROBE);
  probeCtx.drawImage(source, 0, 0, w, h);
  try {
    read.clearRect(0, 0, PROBE, PROBE);
    read.drawImage(probe, 0, 0, w, h, 0, 0, w, h);
    return { data: read.getImageData(0, 0, w, h).data, w, h };
  } catch {
    return null; // tainted: a cover served without CORS
  }
}

const max = (a, b) => ({ top: Math.max(a.top, b.top), bottom: Math.max(a.bottom, b.bottom) });
/** An unreadable picture: the type must assume the worst. */
const UNKNOWN = { top: 1, bottom: 1 };

/** Render and hand over a picture. `seen` is the light already measured on
    other frames of the same loop, which the type must also clear. */
function paint(m, src, seen = null, distance = undefined) {
  const t0 = performance.now();
  const out = renderHaze(src ?? solidSource(m.tint), m.geo, m.tint, {
    scale: m.scale,
    seed: seed++,
    frost: m.frost ?? null,
  });
  if (stage.width !== m.geo.w || stage.height !== m.geo.h) {
    stage.width = m.geo.w;
    stage.height = m.geo.h;
  }
  const bitmapOf = (rgba) => {
    stageCtx.putImageData(new ImageData(rgba, m.geo.w, m.geo.h), 0, 0);
    return stage.transferToImageBitmap();
  };
  const bitmap = bitmapOf(out.rgba);
  const frost = out.frost ? bitmapOf(out.frost) : null;
  const light = !src ? UNKNOWN : seen ? max(seen, out.light) : out.light;
  postMessage(
    {
      type: "haze",
      seq: m.seq,
      key: m.key,
      fade: m.fade,
      bitmap,
      frost,
      veil: out.veil,
      grey: out.grey,
      light,
      distance,
      ms: performance.now() - t0,
    },
    frost ? [bitmap, frost] : [bitmap],
  );
}

onmessage = ({ data: m }) => {
  switch (m.type) {
    case "render": {
      if (m.source !== undefined) {
        const src = m.source ? readSource(m.source) : null;
        m.source?.close();
        if (src === undefined) break;
        if (src && m.video && !m.allowBlank && isBlank(src)) {
          postMessage({ type: "blank", seq: m.seq });
          break;
        }
        if (kept?.key !== m.key) loop = null;
        kept = { key: m.key, src };
      }
      if (kept?.key !== m.key) break;
      paint(m, kept.src, loop?.key === m.key ? loop.light : null);
      break;
    }
    case "sample": {
      const src = kept?.key === m.key && kept.src ? readSource(m.frame) : null;
      m.frame.close();
      if (!src) break;
      const light = lightOf(src, framing(src, m.geo).shown);
      if (loop?.key !== m.key) {
        loop = { key: m.key, frames: [kept.src], light: lightOf(kept.src, framing(kept.src, m.geo).shown) };
      }
      const before = loop.light;
      loop.frames.push(src);
      loop.light = max(before, light);
      /* Brighter is answered at once: the type's shadow should not wait
         for the loop to finish to learn about a flash. */
      if (loop.light.top > before.top + 0.02 || loop.light.bottom > before.bottom + 0.02) {
        postMessage({ type: "light", key: m.key, light: loop.light });
      }
      break;
    }
    case "settle": {
      if (!loop || loop.key !== m.key || kept?.key !== m.key) {
        postMessage({ type: "done", seq: m.seq, key: m.key, light: null });
        break;
      }
      const { frames, light } = loop;
      loop = null;
      const sigs = frames.map((src) => signatureOf(src, framing(src, m.geo).rect));
      const pick = medoid(sigs);
      const distance = pick === 0 ? 0 : sigDistance(sigs[pick], sigs[0]);
      /* The haze was made of frames[0]. Only a clearly different picture is
         worth a (very slow) change; the same picture a moment later is not. */
      if (distance > SETTLE_DISTANCE) {
        kept = { key: m.key, src: frames[pick] };
        paint(m, kept.src, light, distance);
      } else {
        postMessage({ type: "done", seq: m.seq, key: m.key, light, distance });
      }
      break;
    }
  }
};
