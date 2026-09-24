# Glass redesign: handoff

A temporary working note for the next agent. **Delete this file in the commit that finishes the redesign.**

## Where it stands
The app is glass over an ambient haze taken from the now-playing Canvas or cover. The first pass is commit `64b9b83`; read its message. The commit that adds this file brings the second design pass:
- **Background:** the "A light" background (the real artwork seen through light frost: haze blur about 6px, frost 12px), lined up with the video, with a brighter band at the seam.
- **Seam:** corrected to the owner's spec. The Canvas is never obstructed or widened. Its top-left and bottom-left corners are rounded (18px). The pane and bar sit left of it with a normal gap, rounded corners and a glass rim.
- **Tall windows:** a Canvas stops growing at 1.2× its native pixels, and its top and bottom fade into the haze.
- **Also in:** the wide seek bar again, glass on every popup, a smooth rim at DPR 1, and `glass.test.js`, which holds the text contrast on glass.

`bun test` passes all 60 tests, and `bun run build` passes.

## What the owner wants (don't re-litigate these)
- **The reference:** their original AI concept, a dark UI that is richly colourful and lit by the record.
- **Haze motion:** still at rest; a fast (~1 s) crossfade on track change; zero work at rest.
- **Over the video:** no scrims; the text floats on it with a glyph shadow.
- **One panel mode:** cover and Canvas share one layout.
- **Scrollbars:** overlay, shown on hover.
- **Performance:** it matters as much as looks. Measure it; don't assume.

## Remaining work, in priority order
1. **Background leftovers,** in the section below:
   - check the seam on bright Canvases (Spectrum, the "Nothing Can Touch Us" jellyfish);
   - confirm the corner anti-aliasing in the real WebView2 at 150%;
   - run the perf check;
   - update the `ambient.svelte.js` header.
2. **Active nav contrast:** `.nav-item.active` misses its contrast target over the brightest haze (secondary 3.55:1, tertiary 2.37:1) whatever the tint. Fix it in the rule, for example with a darker active fill, and extend `glass.test.js` to cover it.
3. **Layout leftovers,** in the section below.
4. **The wiring checklist,** further down: glass on the remaining cards, strips and dialogs; dialog scrollbars; the `--bg-*` to raise-token migration. The overlay/popup section is DONE; skip it.
5. **Perf regression check:** with a Canvas playing and the playing row visible, the absolute GPU numbers are above the old baseline (GPU % of a core: idle 7.8, eq 22.4, scroll 67.5). Find out whether it's the haze or the glass. The new haze must still do zero work at rest.

## How to verify
- **Dev harness:** start the dev server with `bun run dev`, or the "ui-preview" entry in `.claude/launch.json`. Open `http://127.0.0.1:1420/dev/ui-harness.html?real`, which serves the owner's real library, covers and Canvases from `dev/.real-cache` (see `dev/real-bridge.js`). For live reads, quit the app and run `bun dev/real-app.js`.
- **Test tracks:**
  - Automatic Yes, Inside Out and Lost In Japan (cover only) are in "Zedd - Radio": `__harness.navigate('playlist','5XtFXqFoTwqe4PQWSVbOyY')`.
  - Nothing Can Touch Us is in "headphone demo".
  - Spectrum: wrap `__TAURI_INTERNALS__.invoke` so that `browse_canvas` returns `https://canvaz.scdn.co/upload/licensor/7JGwF0zhX9oItt9901OvB5/video/5171946a962b4154adf91a58092e5c1d.cnvs.mp4`.
