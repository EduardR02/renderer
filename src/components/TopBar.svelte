<script>
  import {
    route,
    detail,
    search,
    ui,
    api,
    playback,
    togglePlay,
    navigate,
    goBack,
    goForward,
    canGoBack,
    canGoForward,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";

  let field = $state(null);

  // The sidebar's Search entry and Ctrl+F both focus this one field.
  $effect(() => {
    ui.searchFocusTick;
    if (route.name === "search") field?.focus();
  });

  /** Title shown in the bar once the detail header has scrolled away. */
  const title = $derived(
    route.name === "playlist"
      ? (detail.playlist?.name ?? "")
      : route.name === "album"
        ? (detail.album?.name ?? "")
        : route.name === "artist"
          ? (detail.artist?.name ?? "")
          : ""
  );

  function onInput(e) {
    search.query = e.currentTarget.value;
  }

  function onSubmit(e) {
    e.preventDefault();
    const q = search.query.trim();
    if (!q) return;
    if (route.name !== "search") navigate("search");
    api.search(q, 40).catch(() => {});
  }
</script>

<div class="topbar">
  <div class="hist">
    <button class="round" title="Back (Alt ←)" disabled={!canGoBack()} onclick={goBack}>
      <Icon name="back" size={16} />
    </button>
    <button class="round" title="Forward (Alt →)" disabled={!canGoForward()} onclick={goForward}>
      <Icon name="forward" size={16} />
    </button>
  </div>

  <!-- Fades in on scroll via `animation-timeline: scroll()`; no scroll listener. -->
  <div class="topbar-title" class:has={!!title}>
    {#if title}
      <button class="topbar-play" title={playback.playing ? "Pause" : "Play"} onclick={togglePlay}>
        <Icon name={playback.playing ? "pause" : "play"} size={12} />
      </button>
      <span class="t">{title}</span>
    {/if}
  </div>

  <form class="searchbox" onsubmit={onSubmit}>
    <Icon name="search" size={14} />
    <input
      bind:this={field}
      value={search.query}
      oninput={onInput}
      placeholder="Search songs, albums, artists"
      spellcheck="false"
      onfocus={() => route.name !== "search" && navigate("search")}
    />
  </form>
</div>
