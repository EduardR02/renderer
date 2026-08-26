<script>
  import {
    detail,
    navigate,
    navigateArtist,
  } from "../lib/state.svelte.js";
  import { createCataloguePaging, sentinelLoader } from "../lib/discography.svelte.js";
  import { playAlbumById, cardPlay } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  const artist = $derived(detail.artist);
  const counts = $derived(artist?.release_counts ?? {});
  const appearsOnCount = $derived(counts.appears_on ?? 0);
  const APPEARS_ON_TYPES = ["appears_on"];

  let busy = $state({ id: "" });
  let playError = $state("");
  let sentinel = $state(null);

  /**
   * The paged catalogue reader shared with the discography (see src/lib/
   * discography.svelte.js), fixed to the appears-on slice. The summary count
   * gates it: a profile with no appears-on releases never spends a request.
   */
  const paging = createCataloguePaging({
    getId: () => artist?.id ?? "",
    releaseTypes: () => APPEARS_ON_TYPES,
    pageSize: 6,
    mayLoad: () => !!appearsOnCount,
    seedTotal: () => appearsOnCount,
    errorMessage: "Could not load Appears On.",
  });

  /* A new artist is another list: drop what arrived for the last one. */
  $effect(() => paging.reset());

  /* Play marks belong to one artist's payload; they die with it. */
  $effect(() => {
    artist?.id;
    busy.id = "";
    playError = "";
  });

  /* Further pages load only when real scrolling reaches the footer; the
     button remains the keyboard fallback. */
  $effect(() => sentinelLoader(sentinel, () => paging.loadNext()));
  async function playRelease(id) {
    if (busy.id) return;
    const artistId = artist?.id;
    playError = "";
    playError = await cardPlay(
      busy,
      id,
      () => playAlbumById(id),
      "Could not play this release.",
      () => artist?.id === artistId,
    );
  }


</script>

<section class="view page aux-page appears-page">
  {#if artist}
    <header class="aux-head">
      <button class="aux-back" onclick={() => navigateArtist(artist.id, artist.name)}>
        <Icon name="back" size={13} />{artist.name}
      </button>
      <div class="aux-title-row">
        <h1 class="page-title">Appears On</h1>
        <span class="section-count tnum">{appearsOnCount}</span>
      </div>
    </header>

    {#if paging.releases.length}
      <div class="grid appears-grid">
        {#each paging.releases as release (release.id)}
          {@const tone = coverTone(release.cover_url, release.id)}
          <div class="card" style:--tone-glow={tone.glow}>
            <div class="card-art">
              <Cover src={release.cover_url} id={release.id} name={release.name} fill lg />
              <button
                class="card-open"
                aria-label={`Open ${release.name}`}
                onclick={() => navigate("album", release.id)}
              ></button>
              <button
                class="card-play"
                aria-label={`Play ${release.name}`}
                title={`Play ${release.name}`}
                disabled={!!busy.id}
                onclick={() => playRelease(release.id)}
              >
                <Icon name={busy.id === release.id ? "more" : "play"} size={15} />
              </button>
            </div>
            <button class="card-copy" onclick={() => navigate("album", release.id)}>
              <span class="card-name">{release.name}</span>
              <span class="card-sub">{[release.year || null, "Appears on"].filter(Boolean).join(" · ")}</span>
            </button>
          </div>
        {/each}
      </div>
    {:else if paging.loading}
      <div class="grid appears-grid" aria-hidden="true">
        {#each Array.from({ length: 3 }) as _, i (i)}
          <div class="card">
            <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
            <span class="card-copy">
              <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
              <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
            </span>
          </div>
        {/each}
      </div>
    {:else if !paging.error}
      <div class="empty">
        <p class="h">No Appears On releases are available.</p>
        <p class="sub">Spotify did not return release summaries for this artist.</p>
      </div>
    {/if}

    <div class="aux-foot" bind:this={sentinel}>
      {#if paging.loading && paging.releases.length}
        <span class="aux-status">Loading releases…</span>
      {:else if paging.error && paging.nextOffset != null}
        <button class="btn-ghost" onclick={() => paging.loadNext()}>Try again</button>
      {:else if paging.nextOffset != null && paging.releases.length}
        <button class="btn-ghost" onclick={() => paging.loadNext()}>Load more<Icon name="chevron-down" size={14} /></button>
      {:else if paging.releases.length}
        <span class="aux-status"><span class="tnum">{paging.releases.length}</span>{paging.total && paging.total !== paging.releases.length ? ` of ${paging.total}` : ""} releases</span>
      {/if}
    </div>
    {#if paging.error}<p class="inline-error" role="alert">{paging.error}</p>{/if}
    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
  {:else if detail.error}
    <header class="aux-head">
      <div class="aux-title-row"><h1 class="page-title">Appears On</h1></div>
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
  .appears-grid { grid-template-columns: repeat(auto-fill, minmax(158px, 1fr)); }
  .aux-foot { display: flex; justify-content: center; min-height: 56px; padding-top: var(--s6); }
  .aux-status { color: var(--fg-2); font-size: var(--t-12); }
</style>