- **Screenshots and perf:** use an isolated Microsoft Edge (`--remote-debugging-port=<port> --user-data-dir=<temp dir>`) driven over CDP. It's the same engine as WebView2. Compare GPU and renderer CPU against a `prefers-reduced-transparency: reduce` baseline.
- **`scratchpad/` paths below:** they mean `C:\Users\Eduard\AppData\Local\Temp\claude\C--Users-Eduard-vibe-coded-spotify-renderer\90134ba5-3694-45cf-8856-35ea67199385\scratchpad\`. That is a temp folder of helper scripts and screenshots from the previous session. They may be gone, and none of them is required.

---

## Background agent's notes

# Haze / frost phase 2: handoff

The tree is clean: `bun test` passes (60), `bun run build` passes, nothing is committed. Only my files were touched, plus the GLASS tokens block in `src/styles/app.css`.

## Done and verified
- **Only one pipeline remains (option A).** `src/lib/haze.js` has a single `renderHaze()`. The old tone/mirror renderer, `VARIANTS`, the dev switch and the localStorage flag are gone from `haze.js`, `haze.worker.js` and `Ambient.svelte`.
  - Pipeline stages:
    1. Read the source at `PROBE` = 360.
    2. `gradeInto`: key on the median, keep chroma, vibrance, the warm/brown fold, then `capInto` (perceived and luminance peaks).
    3. Local contrast (`LOCAL_SIGMA` / `LOCAL_GAIN`).
    4. Lay out: mirror at the video edge, magnified leftward to `ZOOM` 2.4 over `ZOOM_REACH` 0.55.
    5. `LENS` blur of 6 CSS px, with chroma restored in pass 1.
    6. Ceilings: peak, `C_MEAN`, `P_MEAN`, `CEILING.mean` gain.
    7. `emit()`: the encode lookup table, dither, the seam band, and the half-resolution frost (`halve` then `frostOf`, which returns `frostW` and `frostH`).
  - `Ambient.svelte`: `DENSITY` 2, `MAX_W` 1000. `geometry()` now passes `px` and `video`, where `video` is `haze.anchor.video` converted to haze px. `sameGeo` also compares the video rect.
  - The worker makes the frost bitmap at its own size.
- **Render time in Edge**, median worker ms per render, re-rendering by nudging the viewport (`scratchpad/bg/rtime2.js`):
  - Automatic Yes: 285 → 195
  - jelly: 356 → 189
  - Lost In Japan cover: 270 (no Edge baseline was taken; in Bun it went 561 → 253)
  - Canvases at 1600×900: the haze is 800×450 and the frost 400×225, about 5.2 MB across 3 layers.
- **Frost and tints:**
  - `--frost-plane` is now `blur(12px) saturate(1.45) brightness(0.7)`.
  - `--tint-chrome` went 0.33 → 0.36. `--tint-pane` stays at 0.38.
  - New `src/lib/glass.test.js` takes the worst colour the haze can produce, frosts it as CSS does, then checks body ≥ 7:1, secondary ≥ 4.5:1 and tertiary ≥ 3:1 on both planes: plain, under the sheen, and on a hovered row. It passes; the tightest case is `--fg-3` at 3.04:1 on a hovered chrome row. At the old 0.33 tint, secondary text on a hovered chrome row was 4.40 and failed.
- **Seam:**
  - The layout agent publishes `haze.anchor.video = {x, y, w, h}`, the visible video rect in CSS px, from `NowPlayingPanel.placeCanvas()`. The haze frames the picture to that rect: `frameOf()` and `framing()`.
  - The seam band covers `SEAM` 44 px left of the video edge and `SEAM_IN` 22 px inside it (for the video's own rounded corners). In that strip the haze is the video's own linear light at `SEAM_DIM` 0.9, fading into the graded haze. It lives only in the raw haze layer; the frost is made without it. A test covers this.

## Left to do (phase 2 items)
1. Done.
2. Done. **Open for the layout agent:** the active nav item (`.nav-item.active`: raise-2 plus the accent wash) can't hold secondary or tertiary text contrast over the brightest haze with any sensible tint: 3.55 and 2.37 at worst. Over pure black it is 5.55 and 3.70, so it is the state's own lighting. Fix it in that rule, or keep only body-grey text on active items.
3. Built and unit-tested, but **not visually verified on a bright video.** Shoot the jelly and Spectrum seams: crop about x 1040–1140 around the video's left edge, at the top and the bottom corners. Tune `SEAM`, `SEAM_DIM` and `SEAM_IN` in `haze.js` if the gap reads too bright or too dark.
4. **Anti-aliasing is unresolved.** In Edge at DPR 1 and 1.5 the pane and bar corners looked smooth in nearest-neighbour zooms (`scratchpad/bg/z15-*.png`); the rim covers the clip edge. Verify in WebView2.
   - Suspected cause: the old frost was a 320×180 canvas (5 CSS px per pixel) with `clip-path: path()` on a composited, upscaled canvas.
   - If it still shows, clip a full-window wrapper `div` instead of the canvas: in `Ambient.svelte`, move the `.frost` clip-path to a wrapper around the frost canvas, and make sure the wrapper doesn't get `will-change`.
5. **Performance is not measured.** Use `scratchpad/frostperf2.js` as the pattern: isolated Edge, Canvas playing, GPU % of a core for idle, playing row visible, and scroll. Compare against 7.8 / 22.4 / 67.5. At rest there must be zero haze work (`window.__haze` counters stay flat).
6. **The `src/lib/ambient.svelte.js` header is stale.** It still describes the old seam: the video must not run under a plane, square right-hand corners, and `haze.anchor = {left}` only. Rewrite it for:
   - `haze.anchor = {left, video}` (the video rect in CSS px, published by `placeCanvas`);
   - rounded planes with a normal gap;
   - the seam band;
   - a 12px frost at half resolution;
   - the contrast test.

   Also update the `Ambient.svelte` header (it still says "a fifth of the window's size"). Then take the final screenshots of Automatic Yes, Lost In Japan, Inside Out, jelly and Spectrum at 1600×900, plus seam and corner zooms.

## Decisions and why
- **Option A** (the owner's pick): a 6px haze lens and a 12px frost. The owner wanted real structure (shapes, edges, colour gradients) instead of blobs, and the magnified mirror avoids the Rorschach look while staying continuous at the video edge.
- **The grade is keyed to the picture's median, with a mean-chroma budget and a perceived-lightness cap.** This fixes the flat bright-blue wash on the jelly and the too-bright Spectrum, while keeping colourful sources rich. The luminance `CEILING` (peak 0.10, mean 0.045) is unchanged, so the glass calibration still holds.
- **The seam band stays out of the frost,** so the gap and corner cut-outs carry the video's real brightness while type on the glass stays calibrated. The glass edge is where the light visibly dims.

## Gotchas
- **Harness:** `scratchpad/bg/scene.js` and `shoot.js` drive `http://127.0.0.1:1420/dev/ui-harness.html?real` through a CDP bridge (`bg/cdp-server.js`, HTTP on :9461, isolated Edge on :9452).
- **Concurrent edits:** the other agent's edits can break the harness mid-edit (a missing `morphing` export once). Wait and reload.
- **Tints:** `glass.test.js` reads the tokens from `app.css` using the first match, which is `:root`. The reduced-transparency block redefines them further down.
- **Test geometries:** `renderHaze` needs `geo.px` (CSS px per haze px). The tests default to 5.
- **The band exceeds the peak on purpose.** The ceiling test excludes the seam strip.

