<script>
  import { untrack } from "svelte";
  import {
    playback,
    api,
    togglePlay,
    navigate,
    route,
    positionMs,
    ui,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Slider from "./Slider.svelte";
  import { formatTime } from "../lib/time.js";
  import { listen } from "@tauri-apps/api/event";

  let dragPos = $state(null);

  function numberMs(value) {
    const ms = Number(value);
    return Number.isFinite(ms) ? Math.max(0, ms) : 0;
  }

  /**
   * Queue edit ranges stay in source coordinates. The player rail, however,
   * is the engine's compiled one-pass timeline: each cut collapses to one
   * seam and everything after it shifts left by the removed duration.
   */
  function makeEditTimeline(edit, originalValue, wireValue) {
    const originalDuration = numberMs(originalValue);
    const cuts = (edit?.cuts ?? [])
      .map((range) => ({
        start: Number(range?.start_ms),
        end: Number(range?.end_ms),
      }))
      .filter((range) => (
        Number.isFinite(range.start)
        && Number.isFinite(range.end)
        && range.start >= 0
        && range.end > range.start
        && range.end <= originalDuration
      ))
      .sort((left, right) => left.start - right.start || left.end - right.end)
      .reduce((valid, range) => {
        const previous = valid[valid.length - 1];
        if (!previous || range.start >= previous.end) valid.push(range);
        return valid;
      }, []);
    const removedDuration = cuts.reduce((total, cut) => total + cut.end - cut.start, 0);
    const onePassDuration = Math.max(0, originalDuration - removedDuration);
    const wireDuration = numberMs(wireValue);
    const compiledDuration = edit && wireDuration > 0 ? wireDuration : onePassDuration;

    const sourceToCompiled = (sourceValue) => {
      const source = Math.min(originalDuration, Math.max(0, numberMs(sourceValue)));
      let removedBefore = 0;
      for (const cut of cuts) {
        if (source <= cut.start) break;
        if (source < cut.end) return cut.start - removedBefore;
        removedBefore += cut.end - cut.start;
      }
      return source - removedBefore;
    };
    const percent = (value) => compiledDuration > 0
      ? Math.min(100, Math.max(0, (value / compiledDuration) * 100))
      : 0;

    const seamByPosition = new Map();
    for (const cut of cuts) {
      const position = sourceToCompiled(cut.start);
      const existing = seamByPosition.get(position);
      const sourceText = `${formatTime(cut.start)}–${formatTime(cut.end)}`;
      if (existing) {
        existing.sourceText += `, ${sourceText}`;
      } else {
        seamByPosition.set(position, {
          position,
          percent: percent(position),
          sourceText,
        });
      }
    }
    const seams = [...seamByPosition.values()].map((seam) => ({
      ...seam,
      title: `Cut seam at compiled ${formatTime(seam.position)}; removed source range ${seam.sourceText}.`,
    }));

    const rawLoop = edit?.loop_range;
    const loopStart = Number(rawLoop?.start_ms);
    const loopEnd = Number(rawLoop?.end_ms);
    const loopValid = (
      Number.isFinite(loopStart)
      && Number.isFinite(loopEnd)
      && loopStart >= 0
      && loopEnd > loopStart
      && loopEnd <= originalDuration
      && cuts.every((cut) => cut.end <= loopStart || loopEnd <= cut.start)
    );
    const loop = loopValid ? {
      start: sourceToCompiled(loopStart),
      end: sourceToCompiled(loopEnd),
      sourceStart: loopStart,
      sourceEnd: loopEnd,
    } : null;
    if (loop) {
      loop.startPercent = percent(loop.start);
      loop.endPercent = percent(loop.end);
      loop.widthPercent = Math.max(0, loop.endPercent - loop.startPercent);
      loop.title = `Loop span mapped to compiled ${formatTime(loop.start)}–${formatTime(
        loop.end,
      )}; source ${formatTime(loop.sourceStart)}–${formatTime(loop.sourceEnd)}.`;
    }

    const durationTitle = `Edited playback · compiled one-pass duration ${formatTime(
      compiledDuration,
    )}; original duration ${formatTime(originalDuration)}.`;
    const markerTitle = [
      durationTitle,
      seams.length
        ? `Cut seams at compiled ${seams.map((seam) => `${formatTime(seam.position)} (source ${seam.sourceText})`).join(", ")}.`
        : "",
      loop?.title ?? "",
    ].filter(Boolean).join(" ");

    return {
      originalDuration,
      onePassDuration,
      compiledDuration,
      seams,
      loop,
      markerTitle,
    };
  }

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null
  );

  const effectiveEdit = $derived(current?.effective_edit ?? null);
  const editTimeline = $derived.by(() => makeEditTimeline(
    effectiveEdit,
    current?.duration_ms,
    playback.duration_ms,
  ));
  const editIndicator = $derived.by(() => {
    if (!effectiveEdit) return null;
    return {
      label: "Edited",
      title: editTimeline.markerTitle,
    };
  });

  /**
   * Which of the user's own containers hold the playing track. One in-memory
   * IPC per track change — the index lives in the Rust side and is kept
   * fresh there by playlist fetches and a background reconciliation, so the
   * bar never polls and never blocks on the network.
   */
  let savedIn = $state([]);
  let savedSeq = 0;

  async function lookupSavedIn(uri) {
    const seq = ++savedSeq;
    if (!uri?.startsWith("spotify:track:")) {
      savedIn = [];
      return;
    }
    try {
      const refs = await api.getTrackPlaylists(uri);
      if (seq === savedSeq) savedIn = refs ?? [];
    } catch {
      // Backend not ready or gone: no mark is safer than a wrong mark.
      if (seq === savedSeq) savedIn = [];
    }
  }

  /* Track changes re-ask; index changes (an add while this track plays,
     an external like picked up by reconciliation) arrive as one event. */
  $effect(() => {
    lookupSavedIn(current?.uri);
  });
  $effect(() => {
    const event = listen("memberships_changed", () => lookupSavedIn(current?.uri));
    return () => event.then((off) => off()).catch(() => {});
  });

  /* Screen readers get the real container names, not a count. */
  const savedLabel = $derived(`Saved in ${savedIn.map((ref) => ref.name).join(", ")}`);

  function openSaved(id) {
    if (id === "liked") navigate("liked");
    else navigate("playlist", id);
  }


  const pos = $derived(dragPos !== null ? dragPos : positionMs());
  // `track` is the engine's name for repeat-one; anything outside
  // off/context/track is rejected outright by the Rust command layer.
  function cycleRepeat() {
    const order = ["off", "context", "track"];
    const next = order[(order.indexOf(playback.repeat) + 1) % order.length];
    api.setRepeat(next).catch(() => {});
  }

  const repeatTitle = $derived(
    playback.repeat === "track"
      ? "Repeat one"
      : playback.repeat === "context"
        ? "Repeat all"
        : "Repeat off"
  );

  /**
   * Speed is held in HUNDREDTHS here, not as a float. The engine arms its
   * pitch-preserving stretcher on `speed != 1.0` exactly, so the one value
   * that must survive the UI unharmed is 1 - and 100/100 is exactly 1.0 in
   * binary, where accumulating 0.05 steps would not be. It also makes the
   * step grid, the clamp and the equality checks plain integer work.
   */
  const SPEED_MIN = 50;
  const SPEED_MAX = 200;
  const SPEED_STEP = 5;
  const SPEED_PRESETS = [75, 100, 125, 150];

  let speedDraft = $state(null);
  let speedOpen = $state(false);
  let speedButton = $state(null);
  let speedMenu = $state(null);
  let speedAnchor = $state({ left: 0, bottom: 0 });
  let speedTimer = null;
  let pendingSpeed = null;
  let speedRequestActive = false;
  let speedIntent = 0;


  const speedPercent = $derived(
    speedDraft ?? Math.round((playback.playback_speed || 1) * 100)
  );
  const speedLabel = $derived(formatSpeed(speedPercent));

  function formatSpeed(percent) {
    return (percent / 100).toFixed(2).replace(/0$/, "");
  }

  /**
   * Wheel and slider input is first collapsed into one settled intent. If a
   * command is already in flight, only the newest settled value waits behind
   * it; no two speed commands overlap and intermediate values are discarded.
   */
  async function flushSpeed() {
    if (speedRequestActive) return;
    speedRequestActive = true;
    while (pendingSpeed) {
      const request = pendingSpeed;
      pendingSpeed = null;
      try {
        await api.setPlaybackSpeed(request.percent / 100);
      } catch {
        // The API wrapper rolls back only if this is still the active command.
      } finally {
        if (request.intent === speedIntent && speedDraft === request.percent) {
          speedDraft = null;
        }
      }
    }
    speedRequestActive = false;
  }

  function commitSpeed(percent) {
    const snapped = Math.round(percent / SPEED_STEP) * SPEED_STEP;
    const next = Math.min(SPEED_MAX, Math.max(SPEED_MIN, snapped));
    const intent = ++speedIntent;
    speedDraft = next;
    clearTimeout(speedTimer);
    speedTimer = setTimeout(() => {
      pendingSpeed = { percent: next, intent };
      flushSpeed();
    }, 120);
  }

  $effect(() => () => clearTimeout(speedTimer));

  function placeSpeedMenu() {
    const rect = speedButton?.getBoundingClientRect();
    if (!rect) return;
    speedAnchor = {
      left: rect.left + rect.width / 2,
      bottom: window.innerHeight - rect.top + 8,
    };
  }

  function toggleSpeedMenu() {
    speedOpen = !speedOpen;
    if (speedOpen) placeSpeedMenu();
  }

  /* Bound here rather than with onwheel so it can be non-passive: scrolling
     the control must adjust it, not scroll whatever sits behind it. */
  $effect(() => {
    const node = speedButton;
    if (!node) return;
    const onWheel = (event) => {
      event.preventDefault();
      const direction = event.deltaY < 0 ? 1 : -1;
      commitSpeed(speedPercent + direction * SPEED_STEP);
    };
    node.addEventListener("wheel", onWheel, { passive: false });
    return () => node.removeEventListener("wheel", onWheel);
  });

  $effect(() => {
    if (!speedOpen) return;
    const node = speedMenu;
    node?.showPopover?.();
    const reposition = () => placeSpeedMenu();
    window.addEventListener("resize", reposition);
    /* Light dismiss closes the popover without telling the component. */
    const onToggle = (event) => {
      if (event.newState === "closed") speedOpen = false;
    };
    node?.addEventListener("toggle", onToggle);
    return () => {
      window.removeEventListener("resize", reposition);
      node?.removeEventListener("toggle", onToggle);
      node?.hidePopover?.();
    };
  });
