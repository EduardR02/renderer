<script>
  import { detail, navigate, route } from "../lib/state.svelte.js";
  import { playPlaylistById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { artistPlaylistCollections, playlistSubtitle } from "../lib/artist.js";

  /* Both routes project the artist overview already in memory. Opening a full
     collection must not repeat the artist request. Keep the same cross-shelf
     deduplication as ArtistView: Artist Pick wins, then Artist playlists, then
     Discovered on. */
  const artist = $derived(detail.artist);
  const overview = $derived(artist?.overview ?? null);
  const collections = $derived(artistPlaylistCollections(overview));
  const discovered = $derived(route.name === "discovered-on");
  const title = $derived(discovered ? "Discovered on" : "Artist playlists");
  const playlists = $derived(discovered ? collections.discovered : collections.artist);

  let busy = $state("");
  let error = $state("");
  let playGeneration = 0;
  let loadedRoute = "";

  $effect(() => {
    const key = `${route.name}:${route.id ?? ""}`;
    if (key === loadedRoute) return;
    loadedRoute = key;
    playGeneration += 1;
    busy = "";
    error = "";
  });

  async function play(id) {
    if (busy) return;
    const generation = playGeneration;
    const key = `${route.name}:${route.id ?? ""}`;
    busy = id;
    error = "";
    try {
      await playPlaylistById(id);
    } catch (reason) {
      if (generation === playGeneration && key === `${route.name}:${route.id ?? ""}`) {
        error = String(reason || "Could not play this playlist.");
      }
    } finally {
      if (generation === playGeneration && key === `${route.name}:${route.id ?? ""}`) {
        busy = "";
      }
    }
  }
</script>

{#snippet playlistCard(playlist)}
  {@const tone = coverTone(playlist.cover_url || playlist.cover_urls?.[0] || "", playlist.id)}
  <div class="card" style:--tone-glow={tone.glow}>
    <div class="card-art">
      <Cover
        src={playlist.cover_url}
        srcs={playlist.cover_urls ?? []}
        id={playlist.id}
        name={playlist.name}
        fill
        lg
      />
      <button
        class="card-open"
        aria-label={`Open ${playlist.name}`}
        onclick={() => navigate("playlist", playlist.id)}
      ></button>
      <button
        class="card-play"
        aria-label={`Play ${playlist.name}`}
        title={`Play ${playlist.name}`}
        disabled={!!busy}
        onclick={() => play(playlist.id)}
      >
        <Icon name={busy === playlist.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", playlist.id)}>
      <span class="card-name">{playlist.name}</span>
      <span class="card-sub">{playlistSubtitle(playlist)}</span>
    </button>
  </div>
{/snippet}

<section class="view page aux-page">
  {#if artist}
    <header class="aux-head">
      <button class="aux-back" onclick={() => navigate("artist", artist.id)}>
        <Icon name="back" size={13} />{artist.name}
      </button>
      <div class="aux-title-row">
        <h1 class="page-title">{title}</h1>
        {#if playlists.length}<span class="section-count tnum">{playlists.length}</span>{/if}
      </div>
    </header>

    {#if playlists.length}
      <div class="grid aux-grid">
        {#each playlists as playlist (playlist.id)}
          {@render playlistCard(playlist)}
        {/each}
      </div>
    {:else}
      <div class="empty">
        <p class="h">No {title.toLowerCase()} in this profile.</p>
        <p class="sub">This collection is only shown when Spotify supplies playlist references.</p>
      </div>
    {/if}
    {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
  {:else if detail.error}
    <header class="aux-head">
      <div class="aux-title-row"><h1 class="page-title">{title}</h1></div>
    </header>
    <div class="empty failed">
      <p class="h">This artist could not be loaded.</p>
      <p class="why">{detail.error}</p>
    </div>
  {:else}
    <header class="aux-head" aria-hidden="true">
      <span class="skeleton line sm"></span>
      <span class="skeleton line lg" style="height:40px;width:min(340px,60%)"></span>
    </header>
  {/if}
</section>

<style>
  .aux-page { padding-top: var(--s5); }
  .aux-head { padding: var(--s2) 0 var(--s6); }
  .aux-back {
    display: inline-flex; align-items: center; gap: var(--s2);
    color: var(--fg-2); font-size: var(--t-12);
  }
  .aux-back:hover { color: var(--fg); }
  .aux-title-row { display: flex; align-items: baseline; gap: var(--s3); margin-top: var(--s3); }
  .aux-grid { grid-template-columns: repeat(auto-fill, minmax(158px, 1fr)); }
</style>