---

## Layout agent's notes

# Layout / glass handoff (seam, tall windows, seek bar, popups, corner AA)

Nothing is committed. `bun run build` passes and `bun test` passes (60/60). `?real` renders with no console errors.
The screenshots are in `scratchpad/ui/` and `scratchpad/ui/report/`.
The background agent owns the haze files (`Ambient.svelte`, `haze*.js`, `ambient.svelte.js`) and the GLASS `--frost-*`/`--tint-chrome`/`--tint-pane`/`--z-*` tokens.

## Done and verified (isolated Edge on :9451, harness ?real)
- **Seam (the corrected spec):**
  - The pane and bar keep all four corners and the rim.
  - The panel keeps the gutter on its left: `.np-panel` margin is `-g -g -g 0`. It stays flush with the window on its top, right and bottom.
  - The video is never under a plane. There is no z-order change: `.app` keeps `z-index: 1`, and there are no `--z-*` uses.
  - The square-corner rule `.app.has-panel > .pane/.player` is deleted.
  - Shots: `report/seam-insideout-*.png`, `n1-seam-*.png`, `report/seam-lostinjapan-cover-*.png`. The old build is in `before-seam-*.png`.
- **Rounded video corners:** `.np-panel { border-radius: var(--np-radius) 0 0 var(--np-radius); overflow: hidden }` with `--np-radius: 18px`. That is a step wider than the planes' 12px, because 12 looked pinched on a full-height surface. Nothing paints under the video, so there is no fringe. Zooms: `sheet-aa1.png` and `sheet-aa15.png` (d-vidTL, f-vidBL).
- **Tall windows** (`placeCanvas` in NowPlayingPanel.svelte):
  - The video covers the panel, centred, with a scale capped at `CANVAS_LIMIT / devicePixelRatio`, where `CANVAS_LIMIT = 1.2`.
  - When capped, the edges that no longer reach the panel dissolve into the haze through `.np-stage.float`, an eased mask built from `--x0/--x1/--y0/--y1/--dx/--dy`. The dissolve length is `min(96, 3×gap)`, so it comes on continuously.
  - `--panel-w` also caps the column at 864px/`--dpr` minus the gutter (a 720-wide Canvas at 1.2×) and rounds to whole device pixels. The panel keeps `--dpr` on `<html>` current.
  - Measured at 1600×900, 1600×1400, 1200×1600 and 2560×1440, at DPR 1 and 1.5, with 1080p (Automatic Yes, Nothing Can Touch Us) and 720p (Honest, Inside Out) Canvases. The device scale is never above 1.2. Shots: `tall-{ay,nctu,hon}-WxH@dpr.png`. The capped case is shown in `tall-hon-1600x1400@1.5.png`.
- **Publishing:** `haze.anchor = { left, video: {x,y,w,h} | null }`. `video` is the VISIBLE content rect in window CSS px, or null when the video isn't shown.
- **Seek bar (app.css, PLAYER BAR):**
  - Closed, the grid is `minmax(200px,1fr) minmax(0,2fr) minmax(max-content,1fr)`, and the centre's max-width is 900.
  - With the panel open and the bar ≤1180px (`@container player` + `.has-panel`), `.p-now` is hidden, the grid becomes 2 columns, and the column gap is `--s7`.
  - Rail widths: 1600 closed 648 / open 672; 1200 closed 448 / open 298 (was ~300 / 226). Shots: `bar-*.png`.
