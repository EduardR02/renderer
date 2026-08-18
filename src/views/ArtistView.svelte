<script>
  import { detail, api, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackList from "../components/TrackList.svelte";
  import Menu from "../components/Menu.svelte";

  const artist = $derived(detail.artist);

  function playFrom(i) {
    if (artist) api.playQueue(artist.top_tracks, i).catch(() => {});
  }

  function shufflePlay() {
    if (!artist) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(artist.top_tracks, 0).catch(() => {});
  }
</script>

<section class="page">
  {#if artist}
    <header class="detail-head">
      <Cover src={artist.cover_url} alt={artist.name} style="width:192px;height:192px" iconSize={56} circle rounded={0} />
      <div class="detail-info">
        <span class="detail-type">Artist</span>
        <h1 class="detail-title">{artist.name}</h1>
        <p class="detail-meta">
          {artist.top_tracks.length} popular track{artist.top_tracks.length === 1 ? "" : "s"}
        </p>
        <div class="detail-actions">
          <button class="btn-accent" disabled={!artist.top_tracks.length} onclick={() => playFrom(0)}>
            <Icon name="play" size={18} />
            Play
          </button>
          <button class="btn-ghost" disabled={!artist.top_tracks.length} onclick={shufflePlay}>
            <Icon name="shuffle" size={16} />
            Shuffle
          </button>
          <Menu
            width={200}
            variant="dots"
            items={[
              { label: "Play", disabled: !artist.top_tracks.length, action: () => playFrom(0) },
              { label: "Shuffle play", disabled: !artist.top_tracks.length, action: shufflePlay },
            ]}
          >
            {#snippet children()}
              <Icon name="more" size={22} />
            {/snippet}
          </Menu>
        </div>
      </div>
    </header>

    {#if artist.top_tracks.length}
      <section class="block">
        <h2 class="block-title">Popular</h2>
        <TrackList tracks={artist.top_tracks} playFrom={playFrom} showAlbum={false} />
      </section>
    {/if}

    {#if artist.albums.length}
      <section class="block">
        <h2 class="block-title">Albums</h2>
        <div class="card-grid">
          {#each artist.albums as al (al.id)}
            <button class="card" onclick={() => navigate("album", al.id)}>
              <div class="card-cover">
                <Cover src={al.cover_url} alt={al.name} rounded={4} />
                <span class="card-play"><Icon name="play" size={20} /></span>
              </div>
              <span class="card-name">{al.name}</span>
              <span class="card-sub">{al.artist_names.join(", ")}</span>
            </button>
          {/each}
        </div>
      </section>
    {/if}
  {:else}
    <div class="empty">
      <p>Loading artist…</p>
    </div>
  {/if}
</section>

<style>
  .detail-head {
    display: flex;
    align-items: flex-end;
    gap: var(--space-5);
    margin-bottom: var(--space-6);
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
  .detail-actions {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }
  .btn-accent {
    height: 44px;
  }
  .btn-ghost {
    height: 44px;
  }
  .block {
    margin-top: var(--space-6);
  }
  .block + .block {
    margin-top: var(--space-7);
  }
  .block-title {
    font-size: var(--font-xl);
    font-weight: 700;
    letter-spacing: -0.3px;
    margin-bottom: var(--space-4);
  }
</style>
