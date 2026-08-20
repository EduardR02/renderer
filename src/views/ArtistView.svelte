<script>
  import {
    detail,
    api,
    navigate,
    loadCataloguePage,
    playCatalogueContext,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import Select from "../components/Select.svelte";
  import { formatTotal } from "../lib/time.js";

  /**
   * The artist page, and the whole discography, in one place.
   *
   * There used to be two. This page showed four one-row SHELVES of cover cards
   * — Albums, Singles & EPs, Compilations, Appears on — each with its own
   * "Show all"; a second route showed a chip-filtered reader with the tracks.
   * So "the artist's albums" lived in two views with two different shapes, two
   * different sort behaviours, and five separate ways to get between them.
   *
   * Now there is one toggle and one list under it. Choosing a release type
   * swaps what is below it; choosing "All" reads the catalogue end to end,
   * which is also the only selection in which playback runs ACROSS releases —
   * start a track and it keeps going into the next record. That behaviour is
   * the reason the reader exists at all, so it is what the default lands on.
   *
   * Cards are gone with the shelves. A card is for recognising a record you
   * have not opened; here the record is open — cover, year, length and every
   * track — so the card would have been a lid on a box already unpacked.
   */
  const artist = $derived(detail.artist);
  const top = $derived(artist?.top_tracks ?? []);
  const counts = $derived(artist?.release_counts ?? {});

  /**
   * The toggle. `types` is the exact array the engine pages on, so a selection
   * is not a client-side filter over a fetched list — it is a different query,
   * and "Compilations" costs one page of compilations rather than a walk
   * through everything looking for them.
   */
  const GROUPS = [
    { id: "all", label: "All", types: ["albums", "singles", "compilations", "appears_on"] },
    { id: "albums", label: "Albums", types: ["albums"] },
    { id: "singles", label: "Singles & EPs", types: ["singles"] },
    { id: "compilations", label: "Compilations", types: ["compilations"] },
    { id: "appears_on", label: "Appears on", types: ["appears_on"] },
  ];

  const releaseTotal = $derived(
    ["albums", "singles", "compilations", "appears_on"].reduce((sum, k) => sum + (counts[k] ?? 0), 0),
  );
  const hasReleases = $derived(releaseTotal > 0);

  /** Empty groups are not offered: a dead tab is a worse answer than no tab. */
  const segments = $derived(
    GROUPS.filter((g) => g.id === "all" || (counts[g.id] ?? 0) > 0).map((g) => ({
      ...g,
      count: g.id === "all" ? releaseTotal : (counts[g.id] ?? 0),
    })),
  );

  let group = $state("all");
  const releaseTypes = $derived(GROUPS.find((g) => g.id === group)?.types ?? GROUPS[0].types);

  /**
   * Order applies to what is LOADED, and the footer says so. The catalogue
   * arrives four releases at a time, so "oldest first" is an honest ordering of
   * what you can see rather than a claim about the whole discography.
   */
  const SORTS = [
    { value: "catalogue", label: "Catalogue order" },
    { value: "newest", label: "Newest first" },
    { value: "oldest", label: "Oldest first" },
    { value: "az", label: "A – Z" },
  ];
  let sort = $state("catalogue");

  let releases = $state([]);
  let nextOffset = $state(0);
  let total = $state(0);
  let loading = $state(false);
  let error = $state("");
  let sentinel = $state(null);
  /** Identity of the list currently loaded, so a group or artist change refills. */
  let loadedKey = "";
  /**
   * The sentinel can be on screen before the first batch renders, which would
   * otherwise chain-load the whole catalogue on open. Only a real scroll arms
   * it.
   */
  let armed = false;

  async function loadNext(key = `${artist?.id ?? ""}:${group}`) {
    const id = artist?.id;
    if (!id || loading || nextOffset == null) return;
    const offset = nextOffset;
    loading = true;
    error = "";
    try {
      const page = await loadCataloguePage(id, releaseTypes, offset, 4);
      if (`${artist?.id}:${group}` !== key) return;
      const known = new Set(releases.map((release) => release.id));
      for (const release of page?.releases ?? []) {
        if (known.has(release.id)) continue;
        releases.push(release);
        known.add(release.id);
      }
      total = page?.total ?? total;
      nextOffset = page?.next_offset ?? null;
      armed = false;
    } catch (reason) {
      error = String(reason || "Could not load more of this catalogue.");
    } finally {
      if (`${artist?.id}:${group}` === key) loading = false;
    }
  }

  /**
   * The display order. Every entry keeps the index it has in `releases`,
   * because that array is the playback context: the flat track index the queue
   * is built from is a walk of the LOADED order, and re-sorting it for reading
   * must not silently re-sort what "play from here" means.
   */
  const ordered = $derived.by(() => {
    const items = releases.map((release, index) => ({ release, index }));
    if (sort === "catalogue") return items;
    if (sort === "az") {
      return items.sort((a, b) =>
        a.release.name.localeCompare(b.release.name, undefined, { sensitivity: "base" }),
      );
    }
    // Releases with no year sink to the bottom of either direction rather than
    // pretending to be from year zero.
    const dir = sort === "oldest" ? 1 : -1;
    return items.sort((a, b) => {
      if (!a.release.year && !b.release.year) return 0;
      if (!a.release.year) return 1;
      if (!b.release.year) return -1;
      return (a.release.year - b.release.year) * dir;
    });
  });

  /** Flat index of a track across every release loaded so far. */
  function playFrom(releaseIndex, trackIndex) {
    let index = trackIndex;
    for (let i = 0; i < releaseIndex; i++) index += releases[i].tracks.length;
    playCatalogueContext(releases, artist.id, releaseTypes, nextOffset, index).catch(() => {});
  }

  function playRelease(releaseIndex) {
    playFrom(releaseIndex, 0);
  }

  function playTop(i) {
    if (top.length) api.playQueue(top, i).catch(() => {});
  }

  function shuffleTop() {
    if (!top.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(top, 0).catch(() => {});
  }

  $effect(() => {
    const key = `${artist?.id ?? ""}:${group}`;
    if (!artist?.id || key === loadedKey) return;
    loadedKey = key;
    releases = [];
    nextOffset = 0;
    total = 0;
    error = "";
    armed = false;
    queueMicrotask(() => loadNext(key));
  });

  $effect(() => {
    const node = sentinel;
    if (!node) return;
    const scroller = node.closest(".scroll");
    if (!scroller) return;
    const onScroll = () => {
      armed = true;
    };
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting && armed) loadNext();
      },
      { root: scroller, rootMargin: "0px 0px 400px 0px" },
    );
    scroller.addEventListener("scroll", onScroll, { passive: true });
    observer.observe(node);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  });

  /* The payload does not label a release, so the track count does. Spotify's
     own rule, and it is right often enough to be useful. */
  function releaseKind(release) {
    const count = release.tracks?.length ?? 0;
    if (count === 1) return "Single";
    if (count <= 6) return "EP";
    return "Album";
  }

  /** Everyone credited on the release who is not the artist whose page this is. */
  function guests(release) {
    return (release.artist_names ?? []).filter((name) => name && name !== artist?.name);
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
      {/if}
    </div>
    <span class="tag">Artist</span>
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
      <button class="play-lg" title="Play popular songs" onclick={() => playTop(0)} disabled={!top.length}>
        <Icon name="play" size={19} />
      </button>
      <button class="btn-ghost" onclick={shuffleTop} disabled={!top.length}>
        <Icon name="shuffle" size={14} />Shuffle
      </button>
    </div>

    {#if top.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Popular</h2></div>
        <!-- resetsScroll: this list is one section of a longer page, so it must
             not yank the shared pane scroller when it mounts. -->
        <TrackList tracks={top} playFrom={playTop} showAlbum={false} showPlays resetsScroll={false} />
      </div>
    {/if}

    {#if hasReleases}
      <section class="section dx" aria-labelledby="dx-title">
        <!-- No heading under the tag. The selected segment below already names
             what is on screen, and "Discography / Every release / All 37" is
             the same fact stated three times. The rule is the opener. -->
        <div class="dx-head">
          <span class="tag" id="dx-title">Discography</span>
          <hr class="rule-accent" />
        </div>

        <!-- One row: what to read on the left, in what order on the right.
             The toggle is sticky under the topbar, because on a discography
             several screens long the control that changes what you are reading
             has to still be reachable while you are reading it. -->
        <div class="dx-controls">
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
          <Select
            options={SORTS}
            value={sort}
            label="Sort releases"
            onchange={(value) => (sort = value)}
          />
        </div>

        {#if group === "all"}
          <!-- The one thing the reader can do that nothing else in the app can,
               said once, where the behaviour is. -->
          <p class="dx-note">
            <Icon name="queue" size={13} />Playing anything here runs on through the
            rest of the catalogue.
          </p>
        {/if}

        {#each ordered as entry (entry.release.id)}
          {@const release = entry.release}
          {@const others = guests(release)}
          <article class="release">
            <div class="release-side">
              <!-- Same rule as every other cover in the app: the artwork OPENS
                   the record, the button on it plays. `.release-open` is a
                   transparent button over the tile rather than a button around
                   it, because a button cannot contain a button. -->
              <div class="release-art">
                <Cover src={release.cover_url} id={release.id} name={release.name} size={148} lg raised />
                <button
                  class="release-open"
                  aria-label={`Open ${release.name}`}
                  title={`Open ${release.name}`}
                  onclick={() => navigate("album", release.id)}
                ></button>
                <button
                  class="release-play"
                  aria-label={`Play ${release.name}`}
                  title={`Play ${release.name}`}
                  onclick={() => playRelease(entry.index)}
                >
                  <Icon name="play" size={16} />
                </button>
              </div>
            </div>
            <div class="release-body">
              <h3>
                <button class="release-name" onclick={() => navigate("album", release.id)}>
                  {release.name}
                </button>
              </h3>
              <p class="release-meta">
                <span class="kind">{releaseKind(release)}</span>
                {#if release.year}<span class="num">{release.year}</span>{/if}
                <span class="sep">/</span><span class="num">{release.tracks.length} songs</span>
                {#if release.tracks.length}
                  <span class="sep">/</span><span class="num">{formatTotal(release.tracks)}</span>
                {/if}
                {#if others.length}
                  <span class="sep">/</span><span class="release-with">with {others.join(", ")}</span>
                {/if}
              </p>
              <TrackList
                tracks={release.tracks}
                playFrom={(trackIndex) => playFrom(entry.index, trackIndex)}
                showAlbum={false}
                showArt={false}
                showHead={false}
                showPlays
                resetsScroll={false}
              />
            </div>
          </article>
        {/each}

        <div class="dx-foot" bind:this={sentinel}>
          {#if loading}
            <span class="dx-status">Loading releases…</span>
          {:else if nextOffset != null}
            <!-- The observer does this on approach; the button is the keyboard
                 and failed-observer path, not the primary affordance. -->
            <button class="btn-ghost" onclick={() => loadNext()}>
              Load more<Icon name="chevron-down" size={14} />
            </button>
          {:else if releases.length}
            <span class="dx-status">
              <span class="tnum">{releases.length}</span>{total && total !== releases.length
                ? ` of ${total}`
                : ""} releases
            </span>
          {/if}
        </div>
        {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
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
  /* ----------------------------------------------------------- discography */
  .dx { margin-top: var(--s9); }
  .dx-head { display: flex; align-items: center; gap: var(--s4); }

  /* Sticky, and directly under the topbar. A discography is several screens
     long; a filter that scrolls away is a filter you have to scroll back to. */
  .dx-controls {
    position: sticky; top: var(--topbar-h); z-index: 15;
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4) var(--s5); flex-wrap: wrap;
    margin-top: var(--s4); padding: var(--s3) 0;
    background: var(--bg-1);
  }
  /* `space-between` puts a lone item at flex-START on a wrapped line, so on a
     narrow pane the order control dropped to the left under the toggle rather
     than staying opposite it. `margin-left: auto` is right in both cases. */
  .dx-controls > :global(.sel) { margin-left: auto; }

  .dx-note {
    display: flex; align-items: center; gap: var(--s2);
    margin-top: var(--s2); padding-bottom: var(--s2);
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
  }
  .dx-note :global(svg) { color: var(--accent); flex: none; }

  /* Cover and tracks side by side: the release stays one readable object as
     you scroll past it, instead of a header that scrolls away from its list.
     148px matches the About portrait and is the smallest size at which sleeve
     typography is still readable. */
  .release {
    display: grid; grid-template-columns: 148px minmax(0, 1fr); gap: var(--s6);
    padding: var(--s6) 0; border-top: 1px solid var(--line);
  }
  /* The sticky cover shares a scroller with the sticky topbar AND the sticky
     toggle above it, so its offset is measured from the bottom of both:
     34px of control plus 2 x 12px of padding, plus a little air. */
  .release-side {
    position: sticky; top: calc(var(--topbar-h) + 66px); align-self: start;
  }
  .release-art { position: relative; width: 148px; height: 148px; border-radius: var(--r3); }
  .release-open {
    position: absolute; inset: 0; z-index: 1;
    border-radius: var(--r3); background: none;
  }
  .release-open:focus-visible { outline-offset: 3px; }
  .release-play {
    position: absolute; right: var(--s2); bottom: var(--s2); z-index: 2;
    width: 34px; height: 34px; border-radius: var(--rf);
    display: grid; place-items: center;
    background: var(--accent); color: var(--accent-ink);
    box-shadow: 0 8px 18px -4px rgba(0, 0, 0, 0.7);
    opacity: 0; transform: translateY(4px);
    transition: opacity var(--d1) var(--ease), transform var(--d2) var(--ease),
                background-color var(--d1) var(--ease);
  }
  .release-art:hover .release-play,
  .release-play:focus-visible { opacity: 1; transform: none; }
  .release-play:hover { background: var(--accent-hi); }

  .release-body h3 { margin: 0; }
  .release-name {
    max-width: 100%; text-align: left;
    font-family: var(--font-display);
    font-size: var(--t-20); font-weight: var(--w-bold);
    letter-spacing: -0.015em; line-height: 1.2; color: var(--fg);
    transition: color var(--d1) var(--ease);
  }
  .release-name:hover { color: var(--accent); }
  .release-meta {
    margin: var(--s1) 0 var(--s3);
    font-size: var(--t-12); color: var(--fg-2);
    display: flex; align-items: center; gap: var(--s2); flex-wrap: wrap;
  }
  .release-meta .sep { color: var(--fg-3); }
  .release-with { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .dx-foot { min-height: 72px; display: grid; place-items: center; padding: var(--s4) 0; }
  .dx-status { font-size: var(--t-12); color: var(--fg-2); }

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
    .release { grid-template-columns: 1fr; }
    .release-side { position: static; }
    .about-body { grid-template-columns: minmax(0, 1fr); }
  }
</style>