- **Popups:**
  - `glass-overlay` is on: saved-in (with a `.p-saved-scroll` child using `use:scrollbar`, and the bridge moved to `::after`), volume error, `.sel-list`, `.seek-tip`, ConfirmDialog (love edge moved to `::after`, `::backdrop` rule deleted), and PlaylistCleanup.
    - PlaylistCleanup changes: `--sheet-gutter` removed; scrollbars on body, results and suggestions; th is a strip; notice is a `glass-card`; the input is pressed in.
    - The lightbox steppers are `glass-plate`.
  - The view "…" menus, speed menu and row menus already had the class.
  - **Root cause of "plain":** the class was already on those menus, but the overlay tint had a grey floor and let only about 15% of the backdrop through. Over the dark pane that reads as the old flat grey.
  - **Fix:** `--tint-overlay: rgb(8 9 13 / 0.74)`, which has no grey. `.glass-overlay` now carries the page's `--tone-glow`/`--tone-wash` pooled in two corners, plus a sheen and an inset top highlight. The luminance still stays within the over-white calibration (fg ≥ 7, fg-2 ≥ 4.5 at the centre).
  - PlayerBar sets `--tone-*` from the playing cover, so the speed menu and saved-in panel glow with the record.
  - Shots: `report/pop-*.png`, `report/dialog-*.png`, `sheet-pops.png`.
- **Corner AA:**
  - **Cause:** a 1px, high-contrast rim on a 12px radius at DPR 1 "ropes". Partial pixels blended in gamma space read darker, so the arc looks beaded. The measured pixel coverage was correct, so this is not missing AA.
  - **Fix:** the rim is `padding: var(--rim-w)`. Under `(resolution < 1.25dppx)` it becomes 1.5px with the `--glass-rim` alphas at ×0.74, and the arc reads as one line.
  - The fractional pane and video edges (1077.75) were fixed by rounding `--panel-w`.
- **Morph:** open and close still use the original snapshots (`layout.js` is unchanged). Screencast: `sheet-morph.png`.
- **Perf** (1200×800, Automatic Yes; Edge must run with `--disable-features=CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows`, otherwise an occluded window reads ~1%):
  - Absolute numbers are higher than the old baseline for both builds, because the background agent's haze work is live: eq visible ≈28 / idle ≈10 / scroll ≈62 GPU.
  - A/B against HEAD-equivalent CSS in the same session:
    - eq visible: +1.0 GPU. About 0.7 of that is the rounded clip on the video.
    - idle: equal.
    - scroll: 1.5–3 lower.

## Left
1. **Perf trim (optional):** clipping the stage instead of the panel costs about 0.4 less GPU.
   - Remove `overflow`/`border-radius` from `.np-panel`, and add `border-radius: var(--np-radius) 0 0 var(--np-radius); overflow: hidden` to `.np-stage` and `.np-scroll`.
   - Give `.np-head` `border-top-left-radius: var(--np-radius)`, so its `.over` veil doesn't show a square corner.
   - Measured with CSS injection only. Not applied.
2. **Tone for page-less popovers:** with the panel closed, menus on pages without a tone fall back to `:root --tone-*`. If you want haze colour there, `haze.veil` could feed a root var (the value belongs to the haze agent).
3. **Not converted:**
   - The DnD pill `.tl-drag-pill` (app.css, TRACK DRAG-AND-DROP) is still `--bg-2`.
   - `TrackEditWaveform` and `TrackEditorView` (§4 of wiring-checklist.md).
   - The §2 strips in DiscographyView, HistoryView and TrackEditorView.
   - `glass.test.js` (checklist §6) is still unwritten. Its overlay rows must use the new `--tint-overlay` and the tone gradients.
4. **The harness Zedd-Radio header shows no title.** It looks like a data issue; I didn't investigate.

## Decisions (why)
- **Cover + cap, not contain:** normal windows fill the panel. Only past 1.2× does the video stop and let its edges dissolve. That keeps it continuous and reads as a lit window in its own haze, not a clamp.
- **Nominal 720 in `--panel-w`:** the column width must not change per track. 720 is the common and smaller usual width; canvases measure 720×1280 or 1080×1920, with a few smaller ones that float and dissolve.
- **Overlays stay dark:** contrast over a white backdrop bounds transmission to about 0.3 whatever the tint, and it can't make a menu over a dark pane look lighter. A luminosity-blend child doesn't see the backdrop-filter image in Chromium (tested: it renders flat grey). So the glass look comes from colour and light, not transparency.

## Gotchas
- **After a scripted edit, `touch` the file.** Batch edits through Bun scripts: a bash heredoc or `bun -e` with apostrophes or em dashes broke twice. `scratchpad/ui/edit-lib.js` is the helper.
- **Vite HMR full-reloads the harness** whenever the haze agent edits their files, and `?real` then reboots to the owner's saved track. Scripts must set up their scene every time (`scratchpad/ui/playp.js`, `pop.js`, `capture.js`, `setup.sh`, `ensureplay.js`).
- **`perf.js` in `scratchpad/ui` is patched** to leave focus emulation to the bridge (`cdp-server.js`, :9460, which has `/reload`, `/vp`, `/shot`, `/eval`, `/cmd`, `/logs`). Otherwise the page blurs, and the Canvas and the eq pause.
- **CDP `/shot` with `scale>1` re-rasters,** which hides aliasing. For pixel-true zooms, use `zoom.ps1` on a scale-1 shot.
- **Don't swap HEAD files into the live tree for A/B.** The page came up unstyled. Emulate HEAD with injected CSS instead.

