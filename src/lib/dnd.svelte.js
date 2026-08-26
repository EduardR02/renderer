/**
 * Track drag-and-drop: the one gesture controller behind "grab a song and
 * move it".
 *
 *   - onto a sidebar playlist row          -> COPY there (add only; the
 *     source keeps the track — dropping on any other list adds too)
 *
 * Everything spatial runs in ONE requestAnimationFrame loop that exists only
 * while a drag is live: hit-testing, the landing slot, autoscroll of the two
 * scrollable regions (the content pane and the library rail), and the ghost's
 * position. `pointermove` only records coordinates; it never reads layout.
 * When no drag runs, this module's steady-state cost is exactly zero.
 *
 * Reactivity contract: `trackDrag` is the reactive projection components read
 * (TrackList draws the insertion bar and parts its rows around the slot;
 * Sidebar lights up its drop targets). The DOM ghost is deliberately outside
 * Svelte — moving one transform per frame must not re-render anything.
 */

import { api, library, promotePlaylist } from "./state.svelte.js";

export const trackDrag = $state({
  active: false,
  /** The grabbed row's track object and uri. */
  track: null,
  uri: "",
  /**
   * The playlist the row was dragged FROM, when it is an owned playlist.
   * Dropping back on it is a cancel, never a self-copy.
   */
  sourcePlaylistId: null,
  sourceIndex: -1,
  /** Latest pointer position, window coordinates. */
  x: 0,
  y: 0,
  /** Wrapper element of the hovered reorder zone, or null. */
  listEl: null,
  /**
   * FINAL landing index (0..length-1); -1 when no reorder target is under
   * the pointer. The insertion bar, the parting preview and the reorder
   * call all read this one number, so they cannot disagree.
   */
  slot: -1,
});

/* ------------------------------------------------------------------ */
/* Drop-zone registry                                                  */
/* ------------------------------------------------------------------ */

const zones = new Set();

/**
 * Registers a TrackList body as a reorder target; returns the unregister
 * function. `meta()` is read lazily at drag time so the registry never holds
 * stale lengths or handlers.
 */
export function registerReorderZone(el, meta) {
  const zone = { el, meta };
  zones.add(zone);
  return () => zones.delete(zone);
}

/**
 * True for ~350ms after a real drag ended. Row double-clicks check this so
 * the release of a long drag cannot land as an accidental "play".
 */
let gestureEndedAt = -1e9;
export function justDragged() {
  return performance.now() - gestureEndedAt < 350;
}

/* ------------------------------------------------------------------ */
/* Gesture internals — deliberately non-reactive                       */
/* ------------------------------------------------------------------ */

const PRESS_THRESHOLD = 6; // px of travel before a press becomes a drag
const EDGE_BAND = 64; // px autoscroll band inside a scroller
const MAX_SCROLL_SPEED = 1100; // px/s at full band penetration
const ROW_H = 48; // must track --row-h
/* Compact-pill ghost geometry. The pointer carries the settled pill by a
   fixed handle — 18px in from its left edge, vertically centred — and the
   whole thing stays under a 260px cap however long the title is. */
const PILL_MAX_WIDTH = 260;
const PILL_ANCHOR_X = 18;
const PILL_ANCHOR_Y = ROW_H / 2;
/* The lift scale, applied with transform-origin 0 0 (see positionGhost and
   the #drag-layer CSS). The pill's sliding margins are expressed in
   pre-scale coordinates, so both files need the number. */
const GHOST_SCALE = 1.028;

let press = null; // pending press before the threshold, then live drag state
let ghostWrap = null; // positioned wrap holding the pill ghost
let pillEl = null; // the pill inside ghostWrap; its margins carry the anchor
let originRect = null; // where the ghost came from (fly-back target)
let layer = null; // #drag-layer, created once per activation
let rafId = 0;
let lastFrame = 0;
let scrollers = []; // [{ el }] resolved once per drag
let cleanupTimer = 0;

/**
 * A row was pressed. Nothing visible happens until the pointer travels past
 * the threshold; press-and-release stays the no-op click it always was.
 *
 * ctx: { event, rowEl, index, track, playlistId }
 *   `playlistId` present => move semantics (owned playlist source).
 */
export function pressTrack(ctx) {
  if (press) return;
  const e = ctx.event;
  if (e.button !== 0 || e.isPrimary === false) return;
  const t = ctx.track;
  if (!t?.uri || t.unavailable) return;

  press = {
    active: false,
    ctx,
    rowEl: ctx.rowEl,
    startX: e.clientX,
    startY: e.clientY,
    grabX: 0,
    grabY: 0,
    dropRow: null,
  };
  window.addEventListener("pointermove", onPressMove);
  window.addEventListener("pointerup", onPressUp);
  window.addEventListener("pointercancel", onPressCancel);
  window.addEventListener("keydown", onPressKey, true);
}

