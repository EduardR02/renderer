<script>
  import {
    playback,
    ui,
    navigate,
    openCredits,
  } from "../lib/state.svelte.js";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Icon from "./Icon.svelte";

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const next = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index + 1] ?? null) : null,
  );
  const playCountFormatter = new Intl.NumberFormat();
</script>

<aside class="np-panel" aria-label="Now playing details">
  <div class="np-head">
    <span class="np-head-title"><i aria-hidden="true"></i>Now playing</span>
    <button class="btn-icon" title="Close now playing details" onclick={() => (ui.nowPlayingOpen = false)}>
      <Icon name="x" size={13} />
    </button>
  </div>

  {#if current}
    <div class="np-hero">
      <div class="np-art">
        <Cover
          src={current.cover_url}
          id={current.album_id || current.uri}
          name={current.album_name || current.name}
          fill
          lg
          raised
        />
      </div>
    </div>

    <div class="np-copy">
      <h2>{current.name}</h2>
      <ArtistLinks
        class="np-artists"
        names={current.artist_names}
        ids={current.artist_ids ?? []}
        id={current.artist_id}
      />
    </div>

    <div class="np-actions">
      <button class="btn-ghost" onclick={() => navigate("queue")}>
        <Icon name="queue" size={14} />Queue
      </button>
      <button class="btn-ghost" onclick={() => openCredits(current)}>
        View credits
      </button>
    </div>

    {#if current.album_id || current.play_count}
      <div class="np-context">
        {#if current.album_id}
          <button class="np-fact" onclick={() => navigate("album", current.album_id)}>
            <span class="np-label">Album</span>
            <span>{current.album_name || "Open album"}</span>
            <Icon name="forward" size={12} />
          </button>
        {/if}
        {#if current.play_count}
          <div class="np-fact">
            <span class="np-label">Plays</span>
            <span>{playCountFormatter.format(current.play_count)}</span>
          </div>
        {/if}
      </div>
    {/if}

    {#if next}
      <button class="np-next" onclick={() => navigate("queue")}>
        <span class="np-next-label">Up next</span>
        <Cover src={next.cover_url} id={next.album_id || next.uri} name={next.name} size={40} />
        <span class="np-next-copy">
          <strong>{next.name}</strong>
          <span>{(next.artist_names ?? []).join(", ")}</span>
        </span>
        <Icon name="forward" size={13} />
      </button>
    {/if}
  {:else}
    <div class="np-empty">
      <p>Nothing playing</p>
      <span>Start a song to see its artwork, artists, album, and credits.</span>
    </div>
  {/if}
</aside>
