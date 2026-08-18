<script>
  import { library, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
</script>

<section class="page">
  <header class="page-head">
    <h1>Home</h1>
    <p class="sub">Your playlists, right here.</p>
  </header>

  {#if library.length}
    <div class="card-grid">
      {#each library as pl (pl.id)}
        <button class="card" onclick={() => navigate("playlist", pl.id)}>
          <div class="card-cover">
            <Cover src={pl.cover_url} alt={pl.name} rounded={4} />
            <span class="card-play"><Icon name="play" size={20} /></span>
          </div>
          <span class="card-name">{pl.name}</span>
          <span class="card-sub">{pl.owner ? `Playlist · ${pl.owner}` : "Playlist"}</span>
        </button>
      {/each}
    </div>
  {:else}
    <div class="empty">
      <Icon name="library" size={40} />
      <p>No playlists yet.</p>
      <p class="sub">Log in and your playlists will appear here.</p>
    </div>
  {/if}
</section>
