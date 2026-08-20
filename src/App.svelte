<script>
  import { untrack } from "svelte";
  import {
    initEvents,
    route,
    detail,
    playback,
    credits,
    api,
    togglePlay,
    focusSearch,
    isLoggedOut,
    goBack,
    goForward,
    ui,
    maybeBackfillLazyQueue,
  } from "./lib/state.svelte.js";
  import IconSprite from "./components/IconSprite.svelte";
  import Icon from "./components/Icon.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import PlayerBar from "./components/PlayerBar.svelte";
  import TopBar from "./components/TopBar.svelte";
  import CreditsDialog from "./components/CreditsDialog.svelte";
  import NowPlayingPanel from "./components/NowPlayingPanel.svelte";
  import LibraryView from "./views/LibraryView.svelte";
  import LikedSongsView from "./views/LikedSongsView.svelte";
  import PlaylistView from "./views/PlaylistView.svelte";
  import AlbumView from "./views/AlbumView.svelte";
  import ArtistView from "./views/ArtistView.svelte";
  import SearchView from "./views/SearchView.svelte";
  import SearchSongsView from "./views/SearchSongsView.svelte";
  import QueueView from "./views/QueueView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import LoginView from "./views/LoginView.svelte";

  $effect(() => {
    initEvents();
  });

  // Dynamic album/catalogue queues stay small. The next bounded page is
  // requested only when playback approaches the loaded tail.
  $effect(() => {
    playback.current_index;
    playback.queue.length;
    untrack(() => maybeBackfillLazyQueue().catch(() => {}));
  });

  /* Whether decorative animation is allowed to run at all. A background window
     redrawing a VU meter is pure waste, and this app exists because the real
     client burns CPU at idle — so gate it on focus as well as on playback.
     A class beats a JS ticker: the compositor stops on its own and nothing
     re-enters the main thread. */
  let focused = $state(true);
  $effect(() => {
    focused = document.hasFocus();
    const on = () => (focused = true);
    const off = () => (focused = false);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", off);
    };
  });

  // Fetch detail data when a detail route becomes active. Browse commands use
  // their return value as the sole payload path; the sequence guard prevents a
  // slower response for a previous route from replacing the current page.
  let browseSeq = 0;
  $effect(() => {
    const { name, id } = route;
    const seq = ++browseSeq;
    if (!id) return;
    if (name === "playlist") {
      api.browsePlaylist(id).then((payload) => {
        if (seq === browseSeq) detail.playlist = payload ?? null;
      }).catch(() => {});
    } else if (name === "album") {
      api.browseAlbum(id).then((payload) => {
        if (seq === browseSeq) detail.album = payload ?? null;
      }).catch(() => {});
    } else if (name === "artist") {
      api.browseArtist(id).then((payload) => {
        if (seq === browseSeq) detail.artist = payload ?? null;
      }).catch(() => {});
    }
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

<div class="app" class:anim-paused={!playback.playing || !focused} class:has-inspector={ui.nowPlayingOpen}>
  <Sidebar />

  <main class="pane">
    <div class="scroll">
      <TopBar />

      {#if playback.error}
        <div class="error-banner" role="alert">
          <span class="error-text">{playback.error}</span>
          <button class="btn-icon" title="Dismiss" onclick={() => (playback.error = null)}>
            <Icon name="x" size={14} />
          </button>
        </div>
      {/if}

      {#if isLoggedOut()}
        <LoginView />
      {:else if route.name === "library"}
        <LibraryView />
      {:else if route.name === "search"}
        <SearchView />
      {:else if route.name === "search-songs"}
        <SearchSongsView />
      {:else if route.name === "liked"}
        <LikedSongsView />
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

  {#if ui.nowPlayingOpen}
    <NowPlayingPanel />
  {/if}

  <PlayerBar />
</div>

{#if credits.open}
  <CreditsDialog />
{/if}

<style>
  .error-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    margin: 0 var(--s6);
    padding: var(--s2) var(--s3);
    border-radius: var(--r2);
    background: color-mix(in srgb, var(--love) 12%, transparent);
    color: var(--danger);
    font-size: var(--t-12);
  }
  .error-text {
    min-width: 0; /* a long engine error must ellipsis, not widen the pane */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
