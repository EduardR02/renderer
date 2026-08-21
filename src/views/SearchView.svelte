<script>
  import { untrack } from "svelte";
  import { search, api, navigate, focusSearch, queueSearch, retrySearch } from "../lib/state.svelte.js";
  import { playAlbumById, playPlaylistById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  /* The query field lives in the topbar; this view only renders results. */

  /* Five, because five is what the loaded page shows (`visibleTracks`). A
     loading state with the wrong row count is a loading state that reflows. */
  const SKELETON_ROWS = Array.from({ length: 5 });

  const tracks = $derived(search.results?.tracks ?? []);
  const albums = $derived(search.results?.albums ?? []);
  const playlists = $derived(search.results?.playlists ?? []);
  const artists = $derived(search.results?.artists ?? []);
  const empty = $derived(!tracks.length && !albums.length && !playlists.length && !artists.length);
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

  let busy = $state("");
  let playError = $state("");

  /* The card opens; only this button plays. A result card knows an id and
     nothing else, so playing it costs one browse first. */
  async function play(id, load, failure) {
    if (busy) return;
    busy = id;
    playError = "";
    try {
      await load(id);
    } catch (reason) {
      playError = String(reason || failure);
    } finally {
      busy = "";
    }
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
    <!-- Refining a query keeps the previous results on screen, which is right
         — but then nothing on the page says a newer answer is coming. One
         quiet line does, and it occupies a fixed slot so the results below it
         do not shift when it appears. -->
    <p class="search-status" class:on={search.busy && !!search.results}>Searching…</p>
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
        <p class="sub">Songs, albums, playlists and artists all come back from one query.</p>
        <div class="actions">
          <button class="btn-ghost" onclick={focusSearch}>
            <Icon name="search" size={14} />Search
          </button>
        </div>
      </div>
    {/if}
  {:else if search.error && !search.results}
    <div class="empty failed" role="alert">
      <p class="h">Search couldn't load.</p>
      <p class="why">{search.error}</p>
      <div class="actions">
        <button class="btn-ghost" onclick={retrySearch}>Try again</button>
      </div>
    </div>
  {:else if !search.results}
    <!-- =================================================================
         The frame the results arrive into.

         This used to be six bare rows in a one-column strip, which shares no
         geometry at all with what actually lands — a two-column split with a
         top-result panel beside five songs, then grids of albums, playlists
         and artists. So every search visibly rebuilt the page: the strip
         vanished, four sections appeared, and the whole thing jumped.

         Now the wait IS the page, with its content missing: same split, same
         panel, same row heights, same card grid. Nothing moves when the
         payload lands, which is the only thing a loading state is actually
         for. Static, deliberately — a shimmer animates for exactly as long as
         the request takes, which is the idle cost this app exists to avoid.
         ================================================================= -->
    <div class="search-split" aria-busy="true" aria-label="Searching">
      <div>
        <div class="section-head"><h2 class="section-title">Top result</h2></div>
        <div class="top-result sk-top">
          <span class="skeleton" style="width:92px;height:92px;border-radius:var(--r3)"></span>
          <span style="min-width:0;display:block">
            <span class="skeleton line" style="width:74%;height:24px;margin:0"></span>
            <span class="skeleton line" style="width:44%;height:12px;margin:10px 0 0"></span>
          </span>
        </div>
      </div>
      <div>
        <div class="section-head"><h2 class="section-title">Songs</h2></div>
        <div class="tl" style="--cols:28px 36px minmax(0,1fr) 52px">
          {#each SKELETON_ROWS as _, i (i)}
            <div class="sk-row">
              <span class="sk" style="width:12px"></span>
              <span class="sk art"></span>
              <span class="sk-stack">
                <span class="sk a" style="width:{68 - ((i * 9) % 26)}%"></span>
                <span class="sk b" style="width:{34 - ((i * 5) % 12)}%"></span>
              </span>
              <span class="sk" style="width:28px;justify-self:end"></span>
            </div>
          {/each}
        </div>
      </div>
    </div>

    {#each ["Playlists", "Albums", "Artists"] as heading (heading)}
      <div class="section" aria-hidden="true">
        <div class="section-head"><h2 class="section-title">{heading}</h2></div>
        <div class="grid">
          {#each Array.from({ length: 6 }) as _, i (i)}
            <div class="card">
              <span
                class="skeleton"
                style="display:block;aspect-ratio:1;width:100%;border-radius:{heading === 'Artists'
                  ? 'var(--rf)'
                  : 'var(--r3)'}"
              ></span>
              <span class="card-copy">
                <span class="skeleton line" style="width:{72 - ((i * 13) % 28)}%;height:12px;margin:0"></span>
                <span class="skeleton line" style="width:40%;height:10px;margin:6px 0 0"></span>
              </span>
            </div>
          {/each}
        </div>
      </div>
    {/each}
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
    {#if search.error}
      <p class="inline-error" role="status">
        {search.error}
        <button class="link-more" onclick={retrySearch}>Try again</button>
      </p>
    {/if}
  {:else}
    <div class="search-split">
      {#if top}
        {@const topTone = coverTone(top.cover_url, top.id)}
        <div>
          <div class="section-head"><h2 class="section-title">Top result</h2></div>
          <!-- The one big coloured object on a results page. Search has no
               subject of its own to take a header wash from, but the top
               result IS a subject, so the panel around it takes that record's
               colour rather than being the fourth grey rectangle on screen. -->
          <button class="top-result" style:--tone-wash={topTone.wash} onclick={openTop}>
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
          <TrackList
            tracks={visibleTracks}
            playFrom={playTrack}
            showAlbum={false}
            showHead={false}
            disableWindowing
          />
        </div>
      {/if}
    </div>
    {#if search.error}
      <p class="inline-error" role="status">
        {search.error}
        <button class="link-more" onclick={retrySearch}>Try again</button>
      </p>
    {/if}

    {#if playlists.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Playlists</h2></div>
        <div class="grid">
          {#each playlists as pl (pl.id)}
            {@const tone = coverTone(pl.cover_url || pl.cover_urls?.[0] || "", pl.id)}
            <div class="card" style:--tone-glow={tone.glow}>
              <div class="card-art">
                <Cover
                  src={pl.cover_url}
                  srcs={pl.cover_urls ?? []}
                  id={pl.id}
                  name={pl.name}
                  fill
                  lg
                />
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
                  onclick={() => play(pl.id, playPlaylistById, "Could not play this playlist.")}
                >
                  <Icon name={busy === pl.id ? "more" : "play"} size={15} />
                </button>
              </div>
              <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
                <span class="card-name">{pl.name}</span>
                <span class="card-sub"
                  >{[
                    pl.tracks_total ? `${pl.tracks_total} songs` : null,
                    pl.owner ? `By ${pl.owner}` : null,
                  ]
                    .filter(Boolean)
                    .join(" · ") || "Playlist"}</span
                >
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if albums.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Albums</h2></div>
        <div class="grid">
          {#each albums as al (al.id)}
            {@const tone = coverTone(al.cover_url, al.id)}
            <div class="card" style:--tone-glow={tone.glow}>
              <div class="card-art">
                <Cover src={al.cover_url} id={al.id} name={al.name} fill lg />
                <button
                  class="card-open"
                  aria-label={`Open ${al.name}`}
                  onclick={() => navigate("album", al.id)}
                ></button>
                <button
                  class="card-play"
                  aria-label={`Play ${al.name}`}
                  title={`Play ${al.name}`}
                  disabled={!!busy}
                  onclick={() => play(al.id, playAlbumById, "Could not play this release.")}
                >
                  <Icon name={busy === al.id ? "more" : "play"} size={15} />
                </button>
              </div>
              <button class="card-copy" onclick={() => navigate("album", al.id)}>
                <span class="card-name">{al.name}</span>
                <!-- One expression, not two adjacent ones: `{year} · {names}`
                     across an {#if} loses the space after the middot, which is
                     why these read as "2019 ·Ceramic Hands". -->
                <span class="card-sub"
                  >{[al.year || null, al.artist_names.join(", ")].filter(Boolean).join(" · ")}</span
                >
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}

    {#if artists.length}
      <div class="section">
        <div class="section-head"><h2 class="section-title">Artists</h2></div>
        <div class="grid">
          {#each artists as ar (ar.id)}
            {@const tone = coverTone(ar.cover_url, ar.id)}
            <!-- No play button here, and that is not an omission: an artist is
                 not a record. The whole card opens, which is the same rule the
                 others follow with the exception removed. -->
            <button class="card" style:--tone-glow={tone.glow} onclick={() => navigate("artist", ar.id)}>
              <span class="card-art">
                <Cover src={ar.cover_url} id={ar.id} name={ar.name} fill circle />
              </span>
              <span class="card-copy">
                <span class="card-name">{ar.name}</span>
                <span class="card-sub">Artist</span>
              </span>
            </button>
          {/each}
        </div>
      </div>
    {/if}
  {/if}
</section>
