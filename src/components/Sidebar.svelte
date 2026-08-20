<script>
  import { route, navigate, library, api, playback } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";

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
    filteredLibrary.length;
    filterQuery;
    const list = libList;
    if (!list) return;

    const updateFades = () => {
      const maxScroll = Math.max(0, list.scrollHeight - list.clientHeight);
      fadeTop = list.scrollTop > 1;
      fadeBottom = list.scrollTop < maxScroll - 1;
    };

    queueMicrotask(updateFades);
    list.addEventListener("scroll", updateFades, { passive: true });
    const resizeObserver = new ResizeObserver(updateFades);
    resizeObserver.observe(list);
    return () => {
      list.removeEventListener("scroll", updateFades);
      resizeObserver.disconnect();
    };
  });

  $effect(() => {
    if (creating) field?.focus();
  });

  $effect(() => {
    if (filtering) queueMicrotask(() => filterInput?.focus());
  });

  function startCreate() {
    filtering = false;
    filterQuery = "";
    creating = true;
  }

  function startFilter() {
    creating = false;
    newName = "";
    filtering = true;
  }

  function closeFilter() {
    filtering = false;
    filterQuery = "";
  }

  function commitCreate() {
    const name = newName.trim();
    creating = false;
    newName = "";
    if (name) api.createPlaylist(name).catch(() => {});
  }

  /** The playlist the current track came from, so its row can be marked. */
  const playingId = $derived(playback.queue[playback.current_index]?.album_id ?? null);
</script>

<aside class="sidebar">
  <div class="brand">
    <span class="mark"><Icon name="note" size={13} /></span>
    <span class="name">Renderer</span>
  </div>

  <nav class="nav">
    <button class="nav-item" class:active={route.name === "library"} onclick={() => navigate("library")}>
      <Icon name="home" size={17} /><span>Home</span>
    </button>
    <button class="nav-item" class:active={route.name === "queue"} onclick={() => navigate("queue")}>
      <Icon name="queue" size={17} /><span>Queue</span>
      {#if playback.queue.length}<span class="kbd">{playback.queue.length}</span>{/if}
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
            onpointerdown={(event) => {
              if (creating) event.preventDefault();
            }}
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
          onkeydown={(e) => e.key === "Escape" && ((creating = false), (newName = ""))}
        />
      </form>
    {/if}

    <div class="lib-list" class:fade-top={fadeTop} class:fade-bottom={fadeBottom} bind:this={libList}>
      {#each filteredLibrary as pl (pl.id)}
        <button
          class="lib-row"
          class:active={route.name === "playlist" && route.id === pl.id}
          class:playing={playingId === pl.id}
          onclick={() => navigate("playlist", pl.id)}
        >
          <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} size={32} />
          <span class="lib-name">{pl.name}</span>
          {#if pl.tracks_total}<span class="lib-count">{pl.tracks_total}</span>{/if}
        </button>
      {/each}
      {#if filterQuery.trim() && !filteredLibrary.length}
        <p class="lib-filter-empty">No matching playlists</p>
      {/if}
    </div>
  </div>

  <button class="nav-item settings" class:active={route.name === "settings"} onclick={() => navigate("settings")}>
    <Icon name="settings" size={17} /><span>Settings</span>
  </button>
</aside>