---

## Wiring checklist (from the first pass; its overlay/popup section is done)

# Glass wiring checklist

The design and its reference implementations are finished. Everything below is mechanical: apply the listed class or token, delete the listed old declarations, and check each gotcha. Nothing here needs a design decision. Where a decision was needed, it has already been made and is written down here.

Repo: `C:\Users\Eduard\vibe-coded\spotify_renderer`. Read AGENTS.md first.

**What not to edit:**
- `src/components/Ambient.svelte`, `src/lib/haze.js`, `src/lib/haze.worker.js`, `src/lib/ambient.svelte.js`, `src/lib/haze.test.js` belong to the haze agent.
- `dev/` and `vite.config.js`.

**Vite watcher gotcha:** after an edit made by a script (Bun, or `sed -i`), run `touch <file>`. The watcher has missed quick successive writes, and the dev server then serves the stale file.

---

## 0. The material (already in `src/styles/app.css`; read it before you start)

**Tokens.** All of these are in `:root`, in the "GLASS" block:
- **Frosts:**
  - `--frost-plane: blur(36px) saturate(1.45) brightness(0.7)`
  - `--frost-strip: blur(20px) saturate(1.7) brightness(0.9)`
  - `--frost-overlay: blur(28px) saturate(1.6) brightness(0.7)`
  - `--frost-plate: blur(8px) saturate(1.5) brightness(0.72)`
- **Tints:**
  - `--tint-chrome: rgb(10 11 14 / 0.33)`
  - `--tint-pane: rgb(11 12 15 / 0.38)`
  - `--tint-strip: rgb(13 14 17 / 0.62)`
  - `--tint-overlay: rgb(19 20 24 / 0.78)`
  - `--tint-sheet: rgb(19 20 24 / 0.5)`: a modal overlay (`:modal`) uses this automatically, over its `--dim-modal: rgb(4 5 7 / 0.5)` backdrop.
  - `--tint-plate: rgb(9 9 11 / var(--plate-a, 0.42))`
- **Light and lift:**
  - `--lift-card: rgba(255,255,255,0.03)`
  - `--raise-1/2/3`: white at 0.03, 0.075 and 0.11, used for hover and selected states on glass.
  - `--glass-sheen` (white 0.035 fading out by 34%, from the top-left) and `--glass-rim` (a 145° gradient from white 0.34 to 0.03, with a 0.08 return at the bottom right).
  - `--lift-plane`, `--lift-raised`, `--lift-overlay`.

**Classes.** These are in the "THE MATERIAL" block, near the top of app.css:
- `.glass-chrome`, `.glass-pane`, `.glass-card`, `.glass-overlay` (becomes a SHEET automatically when `:modal`) and `.glass-plate` get the rim through `::before` (z-index 60, mask ring).
- `.glass-strip` is frost and tint only, with no rim.

**Rules for any surface that wears a glass class:**
1. **The host must be positioned** (relative, absolute, fixed or sticky), because the rim is absolutely positioned. Never put `position` in the material classes: an author `position` beats the UA's `dialog:modal { position: fixed }`.
2. **The host's `::before` is taken by the rim.** If the element already uses `::before`, move that decoration to `::after`.
3. **Delete the old surface declarations on that selector:** `background`/`background-color` with `--bg-2`, `--bg-3`, `--bg-4` or `--bg-sheet`; `border: 1px solid var(--line-2)`; `box-shadow: 0 18px 40px…` or `0 24px 64px…`; any per-component `backdrop-filter`; and the `@supports not (backdrop-filter…)` fallback. Also set `border: 0` on popovers, because the UA gives `[popover]` a 3px border.
4. **No continuously animating content inside an element that has a backdrop-filter.** The planes (`.glass-chrome`, `.glass-pane`) have NO backdrop-filter: their frost is the haze layer's pre-frosted twin, shown under every element that wears `use:frost` (from `src/lib/ambient.svelte.js`, worn by `.sidebar`, `.pane`, `.player`). A new plane surface must wear both the class and `use:frost`. Strips, overlays and plates keep a live backdrop-filter.
5. **Hovered or selected items on overlay glass** use `rgba(255,255,255,0.08)` (menus) or `--raise-2`, never `--bg-4`, and their text goes to `--fg` (on a hovered item only `--fg` holds 7:1).
6. **Fields on glass** are pressed in: `background: rgb(0 0 0 / 0.22); border: 1px solid rgba(255,255,255,0.08)`. The reference is `.credits-filter`.