function releaseListeners() {
  window.removeEventListener("pointermove", onPressMove);
  window.removeEventListener("pointerup", onPressUp);
  window.removeEventListener("pointercancel", onPressCancel);
  window.removeEventListener("keydown", onPressKey, true);
}

function onPressMove(e) {
  if (!press) return;
  trackDrag.x = e.clientX;
  trackDrag.y = e.clientY;
  if (press.active) return;
  const dx = e.clientX - press.startX;
  const dy = e.clientY - press.startY;
  if (dx * dx + dy * dy < PRESS_THRESHOLD * PRESS_THRESHOLD) return;
  activate();
}

function onPressKey(e) {
  if (!press || e.key !== "Escape") return;
  e.preventDefault();
  e.stopPropagation();
  endDrag(false);
}

function onPressUp() {
  if (!press) return;
  const commit = press.active;
  endDrag(commit);
}

function onPressCancel() {
  if (!press) return;
  endDrag(false);
}

function activate() {
  const ctx = press.ctx;
  press.active = true;

  layer = ensureLayer();
  ghostWrap = buildGhost(press.rowEl); // also fixes press.grabX/grabY

  const d = trackDrag;
  d.active = true;
  d.track = ctx.track;
  d.uri = ctx.track.uri;
  d.sourcePlaylistId = ctx.playlistId ?? null;
  d.sourceIndex = ctx.index;
  d.listEl = null;
  d.slot = -1;

  scrollers = [];
  const paneScroller = press.rowEl.closest(".scroll");
  if (paneScroller) scrollers.push({ el: paneScroller });
  const rail = document.querySelector(".lib-list");
  if (rail) scrollers.push({ el: rail });

  document.documentElement.classList.add("dragging-track");
  positionGhost();
  lastFrame = performance.now();
  rafId = requestAnimationFrame(frame);
}

function ensureLayer() {
  let el = document.getElementById("drag-layer");
  if (!el) {
    el = document.createElement("div");
    el.id = "drag-layer";
    document.body.appendChild(el);
  }
  return el;
}

/**
 * Builds the ghost: the pressed row folded into a compact pill carrying its
 * art tile and title block — cloned nodes, so Cover's fallback tiles keep
 * working offline — styled entirely by the #drag-layer block in app.css.
 *
 * The gesture opens at the row's FULL geometry: the wrap starts as wide as
 * the row, under the pointer exactly where it closed, then compresses over
 * var(--d2)/var(--ease). Two CSS transitions share that one clock — the
 * wrap's width eases down to the pill's natural width while the pill slides
 * inside the wrap so the grab point travels to the pill's handle (18px in
 * from its left edge, vertically centred). One continuous morph: frame()'s
 * per-frame transform math is untouched by all of this, and reduced motion
 * lands directly in pill form because the media query kills the transitions.
 * Measuring the natural width costs one layout read per activation, never
 * per frame.
 */
function buildGhost(rowEl) {
  const rect = rowEl.getBoundingClientRect();
  originRect = rect;
  press.grabX = trackDrag.x - rect.left;
  press.grabY = trackDrag.y - rect.top;

  const wrap = document.createElement("div");
  wrap.className = "tl-drag-ghost";
  const pill = document.createElement("div");
  pill.className = "tl-drag-pill";
  const art = rowEl.querySelector(".c-art");
  if (art) pill.appendChild(art.cloneNode(true));
  const title = rowEl.querySelector(".c-title");
  if (title) pill.appendChild(title.cloneNode(true));
  wrap.appendChild(pill);
  layer.appendChild(wrap);
  pillEl = pill;

  /* Natural pill width clamped to the design cap, measured off an
     unconstrained render that is corrected before any frame can paint it. */
  wrap.style.width = "max-content";
  const pillWidth = Math.min(pill.getBoundingClientRect().width, PILL_MAX_WIDTH);

  /* Start geometry first — the row's own footprint, pill flush — committed
     with a style flush so the transitions have something to ease FROM; then
     the end state: fold to the pill, slide it under the pointer's handle. */
  wrap.style.width = `${rect.width}px`;
  pill.style.marginLeft = "0px";
  pill.style.marginTop = "0px";
  void wrap.offsetWidth;
  wrap.style.width = `${pillWidth}px`;
  pill.style.marginLeft = `${(press.grabX - PILL_ANCHOR_X) / GHOST_SCALE}px`;
  pill.style.marginTop = `${(press.grabY - PILL_ANCHOR_Y) / GHOST_SCALE}px`;

  return wrap;
}

