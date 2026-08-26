<script>
  import { library, mergePersonalizedPlaylists, navigate } from "../lib/state.svelte.js";
  import { playPlaylistById, cardPlay } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { playlistSubtitle } from "../lib/artist.js";
  const playlists = $derived(mergePersonalizedPlaylists(library));
  const tone = $derived.by(() => {
    const playlist = playlists[0];
    return coverTone(
      playlist?.cover_url || playlist?.cover_urls?.[0] || "",
      playlist?.id || "made-for-you",
    );
  });

  const busy = $state({ id: "" });
  let error = $state("");

  async function play(id) {
    error = "";
    error = await cardPlay(busy, id, () => playPlaylistById(id), "Could not play this playlist.");
  }
</script>

{#snippet playlistCard(pl)}
  {@const cardTone = coverTone(pl.cover_url || pl.cover_urls?.[0] || "", pl.id)}
  <div class="card" style:--tone-glow={cardTone.glow}>
    <div class="card-art">
      <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} fill lg />
      <button
        class="card-open"
        aria-label={`Open ${pl.name}`}
        onclick={() => navigate("playlist", pl.id)}
      ></button>
      <button
        class="card-play"
        aria-label={`Play ${pl.name}`}
        title={`Play ${pl.name}`}
        disabled={!!busy.id}
        onclick={() => play(pl.id)}
      >
        <Icon name={busy.id === pl.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
      <span class="card-name">{pl.name}</span>
      <span class="card-sub">{playlistSubtitle(pl)}</span>
    </button>
  </div>
{/snippet}

<section
  class="view page wash soft"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  <header class="mfy-head">
    <button class="link-more mfy-back" type="button" onclick={() => navigate("library")}>
      <Icon name="back" size={13} /> Home
    </button>
    <h1 class="page-title">Made for you</h1>
    <p class="sub">Your personalized mixes and weekly picks.</p>
  </header>

  {#if playlists.length}
    <div class="section mfy-section" style="margin-top:var(--s7)">
      <div class="grid">
        {#each playlists as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {:else}
    <div class="empty mfy-empty">
      <p class="h">No personalized playlists yet.</p>
      <p class="sub">Your Spotify mixes will appear here when they are available.</p>
      <div class="actions">
        <button class="btn-ghost" type="button" onclick={() => navigate("library")}>Back to Home</button>
      </div>
    </div>
  {/if}

  {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
</section>

<style>
  .mfy-head { padding: var(--s3) 0 var(--s2); }
  .mfy-back { margin-bottom: var(--s4); }
  .mfy-section { margin-top: 0; }
  .mfy-empty { margin-top: var(--s9); }
</style>