**Reference implementations to copy:**
- **Overlay menu:** the three menus in `src/components/TrackList.svelte`. The pattern is a popover with `class="menu glass-overlay"`, a child `<div class="menu-scroll" use:scrollbar style:max-height=…>` holding the items, and in the scoped CSS `.menu { padding: 0 }` and `.menu-scroll { padding: var(--s1); overflow-y: auto; overscroll-behavior: contain }`.
- **Overlay dialog (sheet):** `src/components/CreditsDialog.svelte` together with the `.credits-*` rules in app.css. The pattern is `<dialog class="… glass-overlay">`, `border: 0; border-radius: var(--r4)`, no own `::backdrop` rule (the material supplies the dim), and `use:scrollbar` on the body scroller, whose parent must be positioned.

## 1. Overlays → `glass-overlay`

| Where | Selector / element | Do |
|---|---|---|
| `src/components/PlayerBar.svelte` | `.p-saved-panel` | Add `glass-overlay` in the markup. In the scoped CSS, delete `border`, `background: var(--bg-2)` and `box-shadow`. It already uses `::before` for the hover bridge: rename that to `::after` (same rules). `.p-saved-row:hover` changes from `var(--bg-3)` to `rgba(255,255,255,0.08)`. It scrolls (max-height 232px), so wrap the rows in a child `<div class="p-saved-scroll" use:scrollbar>` that takes `max-height: 232px; overflow-y: auto`, and delete `overflow-y` from the panel. |
| `src/components/PlayerBar.svelte` | `.p-volume-error` | Add `glass-overlay`. Delete `border`, `background: var(--bg-2)` and `box-shadow`. It is already `position: absolute`. |
| `src/components/Select.svelte` + app.css `.sel-list` | `<ul class="sel-list">` | Add `glass-overlay`. In app.css `.sel-list`, delete `border`, `background: var(--bg-2)` and `box-shadow`, and set `border-radius: var(--r3)`. `.sel-list li` gets `border-radius: var(--r2)`. `.sel-list li.active` changes from `var(--bg-4)` to `rgba(255,255,255,0.08)`. |
| `src/components/Slider.svelte` + app.css `.seek-tip` | `.seek-tip` | Add `glass-overlay`. Delete `border`, `background: var(--bg-4)` and `box-shadow`. Keep `border-radius: var(--r1)` or make it `var(--r2)`. |
| `src/components/ConfirmDialog.svelte` + app.css `.confirm-dialog` | `<dialog class="confirm-dialog">` | Add `glass-overlay`. In app.css `.confirm-dialog`, delete `border`, `background: var(--bg-sheet)` and `box-shadow`, and set `border: 0; border-radius: var(--r4)`. **Rename `.confirm-dialog::before` (the 2px love edge) to `.confirm-dialog::after`.** Delete the `.confirm-dialog::backdrop` rule; the material's `:modal::backdrop` supplies the dim. Note that PlaylistCleanup reuses the `confirm-dialog` class (next row). |
| `src/components/PlaylistCleanup.svelte` | `<dialog class="confirm-dialog cleanup-dialog">` | Gets the material through `confirm-dialog`. Add `glass-overlay` in the markup too. Delete `--sheet-gutter` and every `calc(… + var(--sheet-gutter))` (use the plain value). `.cleanup-body`: delete `scrollbar-gutter: stable` and add `use:scrollbar` (its parent `.cleanup-sheet` needs `position: relative`). `.cleanup-results`: add `use:scrollbar` (its parent `.cleanup-preview` needs `position: relative`). `.cleanup-results th` (sticky head): change `background: var(--bg-sheet)` to the strip (§2). `.cleanup-suggestions` (absolute dropdown): add `glass-overlay`, delete `border`, `background: var(--bg-2)` and `box-shadow`, and add `use:scrollbar`. Its parent `.cleanup-input-wrap` is already relative, but the list is the scroller, so wrap its children the way TrackList's menus do. Buttons in it: hover and `[aria-selected=true]` change from `var(--bg-4)` to `rgba(255,255,255,0.08)`. `.cleanup-notice`: `background: var(--bg-2)` becomes `glass-card` (see §3). `.cleanup-input`: use the pressed field (rule 6). `.cleanup-keep input`: `background: var(--bg-2)` becomes `var(--raise-1)`. |
| `src/components/GalleryLightbox.svelte` | `.shot-step`, `.shot-close` | Add `glass-plate` to both buttons. Delete `background: color-mix(… --bg-2 …)` and `box-shadow: var(--ring)`. Hover: `color: var(--fg)`, with a lit plate as `::after` like `.np-round` in app.css (white 0.1). They are already absolute. Leave the lightbox's own `::backdrop` (0.92) as it is: it is a darkroom, not a sheet, and it does not wear `glass-overlay`. |
| `src/views/PlaylistView.svelte`, `AlbumView.svelte`, `ArtistView.svelte` | `.menu` action menus | **Already done** (`glass-overlay` added). Nothing to do. |

## 2. Sticky strips → strip frost

**Pattern:** keep the "transparent at rest, material only when `.stuck`" behaviour. The stuck state gets `background: var(--tint-strip); -webkit-backdrop-filter: var(--frost-strip); backdrop-filter: var(--frost-strip); box-shadow: inset 0 -1px 0 var(--line-2)`. Delete the `color-mix(… --bg-1 …)` backgrounds and the `@supports not (backdrop-filter…)` blocks.

