<script>
  import { playback, ui, navigate, openCredits } from "../lib/state.svelte.js";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Icon from "./Icon.svelte";

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const playCountFormatter = new Intl.NumberFormat();
</script>

<aside class="np-panel" aria-label="Now playing details">
  <div class="np-head">
    <span>Now playing</span>
    <button class="btn-icon" title="Close now playing details" onclick={() => (ui.nowPlayingOpen = false)}>
      <Icon name="x" size={13} />
    </button>
  </div>

  {#if current}
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

    <div class="np-copy">
      <h2>{current.name}</h2>
      <ArtistLinks
        class="np-artists"
        names={current.artist_names}
        ids={current.artist_ids ?? []}
        id={current.artist_id}
      />
    </div>

    <div class="np-facts">
      {#if current.album_id}
        <button class="np-fact" onclick={() => navigate("album", current.album_id)}>
          <span class="np-label">From</span>
          <span>{current.album_name || "Open album"}</span>
        </button>
      {/if}
      {#if current.play_count}
        <div class="np-fact">
          <span class="np-label">Plays</span>
          <span>{playCountFormatter.format(current.play_count)}</span>
        </div>
      {/if}
    </div>

    <button class="btn-ghost np-credits" onclick={() => openCredits(current)}>
      View credits
    </button>
  {:else}
    <div class="np-empty">
      <p>Nothing playing</p>
      <span>Start a song to see its artwork, artists, album, and credits.</span>
    </div>
  {/if}
</aside>
