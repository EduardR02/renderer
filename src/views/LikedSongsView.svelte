<script>
  import { untrack } from "svelte";
  import { api, ui } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Icon from "../components/Icon.svelte";
  import LikedMark from "../components/LikedMark.svelte";
  import { paletteFor } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";

  /* Rose's own hue, rebuilt at the header's fixed dark. Every other detail
     page takes its colour from artwork; this collection has none, and does not
     want any — it is the one page in the app that is about YOU rather than
     about a record, so it gets the palette's "yours" hue at full strength. */
  const ROSE_TONE = paletteFor(21, 0.105);

  /* Same rule as every other detail header: the artwork gives way first. */
  const artSize = $derived(detailArtSize(ui.paneWidth));

  let tracks = $state([]);
  let nextCursor = $state(null);
  let loading = $state(false);
  let error = $state("");
  let loadGeneration = 0;

  async function loadPage(cursor = null, generation = loadGeneration) {
    if (loading) return;
    loading = true;
    error = "";
    try {
      const page = await api.browseLikedSongs(cursor);
      if (generation !== loadGeneration) return;
      const seen = new Set(tracks.map((track) => track.uri));
      const additions = [];
      for (const track of page?.tracks ?? []) {
        if (!track?.uri || seen.has(track.uri)) continue;
        seen.add(track.uri);
        additions.push(track);
      }
      tracks.push(...additions);
      nextCursor = page?.next_cursor ?? null;
    } catch (reason) {
      if (generation === loadGeneration) {
        error = String(reason || "Could not load Liked Songs.");
      }
    } finally {
      if (generation === loadGeneration) loading = false;
    }
  }

  function reloadCollection() {
    const generation = ++loadGeneration;
    tracks = [];
    nextCursor = null;
    loading = false;
    error = "";
    loadPage(null, generation);
  }

  $effect(() => {
    untrack(reloadCollection);
  });

  function playFrom(index) {
    if (tracks.length) api.playQueue(tracks, index, "liked").catch(() => {});
  }
</script>

<section
  class="view page liked-page wash"
  style:--tone-wash={ROSE_TONE.wash}
  style:--tone-wash-deep={ROSE_TONE.washDeep}
  style:--tone-glow={ROSE_TONE.glow}
>
  <header class="liked-head detail-head">
    <LikedMark size={artSize} />
    <div class="liked-copy">
      <span class="tag saved">Your collection</span>
      <h1 class="detail-title">Liked Songs</h1>
      <p class="detail-meta">
        <span class="num">{tracks.length} loaded {tracks.length === 1 ? "song" : "songs"}</span>
        {#if nextCursor}<span class="sep">/</span><span>More available</span>{/if}
      </p>
    </div>
  </header>

  <div class="actions liked-actions">
    <!-- ROSE, not foam, and this is the one page where that is right: the
         palette's rule is that foam is what you can DO and rose is what is
         YOURS, and on every other page those are different objects. Here the
         thing you press and the thing it belongs to are the same collection.
         A 48px solid disc is also exactly the kind of surface rose can carry —
         area, not ink. -->
    <button
      class="play-lg saved"
      title="Play Liked Songs"
      disabled={!tracks.length}
      onclick={() => playFrom(0)}
    >
      <Icon name="play" size={19} />
    </button>
  </div>

  {#if tracks.length}
    <div class="section liked-tracks">
      <TrackList {tracks} {playFrom} queueContext="liked" />
    </div>
  {:else if loading}
    <div class="tl liked-loading" style="--cols:28px 36px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 10 }) as _, index (index)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{66 - ((index * 7) % 26)}%"></span>
            <span class="sk b" style="width:{31 - ((index * 5) % 12)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
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