- **Already done:** `.topbar` (it wears `.glass-strip` while content is under it; see TopBar.svelte `covering`) and `.tl-head.stuck`.
- `src/views/DiscographyView.svelte`: `.dx-controls.stuck`.
- `src/views/HistoryView.svelte`: `.history-block.stuck`.
- `src/components/PlaylistCleanup.svelte`: `.cleanup-results th`. This one is always stuck: give it the stuck declarations.
- `src/views/TrackEditorView.svelte`: `.edit-footer` (sticky bottom). Change `background: color-mix(… --bg-sheet 92% …)` and `backdrop-filter: blur(18px)` to the strip.
- **Reduced transparency** (the end of app.css): add these selectors to the `.topbar, .tl-head { background: var(--bg-1) }` rule and to the `backdrop-filter: none` list.

## 3. Cards → `glass-card`

**Pattern:** add `glass-card` in the markup, make the host `position: relative`, and delete its own `background` and `box-shadow`. For hover, use `background: var(--glass-sheen), var(--raise-2)` (see `.np-album:hover`).

- app.css `.top-result` (search): delete the `linear-gradient(… --tone-wash … --bg-2 …)` background. Keep its hover lift shadow, but restate it as `box-shadow: var(--lift-raised), 0 16px 40px -10px color-mix(in srgb, var(--tone-glow) 58%, transparent)` on hover. `.sk-top`: `var(--raise-1)` becomes nothing, because the card supplies the fill.
- `src/views/ArtistView.svelte` `.pick`: it has `isolation: isolate` and `filter` on hover. Check for `::before` or `::after` before adding the class; move any to the other pseudo. Change `filter: brightness(1.22)` on hover to the hover pattern above (a filter on a glass host re-rasters the rim).
- `src/views/TrackEditorView.svelte` `.repair-sheet`: delete `border`, `background` and `box-shadow`, and add `glass-card`.
- `src/components/PlaylistCleanup.svelte` `.cleanup-notice`: `glass-card`, with `.alert` keeping `background: var(--danger-wash)` layered as `background: var(--glass-sheen), var(--danger-wash)`.
- `src/App.svelte` `.error-banner`: add `glass-card` and `position: relative`. Background: `var(--glass-sheen), var(--danger-wash)`. Keep the text colour.

## 4. `--bg-*` → raise tokens (fields and small controls on glass)

These are one-for-one replacements with no design judgment involved:
- `src/views/SettingsView.svelte` `.set-check`: `background: var(--bg-2)` → `var(--raise-1)`. This is a duplicate of app.css `.set-check`, so check whether the scoped copy can be deleted outright.
- `src/views/TrackEditorView.svelte`: lines around 808 (`.repair-sheet`, covered in §3), 840 and 918 (`background: var(--bg-2)` → the pressed field, rule 6), and 843 (hover `var(--bg-3)` → `var(--raise-2)`).
- `src/components/TrackEditWaveform.svelte` 844 and 848: `color-mix(--bg-2 76%)` → `var(--raise-1)`; hover `var(--bg-3)` → `var(--raise-2)`.
- `src/views/HistoryView.svelte` `.hi-rail`: `background: var(--bg-4)` → `rgba(255,255,255,0.13)`, the same as `.rail`.
- app.css `.btn-accent:disabled` and `.play-btn:disabled`: `var(--bg-4)` → `var(--raise-2)`.
- app.css `.art` keeps `background: var(--bg-2)`. It is the placeholder of a tile that failed to load. Don't touch it: `.art.ready` already clears it once the picture exists.

## 5. Scrollbars still native-less (they have none right now)

`* { scrollbar-width: none }` is global, so these scrollers currently scroll with no indicator. Add `use:scrollbar` (from `src/lib/scrollbar.js`) to each, and make each one's parent positioned:
- `.credits-body`: **done**.
- `.cleanup-body`, `.cleanup-results`, `.cleanup-suggestions`: §1.
- `.p-saved-panel` scroller: §1.
- Any `overflow: auto` or `overflow-y: auto` you find with `grep -rn "overflow-y: auto\|overflow: auto" src`.

**Gotchas:**
- A scroller that is itself a popover must become the popover's CHILD (the TrackList pattern). Otherwise its bar, a sibling, is not in the top layer.
- Remove any `scrollbar-gutter` and any right padding that existed only to make room for the old bar.

## 6. `src/lib/glass.test.js` (write it; do not touch `haze.test.js`)

The model is exactly what `scratchpad/glass/calib.js` does (run it with `bun scratchpad/glass/calib.js 1.45 0.7 0.33 0.38 0.03` for reference):

- **Parse from app.css** with regexes:
  - text tokens `--fg`, `--fg-1`, `--fg-2`, `--fg-3` (hex)
  - `--tint-chrome`, `--tint-pane`, `--tint-strip`, `--tint-overlay`, `--tint-sheet` in the form `rgb(R G B / A)`
  - `--lift-card: rgba(255, 255, 255, A)`
  - the saturate and brightness numbers out of `--frost-plane`, `--frost-strip` and `--frost-overlay`
