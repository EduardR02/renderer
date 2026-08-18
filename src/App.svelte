<script>
  import { initEvents, route, api, playback, togglePlay, focusSearch, isLoggedOut } from "./lib/state.svelte.js";
  import Sidebar from "./components/Sidebar.svelte";
  import PlayerBar from "./components/PlayerBar.svelte";
  import LibraryView from "./views/LibraryView.svelte";
  import PlaylistView from "./views/PlaylistView.svelte";
  import AlbumView from "./views/AlbumView.svelte";
  import ArtistView from "./views/ArtistView.svelte";
  import SearchView from "./views/SearchView.svelte";
  import QueueView from "./views/QueueView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import LoginView from "./views/LoginView.svelte";

  $effect(() => {
    initEvents();
  });

  // Fetch detail data when a detail route becomes active.
  $effect(() => {
    const { name, id } = route;
    if (name === "playlist" && id) api.browsePlaylist(id).catch(() => {});
    else if (name === "album" && id) api.browseAlbum(id).catch(() => {});
    else if (name === "artist" && id) api.browseArtist(id).catch(() => {});
  });

  // Global keyboard shortcuts. Ignored while typing in inputs.
  $effect(() => {
    function onKey(e) {
      const t = e.target;
      const typing =
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.tagName === "SELECT" ||
          t.isContentEditable);

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        focusSearch();
        return;
      }
      if (typing) return;
      if (e.code === "Space") {
        e.preventDefault();
        togglePlay();
        return;
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
</script>

<div class="app">
  <div class="row">
    <Sidebar />
    <main class="content">
      {#if playback.error}
        <div class="error-banner" role="alert">
          <span class="error-text">{playback.error}</span>
          <button class="error-dismiss" title="Dismiss" onclick={() => (playback.error = null)}>
            ✕
          </button>
        </div>
      {/if}

      {#if isLoggedOut()}
        <LoginView />
      {:else}
        {#if route.name === "library"}
          <LibraryView />
        {:else if route.name === "search"}
          <SearchView />
        {:else if route.name === "playlist"}
          <PlaylistView />
        {:else if route.name === "album"}
          <AlbumView />
        {:else if route.name === "artist"}
          <ArtistView />
        {:else if route.name === "queue"}
          <QueueView />
        {:else if route.name === "settings"}
          <SettingsView />
        {/if}
      {/if}
    </main>
  </div>
  <PlayerBar />
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg-base);
  }
  .row {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .content {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    background: var(--bg-base);
  }
  .error-banner {
    position: sticky;
    top: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-2) var(--space-4);
    background: rgba(241, 94, 108, 0.15);
    color: #ffb3bb;
    font-size: var(--font-sm);
  }
  .error-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .error-dismiss {
    flex: none;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    color: #ffb3bb;
    transition: color var(--transition-fast), background-color var(--transition-fast);
  }
  .error-dismiss:hover {
    color: #fff;
    background: rgba(255, 255, 255, 0.1);
  }
</style>
