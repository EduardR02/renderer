<script>
  import { route, navigate, library, libraryState, api, playback } from "../lib/state.svelte.js";
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

  const filteredLibrary = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    if (!query) return library;
    return library.filter((playlist) => playlist.name?.toLocaleLowerCase().includes(query));
  });

  $effect(() => {
    const query = filterQuery;
    filteredLibrary.length;
    library.length;
    const list = libList;
    if (!list) return;

    /* A filtered collection is a new result set, not the old list at its old
       scroll offset. Reset before measuring so both mask edges describe the
       rows that are actually visible after this query. */
    if (query) list.scrollTop = 0;

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

  /** The playlist the current track came from, so its row can be marked. */
  const playingId = $derived(playback.queue[playback.current_index]?.album_id ?? null);
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
      {#if playback.queue.length}<span class="nav-count">{playback.queue.length}</span>{/if}
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
            aria-label="Filter library"
            placeholder="Filter library"
            spellcheck="false"
            onkeydown={(event) => event.key === "Escape" && closeFilter()}
          />
          <button class="lib-filter-close" title="Clear library filter" onclick={closeFilter}>
            <Icon name="x" size={11} />
          </button>
        </div>
      {:else}
        <span class="label">Library</span>
        <div class="lib-head-actions">
          <button
            class="btn-icon"
            title="Filter library"
            onpointerdown={cancelCreate}
            onclick={startFilter}
          >
            <Icon name="search" size={13} />
          </button>
          <button class="btn-icon" title="New playlist" onclick={startCreate}>
            <Icon name="plus" size={14} />
          </button>
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
    </div>
  </div>

  <button class="nav-item settings" class:active={route.name === "settings"} onclick={() => navigate("settings")}>
    <Icon name="settings" size={17} /><span>Settings</span>
  </button>
</aside>
