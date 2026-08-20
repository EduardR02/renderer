<script>
  import { detail, api, navigate, ui, retryDetail } from "../lib/state.svelte.js";
  import { playAlbumById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { GROUPS, RELEASE_KEYS } from "../lib/discography.js";

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
   * "Show all", which asked the same question four times. The segmented toggle
   * asks it once and swaps what is under it, and the selection travels with
   * "See all" so the reader opens on what you were looking at.
   *
   * Every release on the shelf comes out of the artist payload, which already
   * carries per-type summaries. Switching segments costs nothing.
   */
  const artist = $derived(detail.artist);
  const top = $derived(artist?.top_tracks ?? []);
  const counts = $derived(artist?.release_counts ?? {});
  const groupLists = $derived(artist?.releases ?? {});

  const releaseTotal = $derived(RELEASE_KEYS.reduce((sum, k) => sum + (counts[k] ?? 0), 0));
  const hasReleases = $derived(releaseTotal > 0);

  /** Empty groups are not offered: a dead tab is a worse answer than no tab. */
  const segments = $derived(
    GROUPS.filter((g) => g.id === "all" || (counts[g.id] ?? 0) > 0).map((g) => ({
      ...g,
      count: g.id === "all" ? releaseTotal : (counts[g.id] ?? 0),
    })),
  );

  let group = $state("all");

  /* The artist changed under us — a link from a track row, say. Start the new
     artist on "All" rather than on whatever segment the last one was left on,
     which may not even exist here. */
  let seenArtist = null;
  $effect(() => {
    if (artist?.id === seenArtist) return;
    seenArtist = artist?.id ?? null;
    group = "all";
  });

  /** The label for a release's own group, used as its subtext on the shelf. */
  const KIND_LABEL = {
    albums: "Album",
    singles: "Single or EP",
    compilations: "Compilation",
    appears_on: "Appears on",
  };

  /**
   * What the shelf shows for the current segment.
   *
   * A named segment shows that group in the order the engine returned it,
   * which is catalogue order. "All" is the exception and cannot be: a
   * concatenation of four lists would be eleven albums followed by whatever
   * fits, so it is the most recent releases across every type instead — which
   * is what a highlight row on an artist page is for.
   */
  const shelf = $derived.by(() => {
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

  /* A shelf card knows an id and nothing else, so playing it costs one browse
     first — the shared rule for every card in the app (see lib/play.js). */
  let busy = $state("");
  let playError = $state("");
  async function playRelease(id) {
    if (busy) return;
    busy = id;
    playError = "";
    try {
      await playAlbumById(id);
    } catch (reason) {
      playError = String(reason || "Could not play this release.");
    } finally {
      busy = "";
    }
  }
</script>

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
          <button class="link-more" onclick={() => navigate("discography", artist.id, group)}>
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
            <button class="shelf-more" onclick={() => navigate("discography", artist.id, group)}>
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
      </section>
    {/if}

    <!-- About. The engine returns no biography for an artist — there is no
         such field anywhere between librespot's ARTIST_V4 and the frontend —
         so rather than inventing prose, the slot holds the facts that ARE
         real (the release totals, which are true totals and not page counts)
         and says plainly that the text is missing. Wiring a biography later
         is a matter of filling the empty column, not building the section. -->
    <section class="section about" aria-labelledby="about-title">
      <h2 class="section-title" id="about-title">About {artist.name}</h2>
      <div class="about-body">
        <div class="about-portrait">
          <Cover src={artist.cover_url} id={artist.id} name={artist.name} size={148} lg raised />
        </div>
        <div class="about-copy">
          {#if hasReleases}
            <dl class="about-stats">
              {#each GROUPS.slice(1) as g (g.id)}
                {#if (counts[g.id] ?? 0) > 0}
                  <div>
                    <dt class="tnum">{counts[g.id]}</dt>
                    <dd class="caps">{g.label}</dd>
                  </div>
                {/if}
              {/each}
            </dl>
          {/if}
          <p class="about-empty">
            No biography yet — the engine does not return one for an artist, so
            there is nothing here to show.
          </p>
        </div>
      </div>
    </section>

    {#if !top.length && !hasReleases}
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

  /* One row, always. `repeat(auto-fill, …)` was not an option: it decides how
     many cards fit and then wraps the rest, and a shelf that wraps is a grid.
     The count comes from the pane instead (see `perRow`) and the tracks are
     equal fractions, so the row is flush with the page at every width and no
     card is ever half-drawn at the edge. */
  .shelf {
    display: grid; gap: var(--s5);
    grid-template-columns: repeat(var(--per-row, 5), minmax(0, 1fr));
  }
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
  .about-empty {
    max-width: 58ch; padding-top: var(--s4); border-top: 1px solid var(--line);
    color: var(--fg-2); font-size: var(--t-13); line-height: 1.5;
  }

  @media (max-width: 720px) {
    .about-body { grid-template-columns: minmax(0, 1fr); }
  }
</style>
