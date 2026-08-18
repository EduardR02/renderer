<script>
  import { detail, api, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackList from "../components/TrackList.svelte";
  import Menu from "../components/Menu.svelte";
  import { formatDuration } from "../lib/time.js";

  let renaming = $state(false);
  let nameDraft = $state("");
  let renameEl = $state(null);

  $effect(() => {
    if (renaming) renameEl?.focus();
  });

  const pl = $derived(detail.playlist);
  const total = $derived(pl ? pl.tracks.reduce((a, t) => a + (t.duration_ms || 0), 0) : 0);

  function playFrom(i) {
    if (pl) api.playQueue(pl.tracks, i).catch(() => {});
  }

  function shufflePlay() {
    if (!pl) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(pl.tracks, 0).catch(() => {});
  }

  function startRename() {
    nameDraft = pl?.name ?? "";
    renaming = true;
  }

  function commitRename() {
    const n = nameDraft.trim();
    renaming = false;
    if (n && pl && n !== pl.name) api.renamePlaylist(pl.id, n).catch(() => {});
  }

  function removePlaylist() {
    if (!pl) return;
    if (confirm(`Delete playlist "${pl.name}"?`)) {
      api.deletePlaylist(pl.id).catch(() => {});
      navigate("library");
    }
  }
</script>

<section class="page">
  {#if pl}
    <header class="detail-head">
      <Cover src={pl.cover_url} alt={pl.name} style="width:192px;height:192px" iconSize={48} rounded={8} />
      <div class="detail-info">
        <span class="detail-type">Playlist</span>
        {#if renaming}
          <form class="rename-form" onsubmit={(e) => { e.preventDefault(); commitRename(); }}>
            <input
              bind:this={renameEl}
              value={nameDraft}
              placeholder="Playlist name"
              oninput={(e) => (nameDraft = e.currentTarget.value)}
              onkeydown={(e) => { if (e.key === "Escape") renaming = false; }}
            />
          </form>
        {:else}
          <h1 class="detail-title">{pl.name}</h1>
        {/if}
        <p class="detail-meta">
          {pl.owner ?? "Spotify"}
          {#if pl.tracks.length}
            <span class="dot">·</span> {pl.tracks.length} song{pl.tracks.length === 1 ? "" : "s"},
            {formatDuration(total)}
          {/if}
        </p>
        <div class="detail-actions">
          <button class="big-play" title="Play" disabled={!pl.tracks.length} onclick={() => playFrom(0)}>
            <Icon name="play" size={24} />
          </button>
          <Menu
            width={200}
            variant="dots"
            items={[
              { label: "Play", disabled: !pl.tracks.length, action: () => playFrom(0) },
              { label: "Shuffle play", disabled: !pl.tracks.length, action: shufflePlay },
              { label: "Rename", action: startRename },
              { label: "Delete playlist", danger: true, action: removePlaylist },
            ]}
          >
            {#snippet children()}
              <Icon name="more" size={22} />
            {/snippet}
          </Menu>
        </div>
      </div>
    </header>

    {#if pl.tracks.length}
      <TrackList tracks={pl.tracks} playFrom={playFrom} playlistId={pl.id} />
    {:else}
      <div class="empty">
        <Icon name="note" size={40} />
        <p>This playlist is empty.</p>
        <p class="sub">Add songs from Search with “Add to playlist”.</p>
      </div>
    {/if}
  {:else}
    <div class="empty">
      <p>Loading playlist…</p>
    </div>
  {/if}
</section>

<style>
  .detail-head {
    display: flex;
    align-items: flex-end;
    gap: var(--space-5);
    margin-bottom: var(--space-5);
  }
  .detail-info {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
  }
  .detail-type {
    font-size: var(--font-xs);
    font-weight: 700;
    letter-spacing: 1.2px;
    text-transform: uppercase;
    color: var(--text-secondary);
  }
  .detail-title {
    font-size: var(--font-3xl);
    font-weight: 700;
    letter-spacing: -1px;
    line-height: 1.1;
    overflow-wrap: anywhere;
  }
  .detail-meta {
    font-size: var(--font-md);
    color: var(--text-secondary);
  }
  .dot {
    margin: 0 4px;
    color: var(--text-subdued);
  }
  .detail-actions {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    margin-top: var(--space-3);
  }
  .big-play {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 56px;
    height: 56px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: #000;
    box-shadow: 0 8px 16px rgba(0, 0, 0, 0.5);
    transition: background-color var(--transition-fast), transform var(--transition-fast);
  }
  .big-play:hover:not(:disabled) {
    background: var(--accent-hover);
    transform: scale(1.04);
  }
  .big-play:active:not(:disabled) {
    background: var(--accent-active);
    transform: scale(1);
  }
  .big-play:disabled {
    background: var(--bg-hover);
    color: var(--text-secondary);
    box-shadow: none;
  }
  .rename-form {
    max-width: 480px;
  }
  .rename-form input {
    width: 100%;
    padding: 6px var(--space-3);
    border: none;
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    font-size: var(--font-2xl);
    font-weight: 700;
    outline: none;
  }
  .rename-form input:focus {
    box-shadow: 0 0 0 2px var(--accent);
  }
</style>
