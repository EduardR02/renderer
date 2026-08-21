<script>
  import { detail, navigate } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  /* This is deliberately a projection of the artist response, not a browse
     surface. The artist page already paid for the related-artist payload, so
     opening See all must not issue a second request. */
  const artist = $derived(detail.artist);
  const relatedArtists = $derived(artist?.overview?.related_artists ?? []);
</script>

<section class="view page aux-page">
  {#if artist}
    <header class="aux-head">
      <button class="aux-back" onclick={() => navigate("artist", artist.id)}>
        <Icon name="back" size={13} />{artist.name}
      </button>
      <div class="aux-title-row">
        <h1 class="page-title">Fans also like</h1>
        {#if relatedArtists.length}<span class="section-count tnum">{relatedArtists.length}</span>{/if}
      </div>
    </header>

    {#if relatedArtists.length}
      <div class="grid aux-grid">
        {#each relatedArtists as related (related.id)}
          {@const tone = coverTone(related.cover_url, related.id)}
          <button
            class="card artist-card"
            style:--tone-glow={tone.glow}
            onclick={() => navigate("artist", related.id)}
          >
            <span class="card-art">
              <Cover
                src={related.cover_url}
                id={related.id}
                name={related.name}
                fill
                circle
              />
            </span>
            <span class="card-copy">
              <span class="card-name">{related.name}</span>
              <span class="card-sub">Artist</span>
            </span>
          </button>
        {/each}
      </div>
    {:else}
      <div class="empty">
        <p class="h">No related artists in this profile.</p>
        <p class="sub">Fans also like is only shown when Spotify supplies the related-artist list.</p>
      </div>
    {/if}
  {:else if detail.error}
    <header class="aux-head">
      <div class="aux-title-row"><h1 class="page-title">Fans also like</h1></div>
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
  .artist-card { width: 100%; }
  .artist-card .card-art { --tile: 170px; }
</style>
