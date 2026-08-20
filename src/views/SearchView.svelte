<script>
  import { untrack } from "svelte";
  import { search, api, navigate, focusSearch, queueSearch } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  /* The query field lives in the topbar; this view only renders results. */

  const SKELETON_ROWS = Array.from({ length: 6 });

  const tracks = $derived(search.results?.tracks ?? []);
  const albums = $derived(search.results?.albums ?? []);
  const artists = $derived(search.results?.artists ?? []);
  const empty = $derived(!tracks.length && !albums.length && !artists.length);
  const visibleTracks = $derived(tracks.slice(0, 5));

  /* Top result: an artist beats an album beats a song. Whatever the query
     names exactly is almost always one of those, in that order. */
  const top = $derived.by(() => {
    /* `sub` is the credit line, empty for an artist — the kind already says it */
    if (artists[0]) return { kind: "Artist", ...artists[0], sub: "" };
    if (albums[0]) return { kind: "Album", ...albums[0], sub: albums[0].artist_names.join(", ") };
    if (tracks[0]) return { kind: "Song", ...tracks[0], sub: tracks[0].artist_names.join(", ") };
    return null;
  });

  function openTop() {
    if (!top) return;
    if (top.kind === "Artist") navigate("artist", top.id);
    else if (top.kind === "Album") navigate("album", top.id);
    else api.playQueue(tracks, 0).catch(() => {});
  }

  function playTrack(i) {
    if (tracks.length) api.playQueue(tracks, i).catch(() => {});
  }

  /* ---- Recent searches ------------------------------------------------
     Eight, newest first, in localStorage. Reading is wrapped because a
     webview with storage disabled throws on access rather than returning
     null, and a search page must not fail to render over a history list. */
  const RECENTS_KEY = "sr.recent-searches";
  const RECENTS_MAX = 8;

  function readRecents() {
    try {
      const raw = JSON.parse(localStorage.getItem(RECENTS_KEY) ?? "[]");
      if (!Array.isArray(raw)) return [];
      return raw.filter((s) => typeof s === "string" && s.trim()).slice(0, RECENTS_MAX);
    } catch {
      return [];
    }
  }

  let recents = $state(readRecents());

  function writeRecents(next) {
    recents = next.slice(0, RECENTS_MAX);
    try {
      localStorage.setItem(RECENTS_KEY, JSON.stringify(recents));
    } catch {
      /* private mode / storage disabled: the list is simply not persisted */
    }
  }

  function remember(q) {
    writeRecents([q, ...recents.filter((r) => r.toLowerCase() !== q.toLowerCase())]);
  }

  function forget(q) {
    writeRecents(recents.filter((r) => r !== q));
  }

  function rerun(q) {
    search.query = q;
    search.results = null;
    remember(q);
    queueSearch(q);
  }

  /* Record only queries that actually came back. `search.results` is the sole
     tracked read: everything else runs untracked, because `remember` both
     reads and writes `recents` and tracking that would make this effect
     re-trigger itself forever. */
  $effect(() => {
    if (!search.results) return;
    untrack(() => {
      const q = search.query.trim();
      if (q) remember(q);
    });
  });
</script>

<section class="view page">
  <div class="search-intro">
    <h1 class="page-title">
      {search.submitted && search.query ? `Results for “${search.query}”` : "Find something to play"}
    </h1>
  </div>

  {#if !search.submitted}
    {#if recents.length}
      <div class="section" style="margin-top:0">
        <div class="section-head">
          <h2 class="section-title">Recent searches</h2>
          <button class="link-more" onclick={() => writeRecents([])}>Clear all</button>
        </div>
        <div class="chips">
          {#each recents as q (q)}
            <span class="chip">
              <button class="label" onclick={() => rerun(q)}>{q}</button>
              <button class="x" title="Remove" onclick={() => forget(q)}>
                <Icon name="x" size={11} />
              </button>
            </span>
          {/each}
        </div>
      </div>
    {:else}
      <div class="empty">
        <p class="h">Nothing searched yet.</p>
        <p class="sub">Songs, albums and artists all come back from one query.</p>
        <div class="actions">
          <button class="btn-ghost" onclick={focusSearch}>
            <Icon name="search" size={14} />Search
          </button>
        </div>
      </div>
    {/if}
  {:else if !search.results}
    <!-- Static skeleton rows: a shimmer would animate for as long as the
         request takes and cost frames the whole time. -->
    <div class="tl no-album">
      {#each SKELETON_ROWS as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk a"></span>
          <span></span>
          <span class="sk b"></span>
          <span></span>
        </div>
      {/each}
    </div>
  {:else if empty}
    <div class="empty">
      <p class="h">No results for “{search.query}”.</p>
      <p class="sub">Check the spelling, or try a shorter query.</p>
      <div class="actions">
        <button class="btn-ghost" onclick={focusSearch}>
          <Icon name="search" size={14} />Try another search
        </button>
      </div>
    </div>
  {:else}
    <div class="search-split">
      {#if top}
        <div>
          <div class="section-head"><h2 class="section-title">Top result</h2></div>
          <button class="top-result" onclick={openTop}>
            <Cover
              src={top.cover_url}
              id={top.id}
              name={top.name}
              size={92}
              lg
              circle={top.kind === "Artist"}
            />
            <span style="min-width:0">
              <span class="tr-name">{top.name}</span>
              <span class="tr-sub">{top.sub ? `${top.kind} / ${top.sub}` : top.kind}</span>
            </span>
          </button>
        </div>
      {/if}

      {#if tracks.length}
        <div>
          <div class="section-head">
            <h2 class="section-title">Songs</h2>
            {#if tracks.length > 5}
              <!-- The larger request starts only after this explicit click.
                   Keeping it on its own route also gives long results the full pane. -->
              <button class="link-more" onclick={() => navigate("search-songs", search.query)}>See all</button>
            {/if}
          </div>
          <TrackList tracks={visibleTracks} playFrom={playTrack} showAlbum={false} showHead={false} />
        </div>
      {/if}
    </div>

    {#if albums.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Albums</h2></div>
        <div class="grid">
          {#each albums as al (al.id)}
            <button class="card" onclick={() => navigate("album", al.id)}>
              <span class="card-art">
                <Cover src={al.cover_url} id={al.id} name={al.name} fill lg />
                <span class="card-play"><Icon name="play" size={15} /></span>
              </span>
              <span class="card-name">{al.name}</span>
              <span class="card-sub">{#if al.year}{al.year} · {/if}{al.artist_names.join(", ")}</span>
            </button>
          {/each}
        </div>
      </div>
    {/if}

    {#if artists.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Artists</h2></div>
        <div class="grid">
          {#each artists as ar (ar.id)}
            <button class="card" onclick={() => navigate("artist", ar.id)}>
              <span class="card-art">
                <Cover src={ar.cover_url} id={ar.id} name={ar.name} fill circle />
              </span>
              <span class="card-name">{ar.name}</span>
              <span class="card-sub">Artist</span>
            </button>
          {/each}
        </div>
      </div>
    {/if}
  {/if}
</section>
