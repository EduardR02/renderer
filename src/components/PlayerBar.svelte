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
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import Slider from "./Slider.svelte";
  import { formatTime } from "../lib/time.js";

  let dragging = $state(false);
  let dragPos = $state(null);

  const current = $derived(
    playback.current_index >= 0 ? playback.queue[playback.current_index] ?? null : null
  );
  const pos = $derived(dragging && dragPos !== null ? dragPos : positionMs());
  const isLiked = $derived(current ? isTrackLiked(current.uri) : false);

  function onSeekCommit(v) {
    api.seek(v).catch(() => {});
  }

  function onVolumeCommit(v) {
    api.setVolume(v).catch(() => {});
  }

  // `track` is the engine's name for repeat-one; anything outside
  // off/context/track is rejected outright by the Rust command layer.
  function cycleRepeat() {
    const order = ["off", "context", "track"];
    const i = order.indexOf(playback.repeat);
    const next = order[(i + 1) % order.length];
    api.setRepeat(next).catch(() => {});
  }

  const repeatTitle = $derived(
    playback.repeat === "track"
      ? "Repeat one"
      : playback.repeat === "context"
        ? "Repeat all"
        : "Repeat off"
  );
</script>

<footer class="player">
  <div class="p-left">
    {#if current}
      <button class="p-cover" title="Go to album" onclick={() => current.album_id && navigate("album", current.album_id)}>
        <Cover src={current.cover_url} alt={current.name} style="width:56px;height:56px" iconSize={22} rounded={4} />
      </button>
      <div class="p-meta">
        <span class="p-title">{current.name}</span>
        <button
          class="p-artists"
          title="Go to artist"
          onclick={() => current.artist_id && navigate("artist", current.artist_id)}
        >
          {current.artist_names.join(", ")}
        </button>
      </div>
      <button
        class="icon-btn like-btn"
        class:liked={isLiked}
        title={isLiked ? "Remove from Liked Songs" : "Save to Liked Songs"}
        onclick={() => toggleLiked(current.uri)}
      >
        <Icon name={isLiked ? "heart-filled" : "heart"} size={16} />
      </button>
    {/if}
  </div>

  <div class="p-center">
    <div class="p-controls">
      <button
        class="ctl"
        class:on={playback.shuffle}
        title={playback.shuffle ? "Disable shuffle" : "Enable shuffle"}
        onclick={() => api.setShuffle(!playback.shuffle).catch(() => {})}
      >
        <Icon name="shuffle" size={17} />
      </button>
      <button class="ctl" title="Previous" onclick={() => api.previous().catch(() => {})}>
        <Icon name="previous" size={21} />
      </button>
      <button
        class="play-btn"
        title={playback.playing ? "Pause" : "Play"}
        onclick={togglePlay}
        disabled={!playback.queue.length}
      >
        <Icon name={playback.playing ? "pause" : "play"} size={19} />
      </button>
      <button class="ctl" title="Next" onclick={() => api.next().catch(() => {})}>
        <Icon name="next" size={21} />
      </button>
      <button class="ctl" class:on={playback.repeat !== "off"} title={repeatTitle} onclick={cycleRepeat}>
        <Icon name={playback.repeat === "track" ? "repeat-one" : "repeat"} size={17} />
      </button>
    </div>

    <div class="p-seek">
      <span class="time">{formatTime(pos)}</span>
      <div class="seek-wrap">
        <Slider
          min={0}
          max={playback.duration_ms || 0}
          value={positionMs()}
          label="Seek"
          step={5000}
          onCommit={(v) => {
            dragging = false;
            dragPos = null;
            onSeekCommit(v);
          }}
          onDragStart={(v) => {
            dragging = true;
            dragPos = v;
          }}
          onDragChange={(v) => {
            dragPos = v;
          }}
        />
      </div>
      <span class="time">{formatTime(playback.duration_ms)}</span>
    </div>
  </div>

  <div class="p-right">
    <button
      class="ctl"
      class:on={route.name === "queue"}
      title="Queue"
      onclick={() => navigate(route.name === "queue" ? "library" : "queue")}
    >
      <Icon name="queue" size={17} />
    </button>
    <div class="vol-wrap">
      <Slider min={0} max={100} value={playback.volume} label="Volume" step={5} onCommit={onVolumeCommit} />
    </div>
  </div>
</footer>

<style>
  .player {
    display: grid;
    grid-template-columns: minmax(0, 30%) minmax(0, 40%) minmax(0, 30%);
    align-items: center;
    gap: var(--space-4);
    height: var(--player-height);
    padding: 0 var(--space-4);
    background: var(--bg-sidebar);
    flex: none;
  }
  .p-left {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
  }
  .p-cover {
    flex: none;
    border-radius: var(--radius-sm);
    line-height: 0;
  }
  .p-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 2px;
  }
  .p-title {
    font-size: var(--font-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .p-artists {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    transition: color var(--transition-fast);
  }
  .p-artists:hover {
    color: var(--text-primary);
    text-decoration: underline;
  }
  .like-btn {
    flex: none;
  }
  .p-center {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .p-controls {
    display: flex;
    align-items: center;
    gap: var(--space-5);
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--text-secondary);
    transition: color var(--transition-fast), transform var(--transition-fast);
  }
  .ctl:hover {
    color: var(--text-primary);
  }
  .ctl:active {
    transform: scale(0.94);
  }
  .ctl.on {
    color: var(--accent);
  }
  .ctl.on:hover {
    color: var(--accent-hover);
  }
  .play-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    border-radius: var(--radius-full);
    background: var(--text-primary);
    color: #000;
    transition: transform var(--transition-fast), background-color var(--transition-fast);
  }
  .play-btn:hover:not(:disabled) {
    transform: scale(1.05);
    background: var(--accent);
  }
  .play-btn:active:not(:disabled) {
    transform: scale(1);
  }
  .p-seek {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-width: 0;
  }
  .p-seek .seek-wrap {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .time {
    flex: none;
    min-width: 40px;
    font-size: var(--font-xs);
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
    text-align: center;
  }
  .p-right {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    min-width: 0;
  }
  .p-right .vol-wrap {
    width: 100px;
    display: flex;
  }
</style>
