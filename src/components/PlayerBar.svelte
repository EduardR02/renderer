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
        <span class="p-title">{current.name}</span>
        <ArtistLinks
          class="p-artists"
          names={current.artist_names}
          ids={current.artist_ids ?? []}
          id={current.artist_id}
        />
      </span>
      <button
        class="btn-icon"
        class:on={isLiked}
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
