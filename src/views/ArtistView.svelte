<script>
  import { detail, api, navigate, ui, retryDetail, loadCataloguePage } from "../lib/state.svelte.js";
  import { playAlbumById, playPlaylistById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { GROUPS, RELEASE_KEYS } from "../lib/discography.js";
  import { artistPlaylistCollections } from "../lib/artist.js";

  /**
   * The artist page: who this is, what to play, and one shelf of records.
   *
   * The redesign folded the whole discography reader into this page, so an
   * artist page was a several-screen scroll through every track of every
   * release. That is a different job. This page INTRODUCES an artist — the
   * portrait, the songs people actually play, and one row of covers you can
   * recognise a record from — and hands the reading to `discography`.
   *
   * The shelf is ONE row, not four. The pre-redesign page stacked a separate
   * shelf for Albums, Singles, Compilations and Appears-on, each with its own
   * "Show all". The segmented toggle now covers only the three main catalogue
   * groups; Appears On lives in its own lazy shelf near the bottom. "See all"
   * carries a regular catalogue selection into the reader; ranked Popular
   * releases open the reader on "All" because that order only exists in this
   * overview payload. Every release on the main shelf comes out of the artist
   * payload. Regular groups carry per-type summaries; ranked Popular releases
   * come from the overview.
   */
  const artist = $derived(detail.artist);
  const top = $derived(artist?.top_tracks ?? []);
  const counts = $derived(artist?.release_counts ?? {});
  const overview = $derived(artist?.overview ?? null);
  const popularReleases = $derived(overview?.popular_releases ?? []);
  const relatedArtists = $derived(overview?.related_artists ?? []);
  const topCities = $derived(overview?.top_cities ?? []);
  const artistPickPlaylist = $derived(overview?.artist_pick ?? null);
  const playlistShelves = $derived(artistPlaylistCollections(overview));
  const NUMBER_FORMAT = new Intl.NumberFormat();
  const aboutStats = $derived.by(() => {
    const facts = [];
    if (overview?.monthly_listeners) {
      facts.push({ value: NUMBER_FORMAT.format(overview.monthly_listeners), label: "Monthly listeners" });
    }
    if (overview?.followers) {
      facts.push({ value: NUMBER_FORMAT.format(overview.followers), label: "Followers" });
    }
    if (overview?.world_rank) {
      facts.push({ value: `#${NUMBER_FORMAT.format(overview.world_rank)}`, label: "Worldwide" });
    }
    if (overview?.popularity) {
      facts.push({ value: `${overview.popularity}/100`, label: "Popularity" });
    }
    return facts;
  });
  let shelfExtras = $state({});

  function mergeShelfReleases(primary, extras) {
    const seen = new Set();
    const merged = [];
    for (const release of [...(primary ?? []), ...(extras ?? [])]) {
      const id = release?.id;
      if (id && seen.has(id)) continue;
      if (id) seen.add(id);
      merged.push(release);
    }
    return merged;
  }

  const groupLists = $derived.by(() => {
    const base = artist?.releases ?? {};
    const artistId = artist?.id ?? "";
    return Object.fromEntries(
      RELEASE_KEYS.map((key) => [
        key,
        mergeShelfReleases(base[key], shelfExtras[`${artistId}:${key}`]),
      ]),
    );
  });

  const releaseTotal = $derived(RELEASE_KEYS.reduce((sum, k) => sum + (counts[k] ?? 0), 0));
  const hasReleases = $derived(releaseTotal > 0);

  /** Empty groups are not offered: a dead tab is a worse answer than no tab. */
  const segments = $derived(
    [
      ...(popularReleases.length ? [{ id: "popular", label: "Popular releases" }] : []),
      ...GROUPS,
    ]
      .filter(
        (g) =>
          g.id === "all" ||
          g.id === "popular" ||
          (counts[g.id] ?? 0) > 0,
      )
      .map((g) => ({
        ...g,
        count:
          g.id === "all"
            ? releaseTotal
            : g.id === "popular"
              ? popularReleases.length
              : (counts[g.id] ?? 0),
      })),
  );

  let group = $state("popular");

  const requestedShelfPages = new Set();
  let shelfRetry = $state(0);
  let shelfError = $state("");
  let shelfErrorKey = $state("");
  let artistGeneration = 0;
  let busy = $state("");
  let playError = $state("");

  /* The artist changed under us — a link from a track row, say. Start the new
     artist on its ranked releases when the overview provides them, otherwise
     fall back to the balanced "All" shelf. */
  let seenArtist = null;
  $effect(() => {
    if (artist?.id === seenArtist) return;
    seenArtist = artist?.id ?? null;
    artistGeneration += 1;
    group = popularReleases.length ? "popular" : "all";
    shelfExtras = {};
    requestedShelfPages.clear();
    shelfRetry = 0;
    shelfError = "";
    shelfErrorKey = "";
    appearsOnReady = false;
    appearsOnLoading = false;
    appearsOnError = "";
    appearsOnRetry = 0;
    busy = "";
    playError = "";
  });

  /* An overview may be refreshed independently of the catalogue payload. Do
     not leave the selector on a segment whose server-ranked data disappeared. */
  $effect(() => {
    if (!popularReleases.length && group === "popular") group = "all";
  });

  /** The label for a release's own group, used as its subtext on the shelf. */
  const KIND_LABEL = {
    albums: "Album",
    singles: "Single or EP",
    compilations: "Compilation",
  };

  /**
   * What the shelf shows for the current segment.
   *
   * Named catalogue segments show the order returned by the engine. Ranked
   * Popular releases use the overview's server order. "All" is the exception:
   * it intersperses the most recent releases across the three main types,
   * which is what a highlight row on an artist page is for.
   */
  const shelf = $derived.by(() => {
    if (group === "popular") {
      return popularReleases.map((release) => ({
        release,
        kind: release.artist_names?.join(", ") || "Release",
      }));
    }
    if (group !== "all") {
      return (groupLists[group] ?? []).map((release) => ({ release, kind: KIND_LABEL[group] }));
    }
    return RELEASE_KEYS.flatMap((key) =>
      (groupLists[key] ?? []).map((release) => ({ release, kind: KIND_LABEL[key] })),
    ).sort((a, b) => (b.release.year ?? 0) - (a.release.year ?? 0));
  });

  /**
   * How many cards fit in exactly one row.
   *
   * A shelf that wraps is not a shelf, and one that clips a card mid-cover
   * reads as a rendering fault, so the count is computed from the pane rather
   * than left to `auto-fill`. 176px is the smallest tile at which a sleeve is
   * still recognisable at a glance; the cards then stretch to fill the row, so
   * the shelf is flush with the page at every width.
   */
  const CARD_MIN = 176;
  const CARD_GAP = 20; /* --s5 */
  const perRow = $derived.by(() => {
    const usable = (ui.paneWidth || 1200) - 2 * 24;
    const n = Math.floor((usable + CARD_GAP) / (CARD_MIN + CARD_GAP));
    return Math.max(2, Math.min(7, n));
  });
  const visibleShelf = $derived(shelf.slice(0, perRow));
  const visibleRelatedArtists = $derived(relatedArtists.slice(0, perRow));
  const appearsOnCount = $derived(counts.appears_on ?? 0);
  const appearsOnReleases = $derived.by(() =>
    mergeShelfReleases(
      artist?.releases?.appears_on,
      shelfExtras[`${artist?.id ?? ""}:appears_on`],
    ),
  );
  const visibleAppearsOn = $derived(appearsOnReleases.slice(0, perRow));
  let appearsOnSentinel = $state(null);
  let appearsOnReady = $state(false);
  let appearsOnLoading = $state(false);
  let appearsOnError = $state("");
  let appearsOnRetry = $state(0);

  function retryAppearsOn() {
    if (!appearsOnError) return;
    appearsOnError = "";
    appearsOnRetry += 1;
    appearsOnReady = true;
  }

  /**
   * A named shelf starts with lightweight AlbumRefs. Hydrate one bounded page
   * per artist/group; narrower panes reuse and slice it rather than turning
   * every resize into another request.
   */
  const SHELF_PAGE_SIZE = 6;

  $effect(() => {
    const id = artist?.id;
    if (!id || group === "all" || group === "popular") return;

    const selected = GROUPS.find((candidate) => candidate.id === group);
    const target = Math.min(SHELF_PAGE_SIZE, counts[group] ?? 0);
    const loaded = (groupLists[group] ?? []).length;
    if (!selected || target <= 0 || loaded >= target) return;

    const requestKey = `${id}:${group}:${shelfRetry}`;
    if (requestedShelfPages.has(requestKey)) return;
    requestedShelfPages.add(requestKey);

    const viewKey = `${id}:${group}`;
    shelfError = "";
    shelfErrorKey = "";
    loadCataloguePage(id, selected.types, 0, SHELF_PAGE_SIZE)
      .then((page) => {
        if (artist?.id !== id) return;

        const existing = shelfExtras[viewKey] ?? [];
        const known = new Set(
          [...(artist?.releases?.[selected.id] ?? []), ...existing]
            .map((release) => release?.id)
            .filter(Boolean),
        );
        const additions = [];
        for (const release of page?.releases ?? []) {
          if (!release?.id || known.has(release.id)) continue;
          known.add(release.id);
          additions.push(release);
        }
        if (additions.length) shelfExtras[viewKey] = [...existing, ...additions];
      })
      .catch((reason) => {
        requestedShelfPages.delete(requestKey);
        if (artist?.id !== id || group !== selected.id) return;
        shelfErrorKey = viewKey;
        shelfError = String(reason || "Could not load more of this catalogue.");
      });
  });
  /* Appears On is intentionally not part of the main selector. Its first
     bounded page waits until the lower shelf approaches the viewport. */
  $effect(() => {
    const node = appearsOnSentinel;
    if (!node || !artist?.id || !appearsOnCount) return;
    const scroller = node.closest(".scroll");
    if (!scroller) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) appearsOnReady = true;
      },
      { root: scroller, rootMargin: "0px 0px 360px 0px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  $effect(() => {
    const id = artist?.id;
    if (!id || !appearsOnReady || !appearsOnCount || appearsOnLoading || appearsOnError) return;
    const target = Math.min(SHELF_PAGE_SIZE, appearsOnCount);
    if (appearsOnReleases.length >= target) return;
    const requestKey = `${id}:appears_on:${appearsOnRetry}`;
    if (requestedShelfPages.has(requestKey)) return;
    requestedShelfPages.add(requestKey);
    appearsOnLoading = true;
    appearsOnError = "";
    loadCataloguePage(id, ["appears_on"], 0, SHELF_PAGE_SIZE)
      .then((page) => {
        if (artist?.id !== id) return;
        const key = `${id}:appears_on`;
        const existing = shelfExtras[key] ?? [];
        const known = new Set(
          [...(artist?.releases?.appears_on ?? []), ...existing]
            .map((release) => release?.id)
            .filter(Boolean),
        );
        const additions = [];
        for (const release of page?.releases ?? []) {
          if (!release?.id || known.has(release.id)) continue;
          known.add(release.id);
          additions.push(release);
        }
        if (additions.length) shelfExtras[key] = [...existing, ...additions];
      })
      .catch((reason) => {
        requestedShelfPages.delete(requestKey);
        if (artist?.id === id) appearsOnError = String(reason || "Could not load Appears On.");
      })
      .finally(() => {
        if (artist?.id === id) appearsOnLoading = false;
      });
  });

  const currentShelfError = $derived.by(() => {
    const key = `${artist?.id ?? ""}:${group}`;
    return shelfErrorKey === key ? shelfError : "";
  });

  function retryShelf() {
    if (!currentShelfError) return;
    shelfError = "";
    shelfErrorKey = "";
    shelfRetry += 1;
  }

  function openDiscography() {
    navigate("discography", artist.id, group === "popular" ? "all" : group);
  }

  /* Popular opens at five. Ten rows of a table before you reach the covers
     buries the thing the page is actually for. */
  const POPULAR_PREVIEW = 5;
  let popularExpanded = $state(false);
  const visibleTop = $derived(popularExpanded ? top : top.slice(0, POPULAR_PREVIEW));

  function playTop(i) {
    if (top.length) api.playQueue(top, i).catch(() => {});
  }

  function shuffleTop() {
    if (!top.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(top, 0).catch(() => {});
  }

  function openArtistRadio() {
    if (artist?.id) navigate("radio", `artist:${artist.id}`);
  }

  /* A shelf card knows an id and nothing else, so playing it costs one browse
     first — the shared rule for every card in the app (see lib/play.js). */
  async function playRelease(id) {
    if (busy) return;
    const generation = artistGeneration;
    busy = id;
    playError = "";
    try {
      await playAlbumById(id);
    } catch (reason) {
      if (generation === artistGeneration) {
        playError = String(reason || "Could not play this release.");
      }
    } finally {
      if (generation === artistGeneration) busy = "";
    }
  }
  async function playPlaylist(id) {
    if (busy) return;
    const generation = artistGeneration;
    busy = id;
    playError = "";
    try {
      await playPlaylistById(id);
    } catch (reason) {
      if (generation === artistGeneration) {
        playError = String(reason || "Could not play this playlist.");
      }
    } finally {
      if (generation === artistGeneration) busy = "";
    }
  }
</script>
{#snippet playlistCard(pl)}
  {@const tone = coverTone(pl.cover_url || pl.cover_urls?.[0] || "", pl.id)}
  <div class="card" style:--tone-glow={tone.glow}>
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
        disabled={!!busy}
        onclick={() => playPlaylist(pl.id)}
      >
        <Icon name={busy === pl.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
      <span class="card-name">{pl.name}</span>
      <span class="card-sub">{pl.tracks_total ? `${pl.tracks_total} songs` : "Playlist"}</span>
    </button>
  </div>
{/snippet}


<!-- `bleed`: the banner carries its own negative margins up under the topbar,
     so the page must not pad above it. -->
<section class="view page bleed">
  <!-- 260px, fixed. The portrait is a background inside it, never a box that
       the header is sized around, so a missing portrait costs the image and
       nothing else — the gradient and the name stay exactly where they were. -->
  <header class="banner">
    <div class="bg">
      {#if artist}
        <Cover src={artist.cover_url} id={artist.id} name={artist.name} fill />
      {:else}
        <!-- Something has to be behind the banner's darkening gradient while
             the portrait is on its way, or 260px of the page is pure black and
             reads as a rendering failure rather than as a wait. -->
        <span class="banner-sk"></span>
      {/if}
    </div>
    <span class="tag">Artist</span>
    {#if artist}
      <h1 class="name">{artist.name}</h1>
    {:else}
      <span class="skeleton line lg" style="margin-top:var(--s3);height:52px;width:min(420px,62%)"></span>
    {/if}
  </header>

  {#if detail.error && !artist}
    <div class="empty failed" style="margin-top:var(--s6)">
      <p class="h">This artist could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="btn-ghost" onclick={retryDetail}>Try again</button>
        <button class="btn-ghost" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !artist}
    <!-- The frame the artist arrives into. Every block below is the size of
         the block that replaces it, so nothing on this page moves when the
         payload lands — which is the whole point of drawing it at all. -->
    <div aria-hidden="true">
      <p class="detail-meta" style="margin-top:var(--s5)">
        <span class="skeleton line sm" style="margin:0"></span>
      </p>
      <div class="actions">
        <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
        <span class="skeleton" style="width:104px;height:32px;border-radius:var(--r2)"></span>
      </div>
      <div class="section">
        <div class="section-head"><span class="skeleton line" style="width:110px;height:22px"></span></div>
        {#each Array.from({ length: 5 }) as _, i (i)}
          <div class="sk-row" style="--cols:28px 36px minmax(0,1fr) 52px">
            <span class="sk" style="width:12px"></span>
            <span class="sk art"></span>
            <span class="sk-stack">
              <span class="sk a" style="width:{62 - ((i * 9) % 24)}%"></span>
              <span class="sk b" style="width:{30 - ((i * 5) % 11)}%"></span>
            </span>
            <span class="sk" style="width:28px;justify-self:end"></span>
          </div>
        {/each}
      </div>
      <div class="section">
        <div class="section-head"><span class="skeleton line" style="width:132px;height:22px"></span></div>
        <div class="shelf" style:--per-row={perRow}>
          {#each Array.from({ length: perRow }) as _, i (i)}
            <div class="card">
              <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
              <span class="card-copy">
                <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
                <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
              </span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  {:else}
    <p class="detail-meta" style="margin-top:var(--s5)">
      <span class="num">{top.length} popular {top.length === 1 ? "song" : "songs"}</span>
      {#if hasReleases}
        <span class="sep">/</span><span class="num">{releaseTotal} releases</span>
      {/if}
    </p>

    <div class="actions">
      <button class="play-lg" title="Play popular songs" onclick={() => playTop(0)} disabled={!top.length}>
        <Icon name="play" size={19} />
      </button>
      <button class="btn-ghost" onclick={shuffleTop} disabled={!top.length}>
        <Icon name="shuffle" size={14} />Shuffle
      </button>
      <button class="btn-ghost" onclick={openArtistRadio} disabled={!artist?.id}>
        Artist Radio
      </button>
    </div>

    {#if top.length}
      <div class="section">
        <div class="section-head">
          <h2 class="section-title">Popular</h2>
          {#if top.length > POPULAR_PREVIEW}
            <button class="link-more" onclick={() => (popularExpanded = !popularExpanded)}>
              {popularExpanded ? "Show less" : `Show all ${top.length}`}
            </button>
          {/if}
        </div>
        <!-- Popular is a bounded section of a longer page, so it renders all
             rows and owns no shared-pane scroll/resize pipeline. -->
        <TrackList tracks={visibleTop} playFrom={playTop} showAlbum={false} showPlays disableWindowing />
      </div>
    {/if}

    {#if hasReleases}
      <section class="section dx" aria-labelledby="dx-title">
        <div class="section-head">
          <h2 class="section-title" id="dx-title">
            Discography<span class="section-count tnum">{releaseTotal}</span>
          </h2>
          <button class="link-more" onclick={openDiscography}>
            See all<Icon name="fwd" size={12} />
          </button>
        </div>

        <div class="seg" role="tablist" aria-label="Release types">
          {#each segments as option (option.id)}
            <button
              class="seg-btn"
              class:on={group === option.id}
              role="tab"
              aria-selected={group === option.id}
              onclick={() => (group = option.id)}
            >
              {option.label}<span class="seg-n tnum">{option.count}</span>
            </button>
          {/each}
        </div>


        {#if visibleShelf.length}
          <!-- Exactly one row. Same card object as the library and search
               grids, and the same non-negotiable rule: the artwork OPENS the
               record, only the floating button plays it. -->
          <div class="shelf" style:--per-row={perRow}>
            {#each visibleShelf as entry (entry.release.id)}
              {@const release = entry.release}
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
                    disabled={!!busy}
                    onclick={() => playRelease(release.id)}
                  >
                    <Icon name={busy === release.id ? "more" : "play"} size={15} />
                  </button>
                </div>
                <button class="card-copy" onclick={() => navigate("album", release.id)}>
                  <span class="card-name">{release.name}</span>
                  <span class="card-sub"
                    >{[release.year || null, entry.kind].filter(Boolean).join(" · ")}</span
                  >
                </button>
              </div>
            {/each}
          </div>
          {#if shelf.length > visibleShelf.length}
            <!-- The count is the point: it says how much is behind the link,
                 so "See all" is an informed click rather than a guess. -->
            <button class="shelf-more" onclick={openDiscography}>
              {shelf.length - visibleShelf.length} more<Icon name="fwd" size={12} />
            </button>
          {/if}
        {:else}
          <p class="shelf-empty">
            The catalogue lists {counts[group] ?? 0} of these, but the artist payload
            carried no summaries for them. Open the full discography to read them.
          </p>
        {/if}
        {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
        {#if currentShelfError}
          <p class="inline-error" role="alert">{currentShelfError}</p>
          <button class="link-more" onclick={retryShelf}>Try again</button>
        {/if}
      </section>
    {/if}
    {#if artistPickPlaylist}
      <section class="section artist-pick" aria-labelledby="pick-title">
        <div class="section-head">
          <h2 class="section-title" id="pick-title">Artist Pick</h2>
        </div>
        <div class="artist-pick-card">
          {@render playlistCard(artistPickPlaylist)}
        </div>
      </section>
    {/if}

    {#if playlistShelves.artist.length}
      <section class="section" aria-labelledby="artist-playlists-title">
        <div class="section-head">
          <h2 class="section-title" id="artist-playlists-title">
            Artist playlists<span class="section-count tnum">{playlistShelves.artist.length}</span>
          </h2>
          {#if playlistShelves.artist.length > perRow}
            <button class="link-more" onclick={() => navigate("artist-playlists", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf playlist-shelf" style:--per-row={perRow}>
          {#each playlistShelves.artist.slice(0, perRow) as playlist (playlist.id)}
            {@render playlistCard(playlist)}
          {/each}
        </div>
      </section>
    {/if}

    {#if playlistShelves.discovered.length}
      <section class="section" aria-labelledby="discovered-title">
        <div class="section-head">
          <h2 class="section-title" id="discovered-title">
            Discovered on<span class="section-count tnum">{playlistShelves.discovered.length}</span>
          </h2>
          {#if playlistShelves.discovered.length > perRow}
            <button class="link-more" onclick={() => navigate("discovered-on", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf playlist-shelf" style:--per-row={perRow}>
          {#each playlistShelves.discovered.slice(0, perRow) as playlist (playlist.id)}
            {@render playlistCard(playlist)}
          {/each}
        </div>
      </section>
    {/if}

    {#if visibleRelatedArtists.length}
      <section class="section" aria-labelledby="related-title">
        <div class="section-head">
          <h2 class="section-title" id="related-title">Fans also like</h2>
          {#if relatedArtists.length > visibleRelatedArtists.length}
            <button class="link-more" onclick={() => navigate("fans-also-like", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf" style:--per-row={perRow}>
          {#each visibleRelatedArtists as related (related.id)}
            {@const tone = coverTone(related.cover_url, related.id)}
            <button
              class="card"
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
      </section>
    {/if}

    {#if appearsOnCount}
      <section class="section appears" aria-labelledby="appears-title">
        <div class="section-head">
          <h2 class="section-title" id="appears-title">
            Appears On<span class="section-count tnum">{appearsOnCount}</span>
          </h2>
          {#if appearsOnCount > perRow}
            <button class="link-more" onclick={() => navigate("appears-on", artist.id)}>
              See all<Icon name="fwd" size={12} />
            </button>
          {/if}
        </div>
        <div class="shelf appears-shelf" style:--per-row={perRow} bind:this={appearsOnSentinel}>
          {#if visibleAppearsOn.length}
            {#each visibleAppearsOn as release (release.id)}
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
                    disabled={!!busy}
                    onclick={() => playRelease(release.id)}
                  >
                    <Icon name={busy === release.id ? "more" : "play"} size={15} />
                  </button>
                </div>
                <button class="card-copy" onclick={() => navigate("album", release.id)}>
                  <span class="card-name">{release.name}</span>
                  <span class="card-sub">{[release.year || null, "Appears on"].filter(Boolean).join(" · ")}</span>
                </button>
              </div>
            {/each}
          {:else if appearsOnLoading}
            {#each Array.from({ length: perRow }) as _, i (i)}
              <div class="card" aria-hidden="true">
                <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
                <span class="card-copy">
                  <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
                  <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
                </span>
              </div>
            {/each}
          {:else}
            <p class="shelf-empty">Spotify lists {appearsOnCount} Appears On releases, but none are available yet.</p>
          {/if}
        </div>
        {#if appearsOnError}<p class="inline-error" role="alert">{appearsOnError}</p>{/if}
        {#if appearsOnError}<button class="link-more" onclick={retryAppearsOn}>Try again</button>{/if}
      </section>
    {/if}


    <section class="section about" aria-labelledby="about-title">
      <h2 class="section-title" id="about-title">About {artist.name}</h2>
      <div class="about-body">
        <div class="about-portrait">
          <Cover src={artist.cover_url} id={artist.id} name={artist.name} size={148} lg raised />
        </div>
        <div class="about-copy">
          {#if aboutStats.length || hasReleases}
            <dl class="about-stats">
              {#each aboutStats as fact (fact.label)}
                <div>
                  <dt class="tnum">{fact.value}</dt>
                  <dd class="caps">{fact.label}</dd>
                </div>
              {/each}
              {#each GROUPS.slice(1) as g (g.id)}
                {#if (counts[g.id] ?? 0) > 0}
                  <div>
                    <dt class="tnum">{NUMBER_FORMAT.format(counts[g.id])}</dt>
                    <dd class="caps">{g.label}</dd>
                  </div>
                {/if}
              {/each}
            </dl>
          {/if}
          {#if overview?.biography}
            <p class="biography">{overview.biography}</p>
          {/if}
          {#if topCities.length}
            <div class="top-cities">
              <h3 class="caps">Top listening cities</h3>
              <ol>
                {#each topCities as city (`${city.city}:${city.region}:${city.country}`)}
                  {@const place = [city.region, city.country].filter(Boolean).join(", ")}
                  <li>
                    <span>
                      <strong>{city.city}</strong>
                      {#if place}<small>{place}</small>{/if}
                    </span>
                    {#if city.listeners}
                      <span class="city-listeners tnum">{NUMBER_FORMAT.format(city.listeners)}</span>
                    {/if}
                  </li>
                {/each}
              </ol>
            </div>
          {/if}
        </div>
      </div>
    </section>

    {#if !top.length && !hasReleases && !appearsOnCount && !popularReleases.length && !relatedArtists.length && !artistPickPlaylist && !playlistShelves.artist.length && !playlistShelves.discovered.length}
      <div class="empty">
        <p class="h">Nothing to show for this artist.</p>
        <p class="sub">The engine returned no popular songs or releases.</p>
      </div>
    {/if}
  {/if}
</section>

<style>
  .banner-sk { position: absolute; inset: 0; background: var(--bg-2); }

  .dx { margin-top: var(--s8); }
  .dx .seg { margin-bottom: var(--s5); }
  .appears { margin-top: var(--s9); }

  /* One row, always. `repeat(auto-fill, …)` was not an option: it decides how
     many cards fit and then wraps the rest, and a shelf that wraps is a grid.
     The count comes from the pane instead (see `perRow`) and the tracks are
     equal fractions, so the row is flush with the page at every width and no
     card is ever half-drawn at the edge. */
  .shelf {
    display: grid; gap: var(--s5);
    grid-template-columns: repeat(var(--per-row, 5), minmax(0, 1fr));
  }
  .artist-pick-card { width: min(240px, 100%); }
  .playlist-shelf { margin-top: var(--s1); }
  /* The overflow, named. It sits under the shelf rather than beside the
     heading because it is about the row you just looked at. */
  .shelf-more {
    display: inline-flex; align-items: center; gap: var(--s1);
    margin-top: var(--s4); color: var(--fg-2); font-size: var(--t-12);
    transition: color var(--d1) var(--ease);
  }
  .shelf-more:hover { color: var(--accent); }
  .shelf-empty { max-width: 56ch; color: var(--fg-2); font-size: var(--t-12); }

  /* ---------------------------------------------------------------- about */
  .about { margin-top: var(--s9); }
  .about-body {
    display: grid; grid-template-columns: 148px minmax(0, 1fr);
    gap: var(--s6); align-items: start; margin-top: var(--s4);
  }
  /* Fixed track and a fixed tile: a portrait that never arrives costs the
     image and nothing else, and the copy beside it does not move. */
  .about-portrait { width: 148px; height: 148px; --tile: 148px; }
  .about-copy { min-width: 0; }
  .about-stats {
    display: flex; flex-wrap: wrap; gap: var(--s5) var(--s7); margin: 0 0 var(--s5);
  }
  .about-stats dt {
    font-family: var(--font-display); font-size: var(--t-32); font-weight: var(--w-bold);
    letter-spacing: -0.02em; line-height: 1; color: var(--fg);
  }
  .about-stats dd { margin: var(--s2) 0 0; }
  .biography {
    max-width: 72ch; margin: 0; padding-top: var(--s4); border-top: 1px solid var(--line);
    color: var(--fg-1); font-size: var(--t-13); line-height: 1.65; white-space: pre-line;
  }
  .top-cities { max-width: 620px; margin-top: var(--s5); }
  .top-cities h3 { margin: 0 0 var(--s2); color: var(--fg-2); }
  .top-cities ol {
    display: grid; grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0 var(--s6); margin: 0; padding: 0; list-style: none;
  }
  .top-cities li {
    display: flex; align-items: baseline; justify-content: space-between; gap: var(--s3);
    min-width: 0; padding: var(--s2) 0; border-bottom: 1px solid var(--line);
    color: var(--fg-1); font-size: var(--t-12);
  }
  .top-cities strong { font-weight: var(--w-medium); }
  .top-cities small { margin-left: var(--s2); color: var(--fg-3); font-size: var(--t-11); }
  .city-listeners { flex: none; color: var(--fg-2); font-size: var(--t-11); }

  @media (max-width: 720px) {
    .about-body { grid-template-columns: minmax(0, 1fr); }
    .top-cities ol { grid-template-columns: minmax(0, 1fr); }
  }
</style>
