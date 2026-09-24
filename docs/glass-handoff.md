# Glass redesign: handoff

A temporary working note for the QA pass. **Delete this file in the commit that finishes the redesign.**

## Where it stands
The app is glass over an ambient haze taken from the now-playing Canvas or cover. The first pass is commit `64b9b83` and the second is `929f828`; read their messages. The implementation is now complete: what is left is QA.

The third pass (uncommitted when this was written) finished the implementation:
- **Wiring:** glass on the remaining cards, strips and dialogs; the `--bg-*` to raise-token migration; overlay scrollbars everywhere (every scroller in `src` now wears `use:scrollbar`).
- **Active rail rows:** they now hold their contrast over the brightest haze.
- **Overlays:** recalibrated so their tone pools hold contrast too.
- **Perf trim:** the panel's corners are clipped on the layers that paint in it, not on the panel.
- **Page-less popovers:** the root `--tone-*` follows the haze.
- **Docs:** the headers in `ambient.svelte.js` and `Ambient.svelte` are current.

`bun test` passes all 63 tests, and `bun run build` passes with no warnings.

## What the owner wants (don't re-litigate these)
- **The reference:** their original AI concept, a dark UI that is richly colourful and lit by the record.
- **Haze motion:** still at rest; a fast (~1 s) crossfade on track change; zero work at rest.
- **Over the video:** no scrims; the text floats on it with a glyph shadow.
- **One panel mode:** cover and Canvas share one layout.
- **Scrollbars:** overlay, shown on hover.
- **Performance:** it matters as much as looks. Measure it; don't assume.
- **The background is "A light":** a 6px haze lens and a 12px frost.
- **The seam spec:** the Canvas is never obstructed or widened. Its left corners are rounded (18px). The pane and the bar sit left of it with a normal gap, rounded corners and a glass rim.

## For QA
1. **The seam on bright Canvases.** It is built and unit-tested but has never been checked by eye on a bright video. Shoot Spectrum and Nothing Can Touch Us (the jellyfish) at 1600×900: crop about x 1040–1140 around the video's left edge, at the top and the bottom corners. If the gap reads too bright or too dark, tune `SEAM`, `SEAM_IN` and `SEAM_DIM` in `haze.js`.
2. **Corner anti-aliasing in the real WebView2 at 150%.** The pane and bar corners, and the video's rounded left corners, looked smooth in Edge at DPR 1 and 1.5. Confirm it in the real app.
   - The video's corners are now cut by `.np-stage` rather than `.np-panel`. That is the perf trim below, so check this item after it.
   - If the planes' frost still shows jaggies: in `Ambient.svelte`, move the `.frost` clip-path to a full-window wrapper `div` around the frost canvas, and make sure the wrapper doesn't get `will-change`.
3. **Perf regression: is it the haze or the glass?** With a Canvas playing and the playing row visible, the absolute GPU numbers were above the old baseline.
   - **Baseline** (GPU % of a core, isolated Edge, 1200×800, Automatic Yes Canvas playing): idle 7.8, eq visible 22.4, scroll 67.5.
   - **How:** A/B against a `prefers-reduced-transparency: reduce` baseline, and against injected CSS. Don't swap HEAD files into the tree; the page comes up unstyled.
   - **At rest:** there must be zero haze work. The `window.__haze` counters must stay flat.
   - **The stage-clip trim:** the layout agent measured it at about −0.4 GPU with CSS injection. Confirm it.
   - **Two new live backdrop-filters:** the drag pill while a row is dragged, and the editor's sticky footer. Both are small.
