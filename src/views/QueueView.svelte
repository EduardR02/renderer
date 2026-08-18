<script>
  import { playback, api } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTime } from "../lib/time.js";

  function playAt(i) {
    api.playQueue(playback.queue, i).catch(() => {});
  }
</script>

<section class="page">
  <header class="page-head">
    <h1>Queue</h1>
    <p class="sub">
      {playback.queue.length} song{playback.queue.length === 1 ? "" : "s"} in queue
      {playback.current_index >= 0 ? ` · now playing #${playback.current_index + 1}` : ""}
    </p>
  </header>

  {#if playback.queue.length}
    <div class="queue-list">
      {#each playback.queue as track, i}
        <div class="q-row" class:current={i === playback.current_index}>
          <button class="q-play" title="Play from here" onclick={() => playAt(i)}>
            {#if i === playback.current_index}
              <Icon name={playback.playing ? "pause" : "play"} size={16} />
            {:else}
              <Icon name="play" size={16} />
            {/if}
          </button>
          <Cover src={track.cover_url} alt={track.name} style="width:40px;height:40px" iconSize={16} rounded={4} />
          <div class="q-meta">
            <span class="q-name" class:current-name={i === playback.current_index}>{track.name}</span>
            <span class="q-artists">{track.artist_names.join(", ")}</span>
          </div>
          <span class="q-time">{formatTime(track.duration_ms)}</span>
          <div class="q-actions">
            <button class="icon-btn" title="Move up" disabled={i === 0} onclick={() => api.moveQueue(i, i - 1).catch(() => {})}>
              <Icon name="chevron-up" size={16} />
            </button>
            <button class="icon-btn" title="Move down" disabled={i === playback.queue.length - 1} onclick={() => api.moveQueue(i, i + 1).catch(() => {})}>
              <Icon name="chevron-down" size={16} />
            </button>
            <button class="icon-btn" title="Remove from queue" onclick={() => api.removeQueue(i).catch(() => {})}>
              <Icon name="x" size={16} />
            </button>
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="empty">
      <Icon name="queue" size={40} />
      <p>The queue is empty.</p>
      <p class="sub">Play something or add a track to the queue.</p>
    </div>
  {/if}
</section>

<style>
  .queue-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .q-row {
    display: grid;
    grid-template-columns: 32px 40px minmax(0, 1fr) auto auto;
    align-items: center;
    gap: var(--space-3);
    height: 56px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-sm);
    transition: background-color var(--transition-fast);
  }
  .q-row:hover {
    background: var(--bg-highlight);
  }
  .q-play {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    color: var(--text-secondary);
    transition: color var(--transition-fast);
  }
  .q-play:hover {
    color: var(--text-primary);
  }
  .q-row.current .q-play {
    color: var(--accent);
  }
  .q-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
  }
  .q-name {
    font-size: var(--font-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .q-name.current-name {
    color: var(--accent);
  }
  .q-artists {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .q-time {
    font-size: var(--font-sm);
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
  }
  .q-actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .q-actions .icon-btn {
    width: 28px;
    height: 28px;
  }
</style>
