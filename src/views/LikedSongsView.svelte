<script>
  import { api } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Icon from "../components/Icon.svelte";

  let tracks = $state([]);
  let nextCursor = $state(null);
  let loading = $state(false);
  let error = $state("");
  let requestedInitial = false;

  async function loadPage(cursor = null) {
    if (loading) return;
    loading = true;
    error = "";
    try {
      const page = await api.browseLikedSongs(cursor);
      const seen = new Set(tracks.map((track) => track.uri));
      const additions = [];
      for (const track of page?.tracks ?? []) {
        if (!track?.uri || seen.has(track.uri)) continue;
        seen.add(track.uri);
        additions.push(track);
      }
      // Preserve the array identity so TrackList extends its virtual window
      // instead of treating pagination as a new collection and scrolling up.
      tracks.push(...additions);
      nextCursor = page?.next_cursor ?? null;
    } catch (reason) {
      error = String(reason || "Could not load Liked Songs.");
    } finally {
      loading = false;
    }
  }

  /* Mounting this route is the lazy boundary. No library/bootstrap path calls
     the endpoint, and later pages require an explicit Load more action. */
  $effect(() => {
    if (requestedInitial) return;
    requestedInitial = true;
    loadPage();
  });

  function playFrom(index) {
    if (tracks.length) api.playQueue(tracks, index).catch(() => {});
  }
</script>

<section class="view page liked-page wash" style:--wash="var(--love)">
  <header class="liked-head">
    <div class="liked-art" aria-hidden="true"><Icon name="heart-filled" size={42} /></div>
    <div class="liked-copy">
      <span class="eyebrow">Your collection</span>
      <h1 class="detail-title">Liked Songs</h1>
      <p class="detail-meta">
        <span class="num">{tracks.length} loaded {tracks.length === 1 ? "song" : "songs"}</span>
        {#if nextCursor}<span class="sep">/</span><span>More available</span>{/if}
      </p>
    </div>
  </header>

  <div class="actions liked-actions">
    <button class="play-lg" title="Play Liked Songs" disabled={!tracks.length} onclick={() => playFrom(0)}>
      <Icon name="play" size={19} />
    </button>
    <span>Read-only Spotify collection</span>
  </div>

  {#if tracks.length}
    <div class="section liked-tracks">
      <TrackList {tracks} {playFrom} showLike={false} />
    </div>
  {:else if loading}
    <div class="tl liked-loading">
      {#each Array.from({ length: 8 }) as _, index (index)}
        <div class="sk-row"><span class="sk" style="width:12px"></span><span class="sk art"></span><span class="sk a"></span><span class="sk b"></span></div>
      {/each}
    </div>
  {:else if error}
    <div class="empty">
      <p class="h">Liked Songs unavailable</p><p class="sub">{error}</p>
      <div class="actions"><button class="btn-ghost" onclick={() => loadPage()}>Try again</button></div>
    </div>
  {:else}
    <div class="empty"><p class="h">No liked songs found.</p><p class="sub">Songs saved to your Spotify library will appear here.</p></div>
  {/if}

  {#if tracks.length && (nextCursor || loading || error)}
    <div class="liked-more">
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      {#if nextCursor}
        <button class="btn-ghost" disabled={loading} onclick={() => loadPage(nextCursor)}>
          {loading ? "Loading…" : "Load more"}
        </button>
      {/if}
    </div>
  {/if}
</section>
