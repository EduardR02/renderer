<script>
  import { untrack } from "svelte";
  import {
    initEvents,
    route,
    loadDetail,
    playback,
    credits,
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
  import RadioView from "./views/RadioView.svelte";
  import AlbumView from "./views/AlbumView.svelte";
  import ArtistView from "./views/ArtistView.svelte";
  import DiscographyView from "./views/DiscographyView.svelte";
  import SearchView from "./views/SearchView.svelte";
  import SearchSongsView from "./views/SearchSongsView.svelte";
  import QueueView from "./views/QueueView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import LoginView from "./views/LoginView.svelte";

  $effect(() => {
    initEvents();
  });

  /* The content pane's own width, published for the track table.
     A ResizeObserver rather than a window resize listener, because the pane
     also changes width when the inspector opens and the window does not; and
     rather than a media query, because the arithmetic that turns a window
     width into a pane width was being written out by hand and got it wrong.
     This is the only layout read outside a scroll handler and it fires only
     when the pane actually changes size. */
  let paneEl = $state(null);
  $effect(() => {
    const node = paneEl;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => {
      const width = Math.round(entry.contentRect.width);
      if (width !== ui.paneWidth) ui.paneWidth = width;
    });
    observer.observe(node);
    ui.paneWidth = Math.round(node.getBoundingClientRect().width);
    return () => observer.disconnect();
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

  /* Fetch detail data when a detail route becomes active. The fetch itself
     lives in the state module so that a failed page's "Try again" is literally
     the same call. `untrack` because loadDetail reads `detail` to decide
     whether the artist payload is already the right one, and this effect must
     depend on the ROUTE and nothing else. */
  $effect(() => {
    const name = route.name;
    const id = route.id;
    untrack(() => loadDetail(name, id));
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

  <main class="pane" bind:this={paneEl}>
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
      {:else if route.name === "radio"}
        <RadioView />
      {:else if route.name === "album"}
        <AlbumView />
      {:else if route.name === "artist"}
        <ArtistView />
      {:else if route.name === "discography"}
        <DiscographyView />
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
