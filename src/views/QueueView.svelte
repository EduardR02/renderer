<script>
  import { playback, api, navigate, togglePlay } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTime, formatTotal } from "../lib/time.js";

  const queue = $derived(playback.queue);

  function playAt(i) {
    if (i === playback.current_index) togglePlay();
    else api.playQueue(queue, i).catch(() => {});
  }
</script>

<section class="view page">
  <div style="padding:var(--s4) 0 var(--s6)">
    <span class="eyebrow">Up next</span>
    <h1 class="page-title">Queue</h1>
    {#if queue.length}
      <p class="detail-meta" style="margin-top:var(--s3)">
        <span class="num">{queue.length} {queue.length === 1 ? "song" : "songs"}</span>
        <span class="sep">/</span><span class="num">{formatTotal(queue)}</span>
        {#if playback.current_index >= 0}
          <span class="sep">/</span><span class="num">now playing #{playback.current_index + 1}</span>
        {/if}
      </p>
    {/if}
  </div>

  {#if queue.length}
    <!-- Same row system as every other track table; only the trailing cell
         differs, so a queue row and a playlist row line up exactly. -->
    <div class="tl queue">
      <div class="tl-head">
        <span style="text-align:right">#</span>
        <span></span>
        <span>Title</span>
        <span style="display:grid;justify-items:end"><Icon name="clock" size={13} /></span>
        <span></span>
      </div>

      {#each queue as track, i (`${track.uri}-${i}`)}
        <div
          class="tl-row"
          class:current={i === playback.current_index}
          role="button"
          tabindex="-1"
          ondblclick={() => playAt(i)}
        >
          <span class="c-idx">
            <span class="n">{i + 1}</span>
            <span class="eq"><i></i><i></i><i></i><i></i></span>
            <button class="go" title="Play from here" onclick={() => playAt(i)}>
              <Icon name={i === playback.current_index && playback.playing ? "pause" : "play"} size={12} />
            </button>
          </span>

          <Cover
            src={track.cover_url}
            id={track.album_id || track.uri}
            name={track.album_name || track.name}
            size={36}
            class="c-art"
          />

          <span class="c-title">
            <span class="t-name">{track.name}</span>
            <span class="t-artists">{track.artist_names.join(", ")}</span>
          </span>

          <span class="c-time">{formatTime(track.duration_ms)}</span>

          <span class="q-actions">
            <button
              title="Move up"
              disabled={i === 0}
              onclick={() => api.moveQueue(i, i - 1).catch(() => {})}
            >
              <Icon name="chevron-up" size={15} />
            </button>
            <button
              title="Move down"
              disabled={i === queue.length - 1}
              onclick={() => api.moveQueue(i, i + 1).catch(() => {})}
            >
              <Icon name="chevron-down" size={15} />
            </button>
            <button class="danger" title="Remove from queue" onclick={() => api.removeQueue(i).catch(() => {})}>
              <Icon name="x" size={14} />
            </button>
          </span>
        </div>
      {/each}
    </div>
  {:else}
    <div class="empty">
      <p class="h">The queue is empty.</p>
      <p class="sub">Play a playlist or add single tracks from any track menu.</p>
      <div class="actions">
        <button class="btn-ghost" onclick={() => navigate("library")}>
          <Icon name="library" size={14} />Go to your library
        </button>
      </div>
    </div>
  {/if}
</section>
