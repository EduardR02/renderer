<script>
  import { detail, api, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackList from "../components/TrackList.svelte";
  import Menu from "../components/Menu.svelte";
  import { formatDuration } from "../lib/time.js";

  const album = $derived(detail.album);
  const total = $derived(album ? album.tracks.reduce((a, t) => a + (t.duration_ms || 0), 0) : 0);
  const artistId = $derived(album?.tracks?.[0]?.artist_id ?? null);

  function playFrom(i) {
    if (album) api.playQueue(album.tracks, i).catch(() => {});
  }

  function shufflePlay() {
    if (!album) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(album.tracks, 0).catch(() => {});
  }
</script>

<section class="page">
  {#if album}
    <header class="detail-head">
      <Cover src={album.cover_url} alt={album.name} style="width:192px;height:192px" iconSize={48} rounded={8} />
      <div class="detail-info">
        <span class="detail-type">Album</span>
        <h1 class="detail-title">{album.name}</h1>
        <p class="detail-meta">
          {#if artistId}
            <button class="artist-link" onclick={() => navigate("artist", artistId)}>
              {album.artist_names.join(", ")}
            </button>
          {:else}
            {album.artist_names.join(", ")}
          {/if}
          <span class="dot">·</span>
          {album.tracks.length} song{album.tracks.length === 1 ? "" : "s"},
          {formatDuration(total)}
        </p>
        <div class="detail-actions">
          <button class="big-play" title="Play" disabled={!album.tracks.length} onclick={() => playFrom(0)}>
            <Icon name="play" size={24} />
          </button>
          <Menu
            width={200}
            variant="dots"
            items={[
              { label: "Play", disabled: !album.tracks.length, action: () => playFrom(0) },
              { label: "Shuffle play", disabled: !album.tracks.length, action: shufflePlay },
            ]}
          >
            {#snippet children()}
              <Icon name="more" size={22} />
            {/snippet}
          </Menu>
        </div>
      </div>
    </header>

    {#if album.tracks.length}
      <TrackList tracks={album.tracks} playFrom={playFrom} showAlbum={false} />
    {:else}
      <div class="empty">
        <Icon name="note" size={40} />
        <p>No tracks yet.</p>
      </div>
    {/if}
  {:else}
    <div class="empty">
      <p>Loading album…</p>
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
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    font-size: var(--font-md);
    color: var(--text-secondary);
  }
  .artist-link {
    color: var(--text-primary);
    transition: color var(--transition-fast), text-decoration-color var(--transition-fast);
    text-decoration: underline;
    text-decoration-color: transparent;
  }
  .artist-link:hover {
    color: var(--text-secondary);
    text-decoration-color: currentColor;
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
</style>