function positionGhost() {
  if (!ghostWrap) return;
  ghostWrap.style.transform =
    `translate3d(${trackDrag.x - press.grabX}px, ${trackDrag.y - press.grabY}px, 0) scale(${GHOST_SCALE})`;
}

/** Ease-in weight for the autoscroll band: gentle at the edge, brisk deeper. */
function bandPenetration(distance) {
  const t = 1 - distance / EDGE_BAND;
  return t * t;
}

/**
 * One hit-test pass at a window point: a reorder zone containing the point
 * wins outright; otherwise a sidebar playlist row is the candidate. Over a
 * list that takes no drops (sorted view, read-only list) neither applies and
 * the drop cancels. Shared by the rAF loop and by endDrag — the pointer can
 * come up before the loop's first pass, and the drop must still resolve.
 */
function hitTest(x, y) {
  let zoneEl = null;
  let slot = -1;
  for (const z of zones) {
    if (!z.el.isConnected) continue;
    const r = z.el.getBoundingClientRect();
    if (x < r.left || x > r.right || y < r.top || y >= r.bottom) continue;
    const meta = z.meta();
    if (meta.canReorder && meta.length > 0) {
      const from = trackDrag.sourceIndex;
      const last = meta.length - 1;
      const raw = Math.round((y - r.top) / ROW_H);
      if (raw <= from) {
        /* Upward and at-origin: every original boundary is a real slot. */
        slot = Math.max(0, Math.min(last, raw));
      } else if (raw === from + 1) {
        /* The WHOLE immediate next row means "land right after me". The
           boundary between the dragged row and this one decodes to a
           no-op, so it must never be offered — offering it is what made
           one-slot-down drags feel impossible. */
        slot = Math.min(last, from + 1);
      } else {
        /* Deeper pointers are judged by midpoint WITHOUT the dragged row
           present — which is exactly the final landing index. */
        slot = Math.max(0, Math.min(last, raw - 1));
      }
      zoneEl = z.el;
    }
    break; // at most one zone contains the point; stop at the first overlap
  }
  const dropRow =
    zoneEl ? null : (document.elementFromPoint(x, y)?.closest?.(".lib-row[data-pid]") ?? null);
  return { zoneEl, slot, dropRow };
}

/**
 * One layout pass per frame while dragging: hover state, landing slot,
 * autoscroll, ghost position. Every write to reactive state is guarded by an
 * equality check, so idling the pointer notifies nobody.
 */
function frame(now) {
  if (!press?.active) return;
  const dt = Math.min(48, now - lastFrame) / 1000;
  lastFrame = now;
  const d = trackDrag;

  for (const { el } of scrollers) {
    if (el.scrollHeight <= el.clientHeight) continue;
    const r = el.getBoundingClientRect();
    if (d.x < r.left || d.x > r.right) continue;
    const fromTop = d.y - r.top;
    const fromBottom = r.bottom - d.y;
    if (fromTop >= 0 && fromTop < EDGE_BAND) {
      el.scrollTop -= MAX_SCROLL_SPEED * bandPenetration(fromTop) * dt;
    } else if (fromBottom >= 0 && fromBottom < EDGE_BAND) {
      el.scrollTop += MAX_SCROLL_SPEED * bandPenetration(fromBottom) * dt;
    }
  }

  const { zoneEl, slot, dropRow } = hitTest(d.x, d.y);

  if (d.listEl !== zoneEl) d.listEl = zoneEl;
  if (d.slot !== slot) d.slot = slot;
  press.dropRow = dropRow;

  positionGhost();
  rafId = requestAnimationFrame(frame);
}

function endDrag(commit) {
  const p = press;
  press = null;
  releaseListeners();
  cancelAnimationFrame(rafId);

  const d = trackDrag;
  const wasActive = d.active;
  let outcome = "cancel";

  if (commit && wasActive) {
    /* The pointer can come up before the first frame after activation has
       run — a flick-release over the rail, or a machine under load. No frame
       loop pass has resolved a target then; do it synchronously from the
       final pointer position so the drop doesn't degrade into a cancel. */
    if (d.listEl === null && d.slot < 0 && p.dropRow === null) {
      const hit = hitTest(d.x, d.y);
      d.listEl = hit.zoneEl;
      d.slot = hit.slot;
      p.dropRow = hit.dropRow;
    }
    let matched = null;
    if (d.listEl) {
      for (const z of zones) {
        if (z.el === d.listEl) { matched = z; break; }
      }
    }
    if (matched) {
      const meta = matched.meta();
      const from = d.sourceIndex;
      /* `slot` is already the FINAL index — bar, parting preview and this
         call all derive from it; no coordinate conversion left. */
      const to = d.slot;
      if (to !== from && to >= 0) {
        meta.run(from, to);
        outcome = "reorder";
      }
    } else if (p.dropRow) {
      const pid = p.dropRow.dataset.pid;
      if (pid !== d.sourcePlaylistId) {
        commitToPlaylist(pid, d.uri);
        pulse(p.dropRow);
        outcome = "playlist";
      }
    }
  }

  d.active = false;
  d.track = null;
  d.uri = "";
  d.sourcePlaylistId = null;
  d.sourceIndex = -1;
  d.listEl = null;
  d.slot = -1;

  if (wasActive) {
    document.documentElement.classList.remove("dragging-track");
    retireGhost(outcome === "cancel" ? "return" : "dissolve");
    gestureEndedAt = performance.now();
    suppressReleaseClick();
  }
}

