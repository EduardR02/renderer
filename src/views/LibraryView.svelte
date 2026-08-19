<script>
  import { library, navigate, playback } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  const greeting = $derived.by(() => {
    const h = new Date().getHours();
    if (h < 5) return "Good night";
    if (h < 12) return "Good morning";
    if (h < 18) return "Good afternoon";
    return "Good evening";
  });
</script>

<section class="view page">
  <div style="padding:var(--s4) 0 var(--s6)">
    <h1 class="page-title">{greeting}</h1>
  </div>

  {#if library.length}
    <div class="section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        {#each library as pl (pl.id)}
          <button class="card" onclick={() => navigate("playlist", pl.id)}>
            <span class="card-art">
              <Cover src={pl.cover_url} id={pl.id} name={pl.name} fill lg />
              <span class="card-play"><Icon name="play" size={15} /></span>
            </span>
            <span class="card-name">{pl.name}</span>
            <span class="card-sub">
              {pl.tracks_total ? `${pl.tracks_total} songs` : "Playlist"}
            </span>
          </button>
        {/each}
      </div>
    </div>
  {:else}
    <div class="empty">
      <p>No playlists yet.</p>
      <p class="sub">They will appear here once your library loads.</p>
    </div>
  {/if}
</section>