</script>

<footer class="player">
  <div class="p-now" class:idle={!current}>
    {#if current}
      <button
        class="p-art-btn"
        title="Go to album"
        onclick={() => current.album_id && navigate("album", current.album_id)}
      >
        <Cover
          src={current.cover_url}
          id={current.album_id || current.uri}
          name={current.album_name || current.name}
          size={48}
          class="p-art"
        />
      </button>
      <span class="p-meta">
        <span class="p-title-line">
          {#if current.album_id}
            <button
              class="p-title"
              title="Go to album"
              onclick={() => navigate("album", current.album_id)}
            >{current.name}</button>
          {:else}
            <span class="p-title">{current.name}</span>
          {/if}
          {#if savedIn.length}
            <span class="p-saved">
              <span class="p-saved-mark"><Icon name="check" size={12} /></span>
              <!-- Keyboard path: tabbing into a row button opens the panel
                   through :focus-within, so the wrapper stays non-focusable
                   and every interactive target remains a real button. -->
              <span class="p-saved-panel" role="group" aria-label={savedLabel}>
                <span class="p-saved-head">Saved in</span>
                {#each savedIn as ref (ref.id)}
                  <button
                    class="p-saved-row"
                    title="Open {ref.name}"
                    onclick={() => openSaved(ref.id)}
                  >{ref.name}</button>
                {/each}
              </span>
            </span>
          {/if}
          {#if editIndicator}
            <span
              class="p-edit-indicator"
              title={editIndicator.title}
              aria-label={editIndicator.title}
            >
              <span class="p-edit-mark" aria-hidden="true"></span>
              {editIndicator.label}
            </span>
          {/if}
        </span>
        <ArtistLinks
          class="p-artists"
          names={current.artist_names}
          ids={current.artist_ids ?? []}
          id={current.artist_id}
        />
      </span>
    {:else}
      <!-- Idle holds the same 48px slot, so the bar does not jump the moment
           the first track lands. -->
      <span class="art p-art" aria-hidden="true"></span>
      <span class="p-meta">
        <span class="p-title">Nothing playing</span>
        <span class="p-artists">Pick something from your library</span>
      </span>
    {/if}
  </div>

  <div class="p-center">
    <div class="transport">
      <button
        class="ctl"
        class:on={playback.shuffle}
        title={playback.shuffle ? "Disable shuffle" : "Enable shuffle"}
        onclick={() => api.setShuffle(!playback.shuffle).catch(() => {})}
      >
        <Icon name="shuffle" size={17} />
      </button>
      <button class="ctl" title="Previous" onclick={() => api.previous().catch(() => {})}>
        <Icon name="previous" size={19} />
      </button>
      <button
        class="play-btn"
        title={playback.playing ? "Pause" : "Play"}
        onclick={togglePlay}
        disabled={!playback.queue.length}
      >
        <Icon name={playback.playing ? "pause" : "play"} size={16} />
      </button>
      <button class="ctl" title="Next" onclick={() => api.next().catch(() => {})}>
        <Icon name="next" size={19} />
      </button>
      <button class="ctl" class:on={playback.repeat !== "off"} title={repeatTitle} onclick={cycleRepeat}>
        <Icon name={playback.repeat === "track" ? "repeat-one" : "repeat"} size={17} />
      </button>
    </div>

    <div class="p-seek">
      <span class="p-time l">{formatTime(pos)}</span>
      <div
        class="p-seek-slider"
        title={effectiveEdit ? editTimeline.markerTitle : undefined}
      >
        <Slider
          min={0}
          max={playback.duration_ms || 0}
          value={positionMs()}
          label="Seek"
          step={5000}
          formatValue={formatTime}
          onCommit={(v) => {
            dragPos = null;
            api.seek(v).catch(() => {});
          }}
          onDragStart={(v) => (dragPos = v)}
          onDragChange={(v) => (dragPos = v)}
        />
        {#if effectiveEdit}
          <span class="p-seek-markers" aria-hidden="true">
            {#if editTimeline.loop && editTimeline.loop.widthPercent > 0}
              <span
                class="p-loop-band"
                style:left="{editTimeline.loop.startPercent}%"
                style:width="{editTimeline.loop.widthPercent}%"
                title={editTimeline.loop.title}
              ></span>
            {/if}
            {#each editTimeline.seams as seam (seam.percent)}
              <span
                class="p-cut-seam"
                style:left="{seam.percent}%"
                title={seam.title}
              ></span>
            {/each}
          </span>
        {/if}
      </div>
      <span class="p-time r">{formatTime(playback.duration_ms)}</span>
    </div>
  </div>

  <div class="p-right">
    <button
      class="p-speed"
      class:on={speedPercent !== 100}
      bind:this={speedButton}
      title="Playback speed — scroll to adjust, double-click to reset"
      aria-label="Playback speed"
      aria-expanded={speedOpen}
      onclick={toggleSpeedMenu}
      ondblclick={() => commitSpeed(100)}
    >
      {speedLabel}×
    </button>
    <div
      class="speed-menu"
      popover="auto"
      bind:this={speedMenu}
      style:left="{speedAnchor.left}px"
      style:bottom="{speedAnchor.bottom}px"
    >
      <div class="speed-head">
        <span class="speed-value">{speedLabel}×</span>
        <span class="speed-note">pitch preserved</span>
      </div>
      <Slider
        min={SPEED_MIN}
        max={SPEED_MAX}
        value={speedPercent}
        label="Playback speed"
        step={SPEED_STEP}
        kind="speed"
        formatValue={(v) => formatSpeed(v) + "×"}
        onDragStart={(v) => (speedDraft = v)}
        onDragChange={(v) => (speedDraft = v)}
        onCommit={(v) => commitSpeed(v)}
      />
      <div class="speed-presets">
        {#each SPEED_PRESETS as preset}
          <button
            class="speed-preset"
            class:on={speedPercent === preset}
            onclick={() => commitSpeed(preset)}
          >
            {formatSpeed(preset)}×
          </button>
        {/each}
      </div>
    </div>
    <button
      class="btn-icon"
      class:on={ui.nowPlayingOpen}
      title="Now playing details"
      onclick={() => (ui.nowPlayingOpen = !ui.nowPlayingOpen)}
    >
      <Icon name="panel" size={18} />
    </button>
    <button
      class="btn-icon"
      class:on={route.name === "queue"}
      title="Queue"
      onclick={() => navigate(route.name === "queue" ? "library" : "queue")}
    >
      <Icon name="queue" size={18} />
    </button>
    <button class="btn-icon" title="Mute" onclick={() => api.setVolume(playback.volume ? 0 : 70).catch(() => {})}>
      <Icon name="volume" size={18} />
    </button>
    <Slider
      min={0}
      max={100}
      value={playback.volume}
      label="Volume"
      step={5}
      kind="vol"
      onCommit={(v) => api.setVolume(v).catch(() => {})}
    />
  </div>
</footer>

<style>
  .p-speed {
    min-width: 46px;
    height: 28px;
    padding: 0 var(--s2);
    border: 1px solid var(--line-2);
    border-radius: var(--rf);
    background: var(--bg-2);
    color: var(--fg-2);
    font: inherit;
    font-size: var(--t-12);
    font-variant-numeric: tabular-nums;
    cursor: pointer;
    transition:
      color var(--d1) var(--ease),
      border-color var(--d1) var(--ease);
  }
  .p-speed:hover {
    color: var(--fg-1);
    border-color: var(--line-2);
  }
  .p-speed.on {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  }

  /* Top layer, so the panel is never clipped by the player bar's own
     stacking context - the same escape the row menu uses. */
  .speed-menu {
    position: fixed;
    inset: auto auto auto 0;
    transform: translateX(-50%);
    margin: 0;
    padding: var(--s4);
    width: 232px;
    border: 1px solid var(--line-2);
    border-radius: var(--r2);
    background: var(--bg-2);
    box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.85), 0 2px 6px rgba(0, 0, 0, 0.5);
    color: var(--fg-1);
  }
  .p-title-line {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    min-width: 0;
  }
  .p-title-line .p-title {
    min-width: 0;
    flex: 1;
  }
  .p-edit-indicator {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 2px 5px;
    border: 1px solid color-mix(in srgb, var(--gold) 34%, transparent);
    border-radius: var(--rf);
    background: color-mix(in srgb, var(--gold) 7%, transparent);
    color: color-mix(in srgb, var(--gold) 78%, var(--fg-1));
    font-family: var(--font-small);
    font-size: 10px;
    font-weight: var(--w-semi);
    letter-spacing: 0.04em;
    line-height: 1.15;
    white-space: nowrap;
  }
  .p-edit-mark {
    width: 5px;
    height: 5px;
    flex: none;
    border: 1px solid currentColor;
    border-radius: 50%;
  }

  /* Saved-in mark: a quiet foam check that opens the list of the user's
     containers holding this track. The panel borrows the speed-menu's
     raised surface; hover or keyboard focus opens it, no JS positioning. */
  .p-saved {
    position: relative;
    display: inline-flex;
    flex: none;
    align-items: center;
    align-self: center;
    cursor: default;
    outline: none;
  }
  .p-saved-mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: var(--rf);
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    transition:
      background var(--d1) var(--ease),
      box-shadow var(--d1) var(--ease);
  }
  .p-saved:hover .p-saved-mark,
  .p-saved:focus-visible .p-saved-mark {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .p-saved-panel {
    position: absolute;
    bottom: calc(100% + 8px);
    left: -8px;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 200px;
    max-width: 280px;
    max-height: 232px;
    overflow-y: auto;
    padding: var(--s3);
    border: 1px solid var(--line-2);
    border-radius: var(--r2);
    background: var(--bg-2);
    box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.85), 0 2px 6px rgba(0, 0, 0, 0.5);
    opacity: 0;
    visibility: hidden;
    transform: translateY(4px);
    pointer-events: none;
    transition:
      opacity var(--d1) var(--ease),
      transform var(--d1) var(--ease),
      visibility var(--d1) var(--ease);
  }
  /* Invisible bridge across the gap, so the pointer can travel from the
     mark into the panel without the hover chain breaking mid-way. */
  .p-saved-panel::before {
    content: "";
    position: absolute;
    top: -8px;
    left: 0;
    right: 0;
    height: 8px;
  }
  .p-saved:hover .p-saved-panel,
  .p-saved:focus-within .p-saved-panel,
  .p-saved:focus-visible .p-saved-panel {
    opacity: 1;
    visibility: visible;
    transform: translateY(0);
    pointer-events: auto;
  }
  .p-saved-head {
    margin-bottom: var(--s2);
    color: var(--fg-3);
    font-family: var(--font-small);
    font-size: 10px;
    font-weight: var(--w-semi);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .p-saved-row {
    overflow: hidden;
    padding: 3px 6px;
    margin: 0 -6px;
    border: 0;
    border-radius: var(--r1);
    background: none;
    color: var(--fg-1);
    font: inherit;
    font-size: var(--t-12);
    line-height: 1.35;
    text-align: left;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    transition:
      color var(--d1) var(--ease),
      background var(--d1) var(--ease);
  }
  .p-saved-row:hover {
    background: var(--bg-3);
  }
  .p-seek-slider {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
    height: 12px;
  }
  .p-seek-markers {
    position: absolute;
    inset: 0;
    z-index: 2;
    pointer-events: none;
  }
  .p-loop-band {
    position: absolute;
    top: 50%;
    height: 8px;
    transform: translateY(-50%);
    border: 1px solid color-mix(in srgb, var(--gold) 48%, transparent);
    border-radius: var(--rf);
    background: color-mix(in srgb, var(--gold) 20%, transparent);
  }
  .p-cut-seam {
    position: absolute;
    top: 50%;
    width: 1px;
    height: 12px;
    transform: translate(-50%, -50%);
    background: color-mix(in srgb, var(--gold) 82%, var(--fg));
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--bg-0) 75%, transparent);
  }
  .speed-menu:not(:popover-open) {
    display: none;
  }
  .speed-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s3);
    margin-bottom: var(--s3);
  }
  .speed-value {
    font-size: var(--t-15);
    font-variant-numeric: tabular-nums;
  }
  .speed-note {
    font-size: var(--t-11);
    color: var(--fg-3);
  }
  .speed-presets {
    display: flex;
    gap: var(--s2);
    margin-top: var(--s3);
  }
  .speed-preset {
    flex: 1;
    height: 24px;
    border: 1px solid var(--line-2);
    border-radius: var(--rf);
    background: none;
    color: var(--fg-2);
    font: inherit;
    font-size: var(--t-11);
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  .speed-preset:hover {
    color: var(--fg-1);
    border-color: var(--line-2);
  }
  .speed-preset.on {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  }
</style>
