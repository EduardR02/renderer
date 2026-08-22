<script>
  import {
    playback,
    api,
    togglePlay,
    toggleLiked,
    isTrackLiked,
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

  let dragPos = $state(null);

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null
  );
  const pos = $derived(dragPos !== null ? dragPos : positionMs());
  const isLiked = $derived(current ? isTrackLiked(current.uri) : false);

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

  const speedPercent = $derived(
    speedDraft ?? Math.round((playback.playback_speed || 1) * 100)
  );
  const speedLabel = $derived(formatSpeed(speedPercent));

  function formatSpeed(percent) {
    return (percent / 100).toFixed(2).replace(/0$/, "");
  }

  /**
   * Held briefly rather than sent per notch. A wheel sweep across 1x would
   * otherwise arm and disarm the engine's pipeline repeatedly, and each of
   * those transitions legitimately discards the queued audio - so an
   * un-debounced sweep would stutter the output on the way past 1x.
   */
  function commitSpeed(percent) {
    const snapped = Math.round(percent / SPEED_STEP) * SPEED_STEP;
    const next = Math.min(SPEED_MAX, Math.max(SPEED_MIN, snapped));
    speedDraft = next;
    clearTimeout(speedTimer);
    speedTimer = setTimeout(() => {
      api
        .setPlaybackSpeed(next / 100)
        .catch(() => {})
        .finally(() => {
          if (speedDraft === next) speedDraft = null;
        });
    }, 120);
  }

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
        {#if current.album_id}
          <button
            class="p-title"
            title="Go to album"
            onclick={() => navigate("album", current.album_id)}
          >{current.name}</button>
        {:else}
          <span class="p-title">{current.name}</span>
        {/if}
        <ArtistLinks
          class="p-artists"
          names={current.artist_names}
          ids={current.artist_ids ?? []}
          id={current.artist_id}
        />
      </span>
      <button
        class="btn-icon"
        class:saved={isLiked}
        title={isLiked ? "Remove from Liked Songs" : "Save to Liked Songs"}
        onclick={() => toggleLiked(current.uri)}
      >
        <Icon name={isLiked ? "heart-filled" : "heart"} size={16} />
      </button>
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