- **Import** `CEILING` and `luminance` from `./haze.js`.
- **Haze set:** every sRGB colour on a 32-step lattice with `luminance(...) <= CEILING.peak`.
- **Frost:** `saturate(s)` is the CSS matrix applied to encoded sRGB, `(0.213+0.787s, 0.715-0.715s, 0.072-0.072s / 0.213-0.213s, 0.715+0.285s, 0.072-0.072s / 0.213-0.213s, 0.715-0.715s, 0.072+0.928s)`, clamped. Then `brightness(b)` multiplies encoded values. Blur is ignored, because a uniform field is the worst case.
- **Composite:** `c = a·tint + (1−a)·frosted`. Then add the sheen, `c += (1−c)·0.035` (a white lift), and for a card add `c += (1−c)·lift`.

| Surface | Worst background | Measured with current tokens | Required |
|---|---|---|---|
| chrome | frost-plane(haze) + tint-chrome + sheen | fg 9.83, fg-1 6.31, fg-2 4.85, fg-3 3.23 | fg ≥ 7, fg-2 ≥ 4.5, fg-3 ≥ 3 |
| pane | frost-plane(haze) + tint-pane + sheen | fg 10.36, fg-2 5.11, fg-3 3.40 | same |
| card on pane | pane + lift-card | fg 9.73, fg-2 4.80, fg-3 3.20 | same |
| strip on pane | frost-strip(pane) + tint-strip | fg 14.62, fg-2 7.21, fg-3 4.80 | same |
| panel veil | `rgb(8 9 11)` at 0.56 over haze, no frost (the `--veil` in `.np-panel`) | fg 12.45, fg-2 6.14, fg-3 4.09 | same |
| card on veil | veil + lift-card + sheen | fg 10.22, fg-2 5.04, fg-3 3.36 | same |
| overlay (menu) | white → frost-overlay (brightness 0.7) → tint-overlay + sheen | fg 9.56, fg-1 6.14, fg-2 4.72, fg-3 3.14 | same |
| overlay, hovered item | above + white 0.08 | fg 7.51 | fg ≥ 7 only (a hovered item's text is `--fg`) |
| modal sheet | white under `--dim-modal` (0.5 of rgb(4 5 7)) → frost-overlay → tint-sheet + sheen | fg 9.50, fg-2 4.69, fg-3 3.12 | fg ≥ 7, fg-2 ≥ 4.5, fg-3 ≥ 3 |
| plate over the Canvas | a grey of the head's light L → frost-plate (brightness 0.72) → tint-plate at `a = 0.4 + 0.24·min(1, L/0.5)` (rounded to 0.02; NowPlayingPanel `plateAlpha`) | fg ≥ 8.18 for every L in 0…1 | fg ≥ 7 |

**Keep the tokens parseable:** every tint stays in `rgb(R G B / A)` form, and every frost stays `blur(Npx) saturate(S) brightness(B)`. If the planes move to the haze agent's pre-frosted layer, the planes' saturate and brightness move with it. Read them from wherever that layer takes them, and keep the same numbers.

## 7. Measured costs to keep in mind

**Current (pre-frosted planes, plates at 8px; isolated Edge 153, 1200×800, 240 Hz, Automatic Yes Canvas playing, GPU / renderer % of a core):** idle (playing row off-screen) 7.8 / 1.9; eq visible 22.4 / 7.6 (reduced-transparency baseline 19.2 / 6.4); continuous scroll 67.5 / 57.5 (the floor is about 65 whatever the glass does). Plate blur 8px vs 14px vs none: 22.4 vs 24.4 vs 23.4, which is within noise. Pacing is unchanged.

**Before the pre-frost (history):** (isolated Edge 153, 1200×800, 240 Hz, Canvas "Automatic Yes" playing, 15 s windows)

| Condition | GPU process | Renderer |
|---|---|---|
| Glass, as shipped | ≈ 50% of a core | ≈ 11% |
| `prefers-reduced-transparency` baseline | ≈ 10% | ≈ 8.5% |
| Plane blur off (colour-only frost) | ≈ 15.7% | |
| No plane frost at all | ≈ 14.3% | |
| Plate frost off | ≈ 44% (the four head plates cost about 8%) | |
| All frost off | ≈ 11.5% | |
| At rest with no Canvas (static haze) | ≈ 0% (0.02%) | ≈ 0.1% |

The cause is the eq bars and the video damage re-running the pane's blur every frame. The fix is the haze agent's pre-frosted layer (§0, rule 4).

**Frame pacing:**
- Scroll gesture over the track list: glass p50 4.2 ms, p95 8.4 ms, max 12.6 ms, 0 frames over 20 ms. The reduced baseline is identical.
- Panel morph (close and open): p95 4.3 ms, max 16.7 ms, 0 frames over 20 ms.
