<script>
  import { tick, untrack } from "svelte";
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
    api,
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
  import MadeForYouView from "./views/MadeForYouView.svelte";
  import LikedSongsView from "./views/LikedSongsView.svelte";
  import PlaylistView from "./views/PlaylistView.svelte";
  import RadioView from "./views/RadioView.svelte";
  import AlbumView from "./views/AlbumView.svelte";
  import ArtistView from "./views/ArtistView.svelte";
  import DiscographyView from "./views/DiscographyView.svelte";
  import FansAlsoLikeView from "./views/FansAlsoLikeView.svelte";
  import AppearsOnView from "./views/AppearsOnView.svelte";
  import ArtistPlaylistCollectionView from "./views/ArtistPlaylistCollectionView.svelte";
  import SearchView from "./views/SearchView.svelte";
  import SearchSongsView from "./views/SearchSongsView.svelte";
  import QueueView from "./views/QueueView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import HistoryView from "./views/HistoryView.svelte";
  import TrackEditorView from "./views/TrackEditorView.svelte";
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

  /* Every route reuses this one scroll container, so the pane keeps a small
     ledger of where each route identity (name + id + param) was last left,
     written by a passive scroll listener for as long as a view is live.
     Revisiting a route restores its entry; a first visit opens at the top.
     Recording from the listener rather than at navigation time matters:
     leaving a detail route drops its data before the route flips, the
     outgoing view collapses, and by the time navigation could read the
     pane the browser has already clamped scrollTop — the last event that
     fired under the outgoing identity is the honest position. Zeroing in
     $effect.pre keeps the incoming view from inheriting those pixels for
     even one frame.

     Detail pages fetch their body after mounting, so a remembered offset
     routinely exceeds what is laid out yet. Restoration then stays
     pending: it follows layout growth with a ResizeObserver over the
     view's children and applies once the scroll range can represent the
     target. While pending, every scroll event is noise — the browser's
     clamp, our own zeroing, view-internal autoscrolls — and is ignored, so
     none of them can overwrite the saved target. Once applied, programmatic
     scrolls simply re-record the same offset under the incoming key.
     No timers, no polling: growth itself wakes the observer, and every
     exit path disconnects both it and the listener. */
  let scrollEl = $state(null);
  const SCROLL_MEMORY_MAX = 50; // matches the navigation history scale
  const scrollMemory = new Map(); // route identity -> last pane offset
  let lastRouteKey = null;
  let pendingRestoreKey = null;

  function routeKey() {
    return `${route.name}\u0000${route.id ?? ""}\u0000${route.param ?? ""}`;
  }

  function remember(key, offset) {
    scrollMemory.delete(key);
    scrollMemory.set(key, offset);
    if (scrollMemory.size > SCROLL_MEMORY_MAX) {
      scrollMemory.delete(scrollMemory.keys().next().value);
    }
  }

  $effect.pre(() => {
    const key = routeKey();
    const node = scrollEl;
    const previous = lastRouteKey;
    lastRouteKey = key;
    if (!node || previous === null || previous === key) return;
    pendingRestoreKey = key;
    node.scrollTop = 0;
  });

  $effect(() => {
    const node = scrollEl;
    if (!node) return;
    const onScroll = () => {
      if (lastRouteKey === null || lastRouteKey === pendingRestoreKey) return;
      remember(lastRouteKey, node.scrollTop);
    };
    node.addEventListener("scroll", onScroll, { passive: true });
    return () => node.removeEventListener("scroll", onScroll);
  });

  $effect(() => {
    const key = routeKey();
    const node = scrollEl;
    if (!node) return;
    const target = scrollMemory.get(key) ?? 0;
    let disposed = false;
    let observer = null;

    function fitsTarget() {
      return node.scrollHeight - node.clientHeight >= target;
    }
    function stopWaiting() {
      if (observer) {
        observer.disconnect();
        observer = null;
      }
      if (pendingRestoreKey === key) pendingRestoreKey = null;
    }
    function followGrowth() {
      pendingRestoreKey = key;
      const watching = new Set();
      // Observing a child that leaves the DOM reports it with a zero rect,
      // so a wholesale skeleton-for-content swap re-enters here on its own
      // and picks up the replacement nodes; growth inside a child reports
      // directly. Either wake rechecks the range against the target.
      function watchChildren() {
        for (const child of node.children) {
          if (!watching.has(child)) {
            watching.add(child);
            observer.observe(child);
          }
        }
        for (const child of watching) {
          if (!child.isConnected) {
            watching.delete(child);
            observer.unobserve(child);
          }
        }
      }
      observer = new ResizeObserver(() => {
        watchChildren();
        if (!disposed && fitsTarget()) {
          stopWaiting();
          apply();
        }
      });
      watchChildren();
    }
    function apply() {
      node.scrollTo({ top: target, behavior: "instant" });
    }

    // Restore only after Svelte commits the incoming view, so the range we
    // measure belongs to the new route.
    tick().then(() => {
      if (disposed) return;
      if (fitsTarget()) {
        stopWaiting();
        apply();
      } else followGrowth();
    });

    return () => {
      disposed = true;
      stopWaiting();
    };
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
  $effect(() => {
    ui.windowFocused = document.hasFocus();
    const on = () => (ui.windowFocused = true);
    const off = () => (ui.windowFocused = false);
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
      // Hardware media keys work even while typing — that is their point.
      // (Unfocused delivery is the SMTC integration's job.)
      if (e.key === "MediaPlayPause") {
        e.preventDefault();
        togglePlay();
        return;
      }
      if (e.key === "MediaTrackNext") {
        e.preventDefault();
        api.next();
        return;
      }
      if (e.key === "MediaTrackPrevious") {
        e.preventDefault();
        api.previous();
        return;
      }
      if (typing) return;
      if ((e.ctrlKey || e.metaKey) && e.key === "ArrowRight") {
        e.preventDefault();
        api.next();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "ArrowLeft") {
        e.preventDefault();
        api.previous();
        return;
      }
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

<div class="app" class:anim-paused={!playback.playing || !ui.windowFocused} class:has-inspector={ui.nowPlayingOpen}>
  <Sidebar />

  <main class="pane" bind:this={paneEl}>
    <div class="scroll" bind:this={scrollEl}>
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
      {:else if route.name === "made-for-you"}
        <MadeForYouView />
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
      {:else if route.name === "fans-also-like"}
        <FansAlsoLikeView />
      {:else if route.name === "appears-on"}
        <AppearsOnView />
      {:else if route.name === "artist-playlists" || route.name === "discovered-on"}
        <ArtistPlaylistCollectionView />
      {:else if route.name === "queue"}
        <QueueView />
      {:else if route.name === "history"}
        <HistoryView />
      {:else if route.name === "track-editor"}
        <TrackEditorView />
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
