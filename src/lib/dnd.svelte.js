/**
 * Track drag-and-drop: the one gesture controller behind "grab a song and
 * move it".
 *
 * Two drops mean something:
 *   - inside an owned playlist's own table -> reorder (playlist4 MOV,
 *     Web-API insert-before semantics)
 *   - onto a sidebar playlist row          -> move there (add, then remove
 *     from the source) or, from any other list, plain add
 *
 * Everything spatial runs in ONE requestAnimationFrame loop that exists only
 * while a drag is live: hit-testing, the insertion gap, autoscroll of the two
 * scrollable regions (the content pane and the library rail), and the ghost's
 * position. `pointermove` only records coordinates; it never reads layout.
 * When no drag runs, this module's steady-state cost is exactly zero.
 *
 * Reactivity contract: `trackDrag` is the reactive projection components read
 * (TrackList draws the insertion bar and parts its rows around the gap;
 * Sidebar lights up its drop targets). The DOM ghost is deliberately outside
 * Svelte — moving one transform per frame must not re-render anything.
 */

import { api, library, promotePlaylist } from "./state.svelte.js";

export const trackDrag = $state({
  active: false,
  /** The grabbed row's track object and uri. */
  track: null,
  uri: "",
  /** Set only when the source list is an owned playlist (move semantics). */
  sourcePlaylistId: null,
  sourceIndex: -1,
  /**
   * True when dropping onto another playlist should also remove the track
   * from the source playlist. Dropping back on the source itself cancels.
   */
  move: false,
  /** Latest pointer position, window coordinates. */
  x: 0,
  y: 0,
  /** Wrapper element of the hovered reorder zone, or null. */
  listEl: null,
  /** Insertion gap in ORIGINAL list coordinates (0..length); -1 when none. */
  gap: -1,
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

let press = null; // pending press before the threshold, then live drag state
let ghostWrap = null; // positioned .tl clone holding the row clone
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
  window.addEventListener("pointercancel", onPressUp);
  window.addEventListener("keydown", onPressKey, true);
}

function releaseListeners() {
  window.removeEventListener("pointermove", onPressMove);
  window.removeEventListener("pointerup", onPressUp);
  window.removeEventListener("pointercancel", onPressUp);
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

function activate() {
  const ctx = press.ctx;
  press.active = true;

  layer = ensureLayer();
  ghostWrap = buildGhost(press.rowEl);
  press.grabX = trackDrag.x - originRect.left;
  press.grabY = trackDrag.y - originRect.top;

  const d = trackDrag;
  d.active = true;
  d.track = ctx.track;
  d.uri = ctx.track.uri;
  d.sourcePlaylistId = ctx.playlistId ?? null;
  d.sourceIndex = ctx.index;
  d.move = !!ctx.playlistId;
  d.listEl = null;
  d.gap = -1;

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
 * Clones the pressed row into the fixed layer. The clone keeps the real row
 * markup (art, two-line title, columns) by copying `.tl`'s inline `--cols`
 * template and density class — everything visual about a row comes from
 * app.css through those classes, so the copy is faithful with no re-styling.
 */
function buildGhost(rowEl) {
  const tl = rowEl.closest(".tl");
  const rect = rowEl.getBoundingClientRect();
  originRect = rect;

  const wrap = document.createElement("div");
  wrap.className = "tl tl-drag-ghost";
  if (tl?.classList.contains("dense")) wrap.classList.add("dense");
  const cols = tl?.style.getPropertyValue("--cols");
  if (cols) wrap.style.setProperty("--cols", cols);
  wrap.style.width = `${rect.width}px`;

  const row = rowEl.cloneNode(true);
  row.classList.add("is-ghost");
  row.classList.remove("current");
  wrap.appendChild(row);

  layer.appendChild(wrap);
  return wrap;
}

function positionGhost() {
  if (!ghostWrap) return;
  ghostWrap.style.transform =
    `translate3d(${trackDrag.x - press.grabX}px, ${trackDrag.y - press.grabY}px, 0) scale(1.028)`;
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
  let gap = -1;
  for (const z of zones) {
    if (!z.el.isConnected) continue;
    const r = z.el.getBoundingClientRect();
    if (x < r.left || x > r.right || y < r.top || y >= r.bottom) continue;
    const meta = z.meta();
    if (meta.canReorder && meta.length > 0) {
      gap = Math.max(0, Math.min(meta.length, Math.round((y - r.top) / ROW_H)));
      zoneEl = z.el;
    }
    break; // at most one zone contains the point; stop at the first overlap
  }
  const dropRow =
    zoneEl ? null : (document.elementFromPoint(x, y)?.closest?.(".lib-row[data-pid]") ?? null);
  return { zoneEl, gap, dropRow };
}

/**
 * One layout pass per frame while dragging: hover state, insertion gap,
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

  const { zoneEl, gap, dropRow } = hitTest(d.x, d.y);

  if (d.listEl !== zoneEl) d.listEl = zoneEl;
  if (d.gap !== gap) d.gap = gap;
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
    if (d.listEl === null && d.gap < 0 && p.dropRow === null) {
      const hit = hitTest(d.x, d.y);
      d.listEl = hit.zoneEl;
      d.gap = hit.gap;
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
      /* Gap g is an insertion point in original coordinates; the final index
         loses one when the track came from above the gap. Equal => no-op. */
      const to = d.gap <= from ? d.gap : d.gap - 1;
      if (to !== from) {
        meta.run(from, to);
        outcome = "reorder";
      }
    } else if (p.dropRow) {
      const pid = p.dropRow.dataset.pid;
      if (!(d.move && pid === d.sourcePlaylistId)) {
        commitToPlaylist(pid, d.uri, d.sourcePlaylistId, d.move);
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
  d.gap = -1;

  if (wasActive) {
    document.documentElement.classList.remove("dragging-track");
    retireGhost(outcome === "cancel" ? "return" : "dissolve");
    gestureEndedAt = performance.now();
  }
}

/**
 * Move-or-add onto a sidebar playlist. Add first: until the remove confirms,
 * the worst case is the track living in both playlists, never in neither.
 * Failures stay silent like every other fire-and-forget playlist edit — the
 * next reconciliation paints the server's truth.
 */
function commitToPlaylist(pid, uri, sourcePid, move) {
  const target = library.find((p) => p.id === pid);
  api.addPlaylistTracks(pid, [uri])
    .then(() => {
      if (target) promotePlaylist(target.id);
      api.touchPlaylistActivity(pid).catch(() => {});
      if (move && sourcePid && sourcePid !== pid) {
        api.removePlaylistTracks(sourcePid, [uri]).catch(() => {});
      }
    })
    .catch(() => {});
}

/** Brief accent wash on the playlist row that received the drop. WAAPI, not
    a class: Svelte reconciliation cannot strip an animation mid-flight. */
function pulse(rowEl) {
  try {
    rowEl.animate(
      [
        { background: "color-mix(in srgb, var(--accent) 22%, transparent)" },
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
 * front of the user. A committed drop dissolves in place: the real row is
 * already where the ghost was, so flying it anywhere would be a lie.
 */
function retireGhost(mode) {
  if (!ghostWrap) return;
  const wrap = ghostWrap;
  ghostWrap = null;
  clearTimeout(cleanupTimer);

  if (mode === "return" && originRect) {
    wrap.style.transition =
      "transform 220ms cubic-bezier(0.2, 0, 0, 1), opacity 220ms linear";
    wrap.style.transform =
      `translate3d(${originRect.left}px, ${originRect.top}px, 0) scale(1)`;
    wrap.style.opacity = "0.55";
    cleanupTimer = setTimeout(() => wrap.remove(), 240);
  } else {
    wrap.style.transition = "opacity 130ms linear";
    wrap.style.opacity = "0";
    cleanupTimer = setTimeout(() => wrap.remove(), 150);
  }
}
