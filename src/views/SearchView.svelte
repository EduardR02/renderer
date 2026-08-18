<script>
  import { search, ui, api, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackList from "../components/TrackList.svelte";

  let inputEl = $state(null);

  $effect(() => {
    if (ui.searchFocusTick > 0) inputEl?.focus();
  });

  function submit(e) {
    e.preventDefault();
    const q = search.query.trim();
    if (!q) return;
    search.submitted = true;
    search.results = null;
    api.search(q, 40).catch(() => {});
  }

  function playTrack(i) {
    if (search.results?.tracks?.length) {
      api.playQueue(search.results.tracks, i).catch(() => {});
    }
  }
</script>

<section class="page search-page">
  <form class="search-form" onsubmit={submit}>
    <span class="search-icon"><Icon name="search" size={22} /></span>
    <input
      bind:this={inputEl}
      placeholder="What do you want to listen to?"
      value={search.query}
      oninput={(e) => (search.query = e.currentTarget.value)}
    />
    {#if search.query}
      <button class="clear-btn" type="button" title="Clear" onclick={() => { search.query = ""; search.results = null; search.submitted = false; }}>
        <Icon name="x" size={16} />
      </button>
    {/if}
  </form>

  {#if !search.submitted}
    <div class="empty">
      <Icon name="search" size={40} />
      <p>Search for songs, albums or artists.</p>
    </div>
  {:else if !search.results}
    <div class="empty">
      <p>Searching…</p>
    </div>
  {:else if !search.results.tracks.length && !search.results.albums.length && !search.results.artists.length}
    <div class="empty">
      <Icon name="search" size={40} />
      <p>No results for “{search.query}”.</p>
    </div>
  {:else}
    {#if search.results.tracks.length}
      <section class="block">
        <h2 class="block-title">Songs</h2>
        <TrackList tracks={search.results.tracks} playFrom={playTrack} />
      </section>
    {/if}

    {#if search.results.albums.length}
      <section class="block">
        <h2 class="block-title">Albums</h2>
        <div class="card-grid">
          {#each search.results.albums as al (al.id)}
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

    {#if search.results.artists.length}
      <section class="block">
        <h2 class="block-title">Artists</h2>
        <div class="artist-grid">
          {#each search.results.artists as ar (ar.id)}
            <button class="card artist-card" onclick={() => navigate("artist", ar.id)}>
              <div class="card-cover artist-cover">
                <Cover src={ar.cover_url} alt={ar.name} circle rounded={0} iconSize={32} />
              </div>
              <span class="card-name">{ar.name}</span>
              <span class="card-sub">Artist</span>
            </button>
          {/each}
        </div>
      </section>
    {/if}
  {/if}
</section>

<style>
  .search-page {
    padding-top: var(--space-6);
  }
  .search-form {
    position: relative;
    display: flex;
    align-items: center;
    height: 48px;
    margin-bottom: var(--space-6);
    border-radius: var(--radius-full);
    background: var(--bg-input);
  }
  .search-form:focus-within {
    box-shadow: 0 0 0 3px rgba(255, 255, 255, 0.25);
  }
  .search-icon {
    position: absolute;
    left: var(--space-4);
    display: flex;
    color: var(--text-secondary);
    pointer-events: none;
  }
  .search-form input {
    flex: 1;
    height: 100%;
    padding: 0 48px 0 52px;
    border: none;
    background: transparent;
    font-size: var(--font-md);
    outline: none;
  }
  .clear-btn {
    position: absolute;
    right: var(--space-2);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    border-radius: var(--radius-full);
    color: var(--text-secondary);
  }
  .clear-btn:hover {
    color: var(--text-primary);
  }
  .block {
    margin-bottom: var(--space-7);
  }
  .block-title {
    font-size: var(--font-xl);
    font-weight: 700;
    letter-spacing: -0.3px;
    margin-bottom: var(--space-4);
  }
  .artist-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: var(--space-5);
  }
  .artist-cover {
    aspect-ratio: 1 / 1;
  }
</style>
