<script>
  import { detail, api, navigate } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  const artist = $derived(detail.artist);
  const top = $derived(artist?.top_tracks ?? []);
  const releaseSections = $derived.by(() => {
    const releases = artist?.releases ?? {};
    return [
      { key: "albums", title: "Albums", items: releases.albums ?? [] },
      { key: "singles", title: "Singles", items: releases.singles ?? [] },
      { key: "compilations", title: "Compilations", items: releases.compilations ?? [] },
      { key: "appears_on", title: "Appears on", items: releases.appears_on ?? [] },
    ];
  });
  const hasReleases = $derived(releaseSections.some((section) => section.items.length));
  const releaseTotal = $derived(releaseSections.reduce((total, section) => total + section.items.length, 0));
  const RELEASE_PREVIEW = 12;
  let catalogueFilter = $state("all");
  let catalogueExpanded = $state(false);
  let catalogueLoading = $state(false);
  let catalogueError = $state("");
  let loadingReleaseId = $state("");

  const catalogueItems = $derived(
    releaseSections.flatMap((section) =>
      section.items.map((release) => ({
        ...release,
        releaseKey: section.key,
        releaseLabel: section.title === "Appears on" ? "Appears on" : section.title.replace(/s$/, ""),
      })),
    ),
  );
  const filteredCatalogue = $derived(
    catalogueFilter === "all"
      ? catalogueItems
      : catalogueItems.filter((release) => release.releaseKey === catalogueFilter),
  );
  const visibleCatalogue = $derived(
    catalogueExpanded ? filteredCatalogue : filteredCatalogue.slice(0, RELEASE_PREVIEW),
  );
  const cataloguePlayTypes = $derived(
    catalogueFilter === "all" ? ["albums", "singles"] : [catalogueFilter],
  );
  const cataloguePlayLabel = $derived(
    catalogueFilter === "all"
      ? "Play albums + singles"
      : `Play ${releaseSections.find((section) => section.key === catalogueFilter)?.title.toLowerCase() ?? "catalogue"}`,
  );

  /* Seeds the banner's fallback gradient off the same hash as the artwork, so
     an artist with no portrait still gets a stable colour rather than a hole. */
  const hue = $derived.by(() => {
    const seed = artist?.id ?? "";
    let h = 0x811c9dc5;
    for (let i = 0; i < seed.length; i++) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h % 360;
  });

  function playFrom(i) {
    if (top.length) api.playQueue(top, i).catch(() => {});
  }

  function shufflePlay() {
    if (!top.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(top, 0).catch(() => {});
  }

  async function playCatalogue() {
    if (!artist?.id || catalogueLoading) return;
    catalogueLoading = true;
    catalogueError = "";
    try {
      const tracks = await api.browseArtistCatalogueTracks(artist.id, cataloguePlayTypes);
      if (!tracks?.length) {
        catalogueError = "No playable tracks were returned for this part of the catalogue.";
        return;
      }
      await api.playQueue(tracks, 0);
    } catch (reason) {
      catalogueError = String(reason || "Could not play this catalogue.");
    } finally {
      catalogueLoading = false;
    }
  }

  async function playRelease(release) {
    if (!release?.id || loadingReleaseId) return;
    loadingReleaseId = release.id;
    catalogueError = "";
    try {
      const album = await api.browseAlbum(release.id);
      const tracks = album?.tracks ?? [];
      if (!tracks.length) throw new Error("No playable tracks were returned for this release.");
      await api.playQueue(tracks, 0);
    } catch (reason) {
      catalogueError = String(reason || "Could not play this release.");
    } finally {
      loadingReleaseId = "";
    }
  }

  $effect(() => {
    artist?.id;
    catalogueFilter;
    catalogueExpanded = false;
    catalogueError = "";
  });
</script>

<!-- `bleed`: the banner carries its own negative margins up under the topbar,
     so the page must not pad above it. -->
<section class="view page bleed" style:--h={hue}>
  <!-- 260px, fixed. The portrait is a background inside it, never a box that
       the header is sized around, so a missing portrait costs the image and
       nothing else — the gradient and the name stay exactly where they were. -->
  <header class="banner">
    <div class="bg">
      {#if artist}
        <Cover src={artist.cover_url} id={artist.id} name={artist.name} fill />
      {/if}
    </div>
    <span class="eyebrow">Artist</span>
    {#if artist}
      <h1 class="name">{artist.name}</h1>
    {:else}
      <span class="skeleton line lg" style="margin-top:var(--s3)"></span>
    {/if}
  </header>

  {#if artist}
    <p class="detail-meta" style="margin-top:var(--s5)">
      <span class="num">{top.length} popular {top.length === 1 ? "song" : "songs"}</span>
      {#if hasReleases}
        <span class="sep">/</span><span class="num">{releaseTotal} releases</span>
      {/if}
    </p>

    <div class="actions">
      <button class="play-lg" title="Play popular songs" onclick={() => playFrom(0)} disabled={!top.length}>
        <Icon name="play" size={19} />
      </button>
      <button class="btn-ghost" onclick={shufflePlay} disabled={!top.length}>
        <Icon name="shuffle" size={14} />Shuffle
      </button>
    </div>

    {#if top.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Popular</h2></div>
        <TrackList tracks={top} {playFrom} showAlbum={false} showPlays />
      </div>
    {/if}

    {#if hasReleases}
      <section class="section catalogue" aria-labelledby="catalogue-title">
        <div class="catalogue-heading">
          <div>
            <span class="eyebrow">Discography</span>
            <h2 class="section-title" id="catalogue-title">Catalogue<span class="section-count">{releaseTotal}</span></h2>
          </div>
          <button class="btn-ghost catalogue-play" onclick={playCatalogue} disabled={catalogueLoading || !filteredCatalogue.length}>
            <Icon name="play" size={13} />{catalogueLoading ? "Building queue…" : cataloguePlayLabel}
          </button>
        </div>

        <div class="catalogue-tabs" aria-label="Filter catalogue">
          <button class:active={catalogueFilter === "all"} aria-pressed={catalogueFilter === "all"} onclick={() => (catalogueFilter = "all")}>All <span>{releaseTotal}</span></button>
          {#each releaseSections as section (section.key)}
            {#if section.items.length}
              <button
                class:active={catalogueFilter === section.key}
                aria-pressed={catalogueFilter === section.key}
                onclick={() => (catalogueFilter = section.key)}
              >{section.title} <span>{section.items.length}</span></button>
            {/if}
          {/each}
        </div>

        <div class="catalogue-list" data-release-group={catalogueFilter}>
          {#each visibleCatalogue as release (`${release.releaseKey}-${release.id}`)}
            <article class="catalogue-item">
              <button class="catalogue-open" onclick={() => navigate("album", release.id)}>
                <Cover src={release.cover_url} id={release.id} name={release.name} size={64} lg />
                <span class="catalogue-copy">
                  <span class="catalogue-name">{release.name}</span>
                  <span class="catalogue-meta">
                    <span class="release-kind">{release.releaseLabel}</span>
                    {#if release.year}<span>{release.year}</span>{/if}
                    {#if release.artist_names?.length}<span class="catalogue-artists">{release.artist_names.join(", ")}</span>{/if}
                  </span>
                </span>
              </button>
              <button
                class="catalogue-item-play"
                title={`Play ${release.name}`}
                aria-label={`Play ${release.name}`}
                disabled={!!loadingReleaseId}
                onclick={() => playRelease(release)}
              ><Icon name={loadingReleaseId === release.id ? "more" : "play"} size={14} /></button>
            </article>
          {/each}
        </div>

        {#if filteredCatalogue.length > RELEASE_PREVIEW}
          <button class="catalogue-more" onclick={() => (catalogueExpanded = !catalogueExpanded)}>
            {catalogueExpanded ? "Show less" : `Show all ${filteredCatalogue.length}`}
            <Icon name={catalogueExpanded ? "chevron-up" : "chevron-down"} size={14} />
          </button>
        {/if}
        {#if catalogueError}<p class="inline-error" role="alert">{catalogueError}</p>{/if}
      </section>
    {/if}

    {#if !top.length && !hasReleases}
      <div class="empty">
        <p class="h">Nothing to show for this artist.</p>
        <p class="sub">The engine returned no popular songs or releases.</p>
      </div>
    {/if}
  {/if}
</section>