let releaseGuardTimer = 0;

/**
 * A drag that ends over what it started on leaves the browser primed to
 * synthesize a click at the release point. Artist names are role="link"
 * SPANS rather than buttons precisely so they can ellipsize (see
 * ArtistLinks.svelte), which means that synthetic click passes TrackList's
 * interactive-cell press guard and navigates. After a REAL drag, swallow
 * the first click that follows within ~400ms — capture phase,
 * preventDefault + stopPropagation, gone once it has fired. The timeout
 * disarms the trap when no click comes, so a drag released into empty
 * space can never eat the user's NEXT intentional click.
 */
function suppressReleaseClick() {
  clearTimeout(releaseGuardTimer);
  const swallow = (e) => {
    disarm();
    e.preventDefault();
    e.stopPropagation();
  };
  const disarm = () => {
    document.removeEventListener("click", swallow, true);
    clearTimeout(releaseGuardTimer);
  };
  document.addEventListener("click", swallow, true);
  releaseGuardTimer = setTimeout(disarm, 400);
}

/**
 * Copy onto a sidebar playlist. Deliberately an ADD, never a move: dragging
 * out of a playlist leaves the original in place. Failures stay silent like
 * every other fire-and-forget playlist edit — the next reconciliation paints
 * the server's truth.
 */
function commitToPlaylist(pid, uri) {
  const target = library.find((p) => p.id === pid);
  api.addPlaylistTracks(pid, [uri])
    .then(() => {
      if (target) promotePlaylist(target.id);
      api.touchPlaylistActivity(pid).catch(() => {});
    })
    .catch(() => {});
}

/** Brief accent wash on the playlist row that received the drop. WAAPI, not
    a class: Svelte reconciliation cannot strip an animation mid-flight. */
function pulse(rowEl) {
  try {
    rowEl.animate(
      [
        { background: "color-mix(in srgb, var(--rose-ink) 22%, transparent)" },
        { background: "rgba(255,255,255,0)" },
      ],
      { duration: 550, easing: "ease-out" },
    );
  } catch {
    /* No WAAPI: the drop itself is feedback enough */
  }
}

/**
 * Ghost exit paths. Cancel flies the row home — the gesture undoes itself in
 * front of the user, and the pill unfolds back into the row's footprint on
 * the way (same clock as the compression, run backwards). A committed drop
 * dissolves in place: the real row is already where the ghost was, so
 * flying it anywhere would be a lie.
 */
function retireGhost(mode) {
  if (!ghostWrap) return;
  const wrap = ghostWrap;
  const pill = pillEl;
  ghostWrap = null;
  pillEl = null;
  clearTimeout(cleanupTimer);

  if (mode === "return" && originRect) {
    /* The inline transition list must carry width itself: writing it replaces
       the stylesheet's transition property wholesale. The pill re-declares
       its own margins to match the flight rather than var(--d2). */
    const flight = "220ms cubic-bezier(0.2, 0, 0, 1)";
    wrap.style.transition =
      `transform ${flight}, width ${flight}, opacity 220ms linear`;
    wrap.style.transform =
      `translate3d(${originRect.left}px, ${originRect.top}px, 0) scale(1)`;
    wrap.style.width = `${originRect.width}px`;
    if (pill) {
      pill.style.transition = `margin-left ${flight}, margin-top ${flight}`;
      pill.style.marginLeft = "0px";
      pill.style.marginTop = "0px";
    }
    wrap.style.opacity = "0.55";
    cleanupTimer = setTimeout(() => wrap.remove(), 240);
  } else {
    wrap.style.transition = "opacity 130ms linear";
    wrap.style.opacity = "0";
    cleanupTimer = setTimeout(() => wrap.remove(), 150);
  }
}