4. **A visual pass over every view and popup**, at 1600×900 and in a narrow window, with the panel open and closed. Especially:
   - **Menus:** the overlay's top-left glow went from 20% to 10% (below). Check that menus still read as lit by the record, not as the old flat grey. If they read plain, raise the colour without raising luminance, for example with a darker, equally chromatic stop.
   - **Selected rail rows** (nav items, playlists, Liked Songs): they now sit on a denser base (below). Over a bright haze they should read as a slightly deeper pocket, not a hole.
   - **Search Top Result and the artist page's pick:** they are glass cards that keep the record's tone as a wash pooled from the top left. The checklist had said to drop the tone.
   - **Track editor:** the card, the pressed time fields, the waveform well, and the strip footer.
   - **History and Discography:** the stuck strips.
   - **The drag pill** (overlay glass).
   - **Settings:** the checkbox. The scoped 16px copy is gone, so it is now the global 18px one.
   - **The error banner.**
   - **Page-less popovers:** the Queue, History, Settings and Search menus and selects. They should now carry the haze's colour.
5. **Noticed, not fixed:**
   - **The panel head's shadow:** the `.np-head.over` shadow (`0 12px 20px -8px var(--veil)`) is no longer clipped by the panel, so about 2px of it may reach into the gutter under the head. It is faint and in the veil's own colour. Check it.
   - **Waveform chips:** `.wave-status`, `.hover-time` and the preview playhead's dot are still near-opaque `--bg-1` over the (now translucent) waveform well.
   - **Placeholders:** `.art`, `.art.pending` and the lightbox's placeholder gradient keep `--bg-2`/`--bg-3` on purpose.
   - **The harness Zedd-Radio header shows no title.** It looks like a data issue.
   - **A new root tone** means one full-document style recalc per change. It is written only from `publishVeil`, on a real change.

## What the third pass changed, and why
- **Active rail rows** (`.nav-item.active`, `.lib-row.active`, `.lib-row.liked-row.active`):
  - **The problem:** the raise and the accent wash are light, and on the thinnest glass over the brightest haze that light pushed `--fg-2` to 3.54:1.
  - **The fix:** they are now denser glass. `--tint-active: rgb(8 9 12 / 0.55)` sits under the raise and the wash. Over a dark haze it changes almost nothing (Y 0.037 → 0.035). At worst it holds `--fg-2` at 4.71 and `--fg-3` at 3.14 under foam, and 4.61 and 3.07 on the Liked Songs row.
  - In reduced transparency `--tint-active` is `transparent`.
- **Overlays** (`.glass-overlay`):
  - **The problem:** the new test models the tone pools where they are strongest, at the corners, over a white cover with the most luminous tone a record can produce (a teal glow). There the old values failed: `--fg-2` 3.66, and `--fg` on a lit item 5.97.
  - **The new values:** `--tint-overlay` goes 0.74 → 0.76, the glow pool 20% → 10%, and the sheen is the shared `--glass-sheen` (0.035, down from 0.055). The wash pool (36%, bottom right) is unchanged and carries most of the colour.
  - **Worst cases now:** glow corner `--fg-2` 4.56, `--fg-3` 3.04, lit `--fg` 7.28; wash corner 4.87, 3.25 and 7.79.
- **`glass.test.js`** gained three tests. All of them read the tokens and the rule bodies out of `app.css`:
  - the active rail rows;
  - overlays at the centre and in both tone pools, plain and lit;
  - the modal sheet.
- **The panel's corners:** `.np-panel` lost its `overflow` and `border-radius`, and `--np-corners` now carries the shape. `.np-stage` clips (radius + `overflow: hidden`), `.np-scroll` is rounded (it already clips as a scroller), and `.np-head` has a top-left radius so its veil has no square corner.
- **The root tone:**
  - `publishVeil` (`ambient.svelte.js`) now also turns the veil into a tone with covertone's new `toneOfColor`: the veil's hue, with chroma read relative to its lightness, at the palette's fixed lightnesses. Because the lightnesses are fixed, the calibration above holds.
  - It writes `--tone-wash`, `--tone-wash-deep` and `--tone-glow` on `<html>`, and only when the tone string changes.
  - Pages, the panel and the bar still set their own tones, and theirs win.
