<script>
  import { detail, navigateArtist } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  /* This is deliberately a projection of the artist response, not a browse
     surface. The artist page already paid for the related-artist payload, so
     opening See all must not issue a second request. But it is still a page,
     so it wears the artist's own tone — soft, like the other pages whose
     subject is a person's orbit rather than one record. */
  const artist = $derived(detail.artist);
  const relatedArtists = $derived(artist?.overview?.related_artists ?? []);
  const tone = $derived(coverTone(artist?.cover_url || "", artist?.id || ""));
</script>

<section class="view page aux-page wash soft" style:--tone-wash={tone.wash} style:--tone-wash-deep={tone.washDeep} style:--tone-glow={tone.glow}>

  {#if artist}
    <header class="aux-head">
      <button class="page-back" onclick={() => navigateArtist(artist.id, artist.name)}>
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
            onclick={() => navigateArtist(related.id, related.name)}
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
  .aux-title-row { display: flex; align-items: baseline; gap: var(--s3); margin-top: var(--s3); }
  .aux-grid { grid-template-columns: repeat(auto-fill, minmax(158px, 1fr)); }
  .artist-card { width: 100%; }
  .artist-card .card-art { --tile: 170px; }
</style>
