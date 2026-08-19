<script>
  import {
    initEvents,
    route,
    api,
    playback,
    togglePlay,
    focusSearch,
    isLoggedOut,
    goBack,
    goForward,
  } from "./lib/state.svelte.js";
  import IconSprite from "./components/IconSprite.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import PlayerBar from "./components/PlayerBar.svelte";
  import TopBar from "./components/TopBar.svelte";
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

  // Global shortcuts. Ignored while typing in inputs.
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
      if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        goBack();
        return;
      }
      if (e.altKey && e.key === "ArrowRight") {
        e.preventDefault();
        goForward();
        return;
      }
      if (typing) return;
      if (e.code === "Space") {
        e.preventDefault();
        togglePlay();
      }
    }
    // Mouse thumb buttons: desktop users expect these to navigate.
    function onMouseUp(e) {
      if (e.button === 3) {
        e.preventDefault();
        goBack();
      } else if (e.button === 4) {
        e.preventDefault();
        goForward();
      }
    }
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouseUp);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mouseup", onMouseUp);
    };
  });
</script>

<IconSprite />

<div class="app">
  <Sidebar />

  <main class="pane">
    <div class="scroll">
      <TopBar />

      {#if playback.error}
        <div class="error-banner" role="alert">
          <span class="error-text">{playback.error}</span>
          <button class="btn-icon" title="Dismiss" onclick={() => (playback.error = null)}>✕</button>
        </div>
      {/if}

      {#if isLoggedOut()}
        <LoginView />
      {:else if route.name === "library"}
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
    </div>
  </main>

  <PlayerBar />
</div>

<style>
  .error-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    margin: 0 var(--s6);
    padding: var(--s2) var(--s3);
    border-radius: var(--r2);
    background: rgba(255, 107, 107, 0.12);
    color: var(--danger);
    font-size: var(--t-12);
  }
  .error-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
