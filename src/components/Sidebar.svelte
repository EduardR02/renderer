<script>
  import { route, navigate, library, api, playback, focusSearch } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";

  let creating = $state(false);
  let newName = $state("");
  let field = $state(null);

  $effect(() => {
    if (creating) field?.focus();
  });

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
    <button class="nav-item" class:active={route.name === "search"} onclick={focusSearch}>
      <Icon name="search" size={17} /><span>Search</span><span class="kbd">Ctrl F</span>
    </button>
    <button class="nav-item" class:active={route.name === "queue"} onclick={() => navigate("queue")}>
      <Icon name="queue" size={17} /><span>Queue</span>
      {#if playback.queue.length}<span class="kbd">{playback.queue.length}</span>{/if}
    </button>
  </nav>

  <div class="lib">
    <div class="lib-head">
      <span class="label">Library</span>
      <button class="btn-icon" title="New playlist" onclick={() => (creating = true)}>
        <Icon name="plus" size={14} />
      </button>
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

    <div class="lib-list">
      {#each library as pl (pl.id)}
        <button
          class="lib-row"
          class:active={route.name === "playlist" && route.id === pl.id}
          class:playing={playingId === pl.id}
          onclick={() => navigate("playlist", pl.id)}
        >
          <Cover src={pl.cover_url} id={pl.id} name={pl.name} size={32} />
          <span class="lib-name">{pl.name}</span>
          {#if pl.tracks_total}<span class="lib-count">{pl.tracks_total}</span>{/if}
        </button>
      {/each}
    </div>
  </div>

  <button class="nav-item settings" class:active={route.name === "settings"} onclick={() => navigate("settings")}>
    <Icon name="settings" size={17} /><span>Settings</span>
  </button>
</aside>