- **Wiring:**
  - **Strips on the tokens:** the History block, the Discography controls and the editor footer use `--tint-strip`/`--frost-strip`. They need no reduced-transparency rules, because the tokens go opaque there.
  - **Glass cards:** `.repair-sheet`, `.top-result` (with `.sk-top`), `.pick` (hover adds light; the filter is gone) and `.error-banner`.
  - **Overlay glass:** the drag pill (`dnd.svelte.js` sets the class).
  - **Raises:** `.btn-accent:disabled`, `.play-btn:disabled` and `.seg-btn.on` use `--raise-2`. `.hi-rail` is white 0.13. Waveform tool buttons are `--raise-1`/`--raise-2`. The editor's rows and enable-row are `--raise-1`, with the region washes over it.
  - **Pressed fields:** the editor's `.time-input`, the waveform well and the selection readout.
  - **One exception:** `.preview-pause` is `--raise-1`, not a pressed field, because it is a button.
  - **Settings:** the scoped duplicate of `.set-check` is deleted.

## How to verify
- **Dev harness:** start the dev server with `bun run dev`, or the "ui-preview" entry in `.claude/launch.json`. Open `http://127.0.0.1:1420/dev/ui-harness.html?real`, which serves the owner's real library, covers and Canvases from `dev/.real-cache` (see `dev/real-bridge.js`). For live reads, quit the app and run `bun dev/real-app.js`.
- **Test tracks:**
  - Automatic Yes, Inside Out and Lost In Japan (cover only) are in "Zedd - Radio": `__harness.navigate('playlist','5XtFXqFoTwqe4PQWSVbOyY')`.
  - Nothing Can Touch Us is in "headphone demo".
  - Spectrum: wrap `__TAURI_INTERNALS__.invoke` so that `browse_canvas` returns `https://canvaz.scdn.co/upload/licensor/7JGwF0zhX9oItt9901OvB5/video/5171946a962b4154adf91a58092e5c1d.cnvs.mp4`.
  - The track editor: open a row's "…" menu and choose "Edit playback…". (`__harness.openEditor()` only finds fixture tracks.)
- **Screenshots and perf:** use an isolated Microsoft Edge driven over CDP. It is the same engine as WebView2.
  - Launch it with `--remote-debugging-port=<port> --user-data-dir=<temp dir> --disable-features=CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows`. Without the last two flags an occluded window reads about 1% GPU.
  - Use `Emulation.setFocusEmulationEnabled`, or the page blurs and the Canvas and the eq pause.
  - A CDP screenshot with `scale > 1` re-rasters, which hides aliasing. For pixel-true zooms, upscale a scale-1 shot.
- **The harness reloads:** Vite HMR full-reloads it on edits to the haze files, and `?real` then reboots to the owner's saved track. Scripts must set up their scene every time.

## The material, in short (for fixes)
- **Tokens and classes** live in `app.css`: GLASS (in `:root`) and THE MATERIAL.
  - `.glass-chrome` / `.glass-pane` are the planes. They have no backdrop-filter and must also wear `use:frost`.
  - `.glass-card` adds light on glass.
  - `.glass-overlay` is for menus, popovers and dialogs. It becomes a sheet when `:modal`.
  - `.glass-plate` is a control over the Canvas.
  - `.glass-strip` is a sticky bar: frost and tint, no rim.
- **A host of a rimmed class must be positioned,** and its `::before` belongs to the rim. Never put `position` in the material classes; it would beat the UA's `dialog:modal`.
- **On glass:** hovered or selected items use white 0.08 (overlays) or `--raise-2`, and their text goes to `--fg`. Fields are pressed in: `rgb(0 0 0 / 0.22)` with a `rgba(255,255,255,0.08)` border.
- **A scroller that is a popover** must become the popover's child (the TrackList menus pattern), so that its overlay bar is in the top layer.
- **Keep tokens parseable,** because `glass.test.js` reads them: tints stay `rgb(R G B / A)`, and frosts stay `blur() saturate() brightness()`.
