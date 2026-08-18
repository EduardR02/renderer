<script>
  import { route, navigate, library, api, focusSearch } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";

  let creating = $state(false);
  let newName = $state("");
  let createEl = $state(null);

  $effect(() => {
    if (creating) createEl?.focus();
  });

  function createPlaylist() {
    const name = newName.trim();
    creating = false;
    newName = "";
    if (name) api.createPlaylist(name).catch(() => {});
  }
</script>

<aside class="sidebar">
  <div
    class="brand"
    role="button"
    tabindex="0"
    title="Home"
    onclick={() => navigate("library")}
    onkeydown={(e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        navigate("library");
      }
    }}
  >
    <span class="logo"><Icon name="note" size={22} /></span>
    <span class="brand-name">Spotify Renderer</span>
  </div>

  <nav class="nav">
    <button class="nav-item" class:active={route.name === "library"} onclick={() => navigate("library")}>
      <Icon name="home" size={26} />
      <span>Home</span>
    </button>
    <button class="nav-item" class:active={route.name === "search"} onclick={focusSearch}>
      <Icon name="search" size={26} />
      <span>Search</span>
    </button>
  </nav>

  <div class="lib-section">
    <div class="lib-head">
      <span class="lib-title">Your Library</span>
      <button
        class="icon-btn"
        class:active={creating}
        title="Create playlist"
        onclick={() => {
          creating = !creating;
          newName = "";
        }}
      >
        <Icon name="plus" size={18} />
      </button>
    </div>

    {#if creating}
      <form class="create-row" onsubmit={(e) => { e.preventDefault(); createPlaylist(); }}>
        <input
          bind:this={createEl}
          placeholder="Playlist name"
          value={newName}
          oninput={(e) => (newName = e.currentTarget.value)}
          onkeydown={(e) => { if (e.key === "Escape") creating = false; }}
        />
      </form>
    {/if}

    <div class="lib-list">
      {#each library as pl (pl.id)}
        <button
          class="lib-row"
          class:active={route.name === "playlist" && route.id === pl.id}
          onclick={() => navigate("playlist", pl.id)}
        >
          <Cover src={pl.cover_url} alt={pl.name} style="width:48px;height:48px" iconSize={20} rounded={4} />
          <span class="lib-meta">
            <span class="lib-name">{pl.name}</span>
            <span class="lib-sub">{pl.owner ? `Playlist · ${pl.owner}` : "Playlist"}</span>
          </span>
        </button>
      {/each}
      {#if !library.length}
        <div class="lib-empty">No playlists yet</div>
      {/if}
    </div>
  </div>

  <div class="lib-footer">
    <button class="nav-item" class:active={route.name === "settings"} onclick={() => navigate("settings")}>
      <Icon name="settings" size={22} />
      <span>Settings</span>
    </button>
  </div>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    width: var(--sidebar-width);
    min-width: var(--sidebar-width);
    height: 100%;
    padding: var(--space-2);
    background: var(--bg-sidebar);
    overflow: hidden;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-3) var(--space-4);
    cursor: pointer;
  }
  .logo {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: #000;
    flex: none;
  }
  .brand-name {
    font-size: var(--font-lg);
    font-weight: 700;
    letter-spacing: -0.3px;
    white-space: nowrap;
  }
  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: var(--space-2);
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    height: 40px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-sm);
    font-size: var(--font-lg);
    font-weight: 700;
    color: var(--text-secondary);
    transition: color var(--transition-fast), background-color var(--transition-fast);
  }
  .nav-item:hover {
    color: var(--text-primary);
  }
  .nav-item.active {
    color: var(--text-primary);
    background: var(--bg-active);
  }
  .lib-section {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  .lib-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-2) var(--space-3);
  }
  .lib-title {
    font-size: var(--font-xs);
    font-weight: 700;
    letter-spacing: 1.2px;
    color: var(--text-secondary);
  }
  .create-row {
    padding: 0 var(--space-3) var(--space-2);
  }
  .create-row input {
    width: 100%;
    height: 34px;
    padding: 0 var(--space-3);
    border: none;
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    outline: none;
  }
  .create-row input:focus {
    box-shadow: 0 0 0 2px var(--accent);
  }
  .lib-list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 2px;
  }
  .lib-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    padding: var(--space-2);
    border-radius: var(--radius-sm);
    text-align: left;
    transition: background-color var(--transition-fast);
  }
  .lib-row:hover {
    background: var(--bg-highlight);
  }
  .lib-row.active {
    background: var(--bg-active);
  }
  .lib-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
  }
  .lib-name {
    font-size: var(--font-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lib-sub {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color var(--transition-fast);
  }
  .lib-row:hover .lib-sub {
    color: var(--text-primary);
  }
  .lib-empty {
    padding: var(--space-4) var(--space-3);
    font-size: var(--font-sm);
    color: var(--text-subdued);
  }
  .lib-footer {
    padding-top: var(--space-2);
    border-top: 1px solid rgba(255, 255, 255, 0.07);
  }
</style>
