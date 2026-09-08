<script>
  import {
    route,
    navigate,
    navigateArtist,
    library,
    libraryState,
    followed,
    loadFollowedArtists,
    api,
    playback,
  } from "../lib/state.svelte.js";
  import { trackDrag } from "../lib/dnd.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import LikedMark from "./LikedMark.svelte";

  let creating = $state(false);
  let newName = $state("");
  let field = $state(null);
  let libList = $state(null);
  let fadeTop = $state(false);
  let fadeBottom = $state(false);
  let filtering = $state(false);
  let filterQuery = $state("");
  let filterInput = $state(null);

  /**
   * Which collection the one library list is showing.
   *
   * Following used to be a pinned row here, and the owner's objection to that
   * was exact: a permanent row costs a playlist row for ever, for nine
   * artists you rarely open. So it is a MODE of the list that already exists
   * rather than anything new in the column — the switch lives in the head
   * beside Filter and New playlist, which is a row that was already there and
   * half empty. Nothing below it moves.
   */
  let showingArtists = $state(false);

  const filteredLibrary = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return library;
    return library.filter((playlist) => playlist.name?.toLocaleLowerCase().includes(query));
  });

  const filteredArtists = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return followed.artists;
    return followed.artists.filter((artist) =>
      artist.name?.toLocaleLowerCase().includes(query),
    );
  });

  /* The collection is fetched the first time it is actually asked for, never
     at startup: the whole argument for this switch is that Following costs
     nothing while you are not looking at it.

     Deliberately reactive on `loaded` rather than firing once on the switch.
     Signing into another account empties the collection and clears the flag,
     and the rail may well be sitting on artists when that happens — a list
     that went blank and stayed blank until you toggled twice would be the
     alternative. The loader's own guards make a repeat call free. */
  $effect(() => {
    if (showingArtists && !followed.loaded) loadFollowedArtists();
  });

  /* A song in flight has no destination among artists — the drop targets are
     playlists — so a drag that starts over an artist list would present a
     rail that silently refuses everything. Switch back and let the gesture
     find its targets. */
  $effect(() => {
    if (trackDrag.active) showingArtists = false;
  });

  /* Which collection the rail's scroll offset belongs to, so switching lists
     can be told apart from the list already on screen growing. A plain `let`:
     nothing renders from it. */
  let scrolledMode = false;

  $effect(() => {
    const query = filterQuery;
    const mode = showingArtists;
    filteredLibrary.length;
    filteredArtists.length;
    library.length;
    const list = libList;
    if (!list) return;

    /* A filtered collection is a new result set, not the old list at its old
       scroll offset, and so is the other collection entirely. Reset before
       measuring so both mask edges describe the rows that are actually
       visible now. */
    if (query || mode !== scrolledMode) list.scrollTop = 0;
    scrolledMode = mode;

    let frame = 0;
    const updateFades = () => {
      frame = 0;
      const maxScroll = Math.max(0, list.scrollHeight - list.clientHeight);
      fadeTop = list.scrollTop > 1;
      fadeBottom = list.scrollTop < maxScroll - 1;
    };
    const scheduleFades = () => {
      if (frame) return;
      frame = requestAnimationFrame(updateFades);
    };

    scheduleFades();
    list.addEventListener("scroll", scheduleFades, { passive: true });
    const resizeObserver = new ResizeObserver(scheduleFades);
    resizeObserver.observe(list);
    return () => {
      list.removeEventListener("scroll", scheduleFades);
      resizeObserver.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  });

  $effect(() => {
    if (creating) field?.focus();
  });

  $effect(() => {
    if (!filtering) return;
    queueMicrotask(() => filterInput?.focus());

    /* Clicking away dismisses an *empty* filter, the same as pressing Escape.
       A filter with text in it stays: the list on screen is the result of that
       text, so silently discarding it would leave the sidebar showing a subset
       with nothing to explain why. Those are only dismissed deliberately. */
    function onPointerDown(event) {
      if (filterQuery.trim()) return;
      if (filterInput?.parentElement?.contains(event.target)) return;
      closeFilter();
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  function startCreate() {
    /* A previous filter activation may have marked the editor cancelled. */
    filtering = false;
    filterQuery = "";
    creating = true;
  }

  /* A half-typed playlist name is not a name for the other collection, and the
     create field sits under a list that is about to become artists — which
     cannot hold a playlist. Switching abandons it, exactly as pointing at
     Filter does. (The filter needs no such call: it takes over the whole head
     row, so this switch is not on screen while one is open.) */
  function showLibrary(artists) {
    if (showingArtists === artists) return;
    cancelCreate();
    showingArtists = artists;
  }

  function cancelCreate() {
    creating = false;
    newName = "";
  }

  function startFilter() {
    cancelCreate();
    filtering = true;
  }

  function closeFilter() {
    filtering = false;
    filterQuery = "";
  }

  function commitCreate() {
    /* Pointer-down on Filter cancels before moving focus. Its ensuing blur
       must not resurrect the discarded partial name as a new playlist. */
    if (!creating) return;
    const name = newName.trim();
    creating = false;
    newName = "";
    if (name) api.createPlaylist(name).catch(() => {});
  }

  /** The badge is what will actually PLAY, not how many rows the queue holds:
      `queue.length` counts the rows the engine walks around too — excluded
      from automatic playback, unavailable — so it read 77 against a Queue view
      headed "68 songs up next". Same arithmetic as that header (QueueView's
      `upNext`), because two counts for one list is one count too many. */
  const upNextCount = $derived.by(() => {
    const planned = playback.upcoming?.length ?? 0;
    /* With nothing playing the view heads its list with the first planned row
       instead of a current track, so that one is not "up next" either. */
    return playback.current_index >= 0 ? planned : Math.max(0, planned - 1);
  });

  /** The playlist context of the current queue row, when it is a playlist. */
  const playingId = $derived.by(() => {
    const context = playback.queue[playback.current_index]?.context;
    if (typeof context !== "string") return null;
    const match = /^playlist:([^:]+)$/.exec(context);
    return match?.[1] ?? null;
  });
</script>

<aside class="sidebar">
  <nav class="nav">
    <button class="nav-item" class:active={route.name === "library"} onclick={() => navigate("library")}>
      <!-- The renderer's mark, where the outline house used to be. It came off
           an inert branding block above this nav; the rail is short enough that
           48px of it was worth a whole library row, and Home is the one
           destination that is also "the app", so the mark still says what it
           said before without costing a line. -->
      <span class="nav-mark"><Icon name="note" size={12} /></span><span>Home</span>
    </button>
    <button class="nav-item" class:active={route.name === "queue"} onclick={() => navigate("queue")}>
      <Icon name="queue" size={17} /><span>Queue</span>
      {#if upNextCount}<span class="nav-count">{upNextCount}</span>{/if}
    </button>
    <button class="nav-item" class:active={route.name === "history"} onclick={() => navigate("history")}>
      <Icon name="clock" size={17} /><span>History</span>
    </button>
  </nav>

  <div class="lib">
    <div class="lib-head">
      {#if filtering}
        <div class="lib-filter">
          <Icon name="search" size={13} />
          <input
            bind:this={filterInput}
            bind:value={filterQuery}
            aria-label={showingArtists ? "Filter followed artists" : "Filter library"}
            placeholder={showingArtists ? "Filter artists" : "Filter library"}
            spellcheck="false"
            onkeydown={(event) => event.key === "Escape" && closeFilter()}
          />
          <button class="lib-filter-close" title="Clear library filter" onclick={closeFilter}>
            <Icon name="x" size={11} />
          </button>
        </div>
      {:else}
        <!-- The switch IS the section label. Two words where one used to sit,
             so the head row costs exactly what it cost before and the list
             below never moves — which was the whole requirement. The selected
             one is the heading; the other is a quiet caps label you can press,
             the same weight the single "Library" always had. -->
        <div class="lib-modes" role="tablist" aria-label="Library collection">
          <button
            class="lib-mode"
            class:on={!showingArtists}
            role="tab"
            aria-selected={!showingArtists}
            onclick={() => showLibrary(false)}
          >
            Playlists
          </button>
          <button
            class="lib-mode"
            class:on={showingArtists}
            role="tab"
            aria-selected={showingArtists}
            onclick={() => showLibrary(true)}
          >
            Artists
          </button>
        </div>
        <div class="lib-head-actions">
          <button
            class="btn-icon"
            title={showingArtists ? "Filter followed artists" : "Filter library"}
            onpointerdown={cancelCreate}
            onclick={startFilter}
          >
            <Icon name="search" size={13} />
          </button>
          <!-- Only over playlists. There is nothing to create in the artist
               list — this app cannot follow an artist at all, and Spotify's
               own write for it is an internal protobuf service — so the slot
               goes back to the head rather than holding a control that would
               have to explain itself. -->
          {#if !showingArtists}
            <button class="btn-icon" title="New playlist" onclick={startCreate}>
              <Icon name="plus" size={14} />
            </button>
          {/if}
        </div>
      {/if}
    </div>

    {#if creating}
      <form
        class="lib-create"
        onsubmit={(e) => {
          e.preventDefault();
          commitCreate();
        }}
      >
        <input
          bind:this={field}
          bind:value={newName}
          placeholder="Playlist name"
          spellcheck="false"
          onblur={commitCreate}
          onkeydown={(e) => e.key === "Escape" && cancelCreate()}
        />
      </form>
    {/if}

    <div class="lib-list" class:fade-top={fadeTop} class:fade-bottom={fadeBottom} class:droppable={trackDrag.active} bind:this={libList}>
      {#if showingArtists}
        <!-- Portraits, in circles, because a face is most of how an artist is
             recognised and the rail already draws every artist that way. This
             is why the collection is worth being a list at all rather than a
             page of tiles: nine rows fit where nine cards would have needed a
             whole view, and each one opens the page that actually matters. -->
        {#each filteredArtists as artist (artist.id)}
          <button
            class="lib-row"
            class:active={route.name === "artist" && route.id === artist.id}
            onclick={() => navigateArtist(artist.id, artist.name)}
          >
            <Cover src={artist.cover_url} id={artist.id} name={artist.name} size={32} circle />
            <span class="lib-name">{artist.name}</span>
          </button>
        {/each}
        {#if followed.loading && !followed.artists.length}
          <!-- The same loading frame the playlists get, at the same row
               geometry with a round tile, so the list does not jump. -->
          {#each Array.from({ length: 6 }) as _, i (i)}
            <div class="lib-row" aria-hidden="true">
              <span class="skeleton" style="width:32px;height:32px;border-radius:50%"></span>
              <span class="skeleton line" style="width:{72 - ((i * 13) % 30)}%;height:11px;margin:0"></span>
            </div>
          {/each}
        {:else if followed.error}
          <p class="lib-filter-empty">
            {followed.error}
            <button class="link-more" onclick={() => loadFollowedArtists({ force: true })}>
              Try again
            </button>
          </p>
        {:else if filterQuery.trim() && !filteredArtists.length}
          <p class="lib-filter-empty">No matching artists</p>
        {:else if followed.loaded && !followed.artists.length}
          <p class="lib-filter-empty">You are not following any artists yet</p>
        {/if}
      {:else}
        <button
          class="lib-row liked-row"
          class:active={route.name === "liked"}
          onclick={() => navigate("liked")}
        >
          <!-- The same mark the collection page shows at 176px. It used to be a
               different picture here, drawn by different CSS. -->
          <LikedMark size={32} />
          <span class="lib-name">Liked Songs</span>
        </button>
        {#each filteredLibrary as pl (pl.id)}
          <button
            class="lib-row"
            class:active={route.name === "playlist" && route.id === pl.id}
            class:playing={playingId === pl.id}
            class:no-drop={trackDrag.active && trackDrag.sourcePlaylistId === pl.id}
            data-pid={pl.id}
            onclick={() => navigate("playlist", pl.id)}
          >
            <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} size={32} />
            <span class="lib-name">{pl.name}</span>
            {#if trackDrag.active}
              <!-- While a song is in flight the count cedes its slot to the
                   affordance: every user playlist is a valid destination (the
                   source itself wears .no-drop), and "+" says what a drop does
                   without a word of prose. -->
              <span class="lib-drop-hint" aria-hidden="true">+</span>
            {:else if pl.tracks_total}
              <span class="lib-count">{pl.tracks_total}</span>
            {/if}
          </button>
        {/each}
        {#if !libraryState.loaded && !library.length}
          <!-- The rail's own loading frame. Rows at the real height with the
               real tile and name geometry, so the list does not jump when the
               library lands — the rail used to sit empty under a lone Liked
               Songs row for the whole round trip. -->
          {#each Array.from({ length: 8 }) as _, i (i)}
            <div class="lib-row" aria-hidden="true">
              <span class="skeleton" style="width:32px;height:32px;border-radius:var(--r1)"></span>
              <span class="skeleton line" style="width:{78 - ((i * 13) % 34)}%;height:11px;margin:0"></span>
            </div>
          {/each}
        {:else if filterQuery.trim() && !filteredLibrary.length}
          <p class="lib-filter-empty">No matching playlists</p>
        {:else if libraryState.loaded && !library.length}
          <p class="lib-filter-empty">No playlists in your library yet</p>
        {/if}
      {/if}
    </div>
  </div>

  <button class="nav-item settings" class:active={route.name === "settings"} onclick={() => navigate("settings")}>
    <Icon name="settings" size={17} /><span>Settings</span>
  </button>
</aside>
