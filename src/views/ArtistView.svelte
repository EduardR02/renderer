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
  const RELEASE_PREVIEW = 8;
  const expandedSections = $state({});

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
        <span class="sep">/</span><span class="num">{releaseSections.reduce((total, section) => total + section.items.length, 0)} releases</span>
      {/if}
    </p>

    <div class="actions">
      <button class="play-lg" title="Play" onclick={() => playFrom(0)} disabled={!top.length}>
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

    {#each releaseSections as section (section.key)}
      {#if section.items.length}
        <div class="section" data-release-group={section.key}>
          <div class="section-head">
            <h2 class="section-title">
              {section.title}<span class="section-count">{section.items.length}</span>
            </h2>
            {#if section.items.length > RELEASE_PREVIEW}
              <button
                class="link-more"
                onclick={() => (expandedSections[section.key] = !expandedSections[section.key])}
              >{expandedSections[section.key] ? "Show less" : "See all"}</button>
            {/if}
          </div>
          <div class="grid">
            {#each section.items.slice(0, expandedSections[section.key] ? undefined : RELEASE_PREVIEW) as al (al.id)}
              <button class="card" onclick={() => navigate("album", al.id)}>
                <span class="card-art">
                  <Cover src={al.cover_url} id={al.id} name={al.name} fill lg />
                  <span class="card-play"><Icon name="play" size={15} /></span>
                </span>
                <span class="card-name">{al.name}</span>
                <span class="card-sub">
                  {#if al.year}{al.year} · {/if}{al.artist_names.join(", ")}
                </span>
              </button>
            {/each}
          </div>
        </div>
      {/if}
    {/each}

    {#if !top.length && !hasReleases}
      <div class="empty">
        <p class="h">Nothing to show for this artist.</p>
        <p class="sub">The engine returned no popular songs or releases.</p>
      </div>
    {/if}
  {/if}
</section>
