<script>
  import { untrack } from "svelte";
  import {
    library,
    libraryState,
    route,
    playback,
    navigate,
    ui,
    ensurePersonalizedDiscovery,
    mergePersonalizedPlaylists,
  } from "../lib/state.svelte.js";
  import { playPlaylistById, playLikedSongs } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import LikedMark from "../components/LikedMark.svelte";
  import Icon from "../components/Icon.svelte";
  import { playlistSubtitle } from "../lib/artist.js";
  const greeting = $derived.by(() => {
    const h = new Date().getHours();
    if (h < 5) return "Good night";
    if (h < 12) return "Good morning";
    if (h < 18) return "Good afternoon";
    return "Good evening";
  });

  /*
   * Home has one source of truth: the loaded library. The backend orders it by
   * `last_activity` for the sidebar; listening history is the explicit
   * exception and is copied/sorted by `last_played` here.
   */
  const uniqueLibrary = $derived.by(() => {
    const seen = new Set();
    return library.filter((playlist) => {
      const id = playlist?.id;
      if (!id || seen.has(id)) return false;
      seen.add(id);
      return true;
    });
  });

  /*
   * Provisional shelves stay empty until the fresh rootlist lands. The cached
   * snapshot can carry stale `last_played` rows; deriving shelves from it
   * would mount a listening-history band that the fresh answer immediately
   * reshuffles. Your library below renders from the same snapshot unwaited.
   */
  const recentlyPlayed = $derived.by(() => {
    if (!libraryState.fresh) return [];
    return uniqueLibrary
      .filter((playlist) => Number.isFinite(Number(playlist?.last_played)))
      .slice()
      .sort((left, right) => Number(right.last_played) - Number(left.last_played));
  });

  const CARD_MIN = 158;
  const CARD_GAP = 20;
  const cardsPerRow = $derived.by(() => {
    const usable = (ui.paneWidth || 1200) - 2 * 24;
    const count = Math.floor((usable + CARD_GAP) / (CARD_MIN + CARD_GAP));
    return Math.max(2, Math.min(7, count));
  });

  const recentShelf = $derived(recentlyPlayed.slice(0, cardsPerRow));
  const recentIds = $derived(new Set(recentShelf.map((playlist) => playlist.id)));

  /* Search discovery is lazy and session-cached. Waiting for the library
     answer means the conditional Discover Weekly query never races an empty
     rootlist snapshot. */
  $effect(() => {
    const home = route.name === "library";
    const ready = playback.ready === true && playback.auth_state === "ready";
    const answered = libraryState.loaded;
    if (!home || !ready || !answered) return;
    untrack(() => ensurePersonalizedDiscovery(library));
  });

  /* Same gate: personalized cards join only once the rootlist is
     authoritative, so the band mounts already final instead of refilling. */
  const madeForYouCandidates = $derived.by(() => {
    if (!libraryState.fresh) return [];
    return mergePersonalizedPlaylists(uniqueLibrary).filter(
      (playlist) => !recentIds.has(playlist.id),
    );
  });
  const madeForYouInitialCount = $derived(cardsPerRow * 2);
  const madeForYou = $derived(
    madeForYouCandidates.slice(0, madeForYouInitialCount),
  );
  const canSeeAllMadeForYou = $derived(
    madeForYouCandidates.length > madeForYouInitialCount,
  );
  const shownIds = $derived(
    new Set([
      ...recentShelf,
      // Keep personalized cards out of Your library: See all reveals them in
      // one canonical place rather than duplicating them below.
      ...madeForYouCandidates,
    ].map((playlist) => playlist.id)),
  );
  const remainingLibrary = $derived(
    uniqueLibrary.filter((playlist) => !shownIds.has(playlist.id)),
  );

  const tonePlaylist = $derived(recentShelf[0] ?? uniqueLibrary[0] ?? null);
  const tone = $derived(
    coverTone(
      tonePlaylist?.cover_url || tonePlaylist?.cover_urls?.[0] || "",
      tonePlaylist?.id ?? "home",
    ),
  );

  let busy = $state("");
  let error = $state("");

  async function play(id) {
    if (busy) return;
    busy = id;
    error = "";
    try {
      await playPlaylistById(id);
    } catch (reason) {
      error = String(reason || "Could not play this playlist.");
    } finally {
      busy = "";
    }
  }

  async function playLiked() {
    if (busy) return;
    busy = "liked";
    error = "";
    try {
      await playLikedSongs();
    } catch (reason) {
      error = String(reason || "Could not play Liked Songs.");
    } finally {
      busy = "";
    }
  }
</script>

{#snippet likedCard(canPlay)}
  <div class="card liked-card">
    <div class="card-art">
      <LikedMark fill />
      <button class="card-open" aria-label="Open Liked Songs" onclick={() => navigate("liked")}
      ></button>
      {#if canPlay}
        <button
          class="card-play saved"
          aria-label="Play Liked Songs"
          title="Play Liked Songs"
          disabled={!!busy}
          onclick={playLiked}
        >
          <Icon name={busy === "liked" ? "more" : "play"} size={15} />
        </button>
      {/if}
    </div>
    <button class="card-copy" onclick={() => navigate("liked")}>
      <span class="card-name">Liked Songs</span>
      <span class="card-sub">Your collection</span>
    </button>
  </div>
{/snippet}

{#snippet playlistCard(pl)}
  {@const cardTone = coverTone(pl.cover_url || pl.cover_urls?.[0] || "", pl.id)}
  <div class="card" style:--tone-glow={cardTone.glow}>
    <div class="card-art">
      <Cover src={pl.cover_url} srcs={pl.cover_urls ?? []} id={pl.id} name={pl.name} fill lg />
      <button
        class="card-open"
        aria-label={`Open ${pl.name}`}
        onclick={() => navigate("playlist", pl.id)}
      ></button>
      <button
        class="card-play"
        aria-label={`Play ${pl.name}`}
        title={`Play ${pl.name}`}
        disabled={!!busy}
        onclick={() => play(pl.id)}
      >
        <Icon name={busy === pl.id ? "more" : "play"} size={15} />
      </button>
    </div>
    <button class="card-copy" onclick={() => navigate("playlist", pl.id)}>
      <span class="card-name">{pl.name}</span>
      <span class="card-sub">{playlistSubtitle(pl)}</span>
    </button>
  </div>
{/snippet}

<!-- `soft`: a third of the usual wash. A greeting is not a 56px record title,
     and the first row of covers sits inside this field — at full strength the
     colour would compete with the artwork it is supposed to introduce. -->
<section
  class="view page wash soft"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  <div style="padding:var(--s4) 0 var(--s6)">
    <h1 class="page-title">{greeting}</h1>
  </div>

  {#if recentShelf.length}
    <div class="section home-shelf-section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Recently played</h2></div>
      <div
        class="grid home-shelf"
        style={`--shelf-columns:${Math.max(2, recentShelf.length)}`}
      >
        {#each recentShelf as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {/if}

  {#if madeForYou.length}
    <div class="section home-shelf-section">
      <div class="section-head">
        <h2 class="section-title">Made for you</h2>
        {#if canSeeAllMadeForYou}
          <button
            class="link-more"
            type="button"
            onclick={() => navigate("made-for-you")}
          >
            See all
          </button>
        {/if}
      </div>
      <div
        class="grid home-shelf"
        style={`--shelf-columns:${cardsPerRow}`}
      >
        {#each madeForYou as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {/if}

  {#if uniqueLibrary.length}
    <div
      class="section"
      class:home-first={!recentShelf.length && !madeForYou.length}
    >
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        {@render likedCard(true)}
        {#each remainingLibrary as pl (pl.id)}
          {@render playlistCard(pl)}
        {/each}
      </div>
    </div>
  {:else}
    <!-- The library arrives asynchronously and this is what you look at while
         it does, so it is a frame rather than a sentence: real card geometry
         lands without a jump. Once the library answers, the holes disappear. -->
    <div class="section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        {@render likedCard(false)}
        {#if !libraryState.loaded}
          {#each Array.from({ length: 11 }) as _, i (i)}
            <div class="card" aria-hidden="true">
              <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
              <span class="card-copy">
                <span class="skeleton line" style="width:{74 - ((i * 11) % 30)}%;height:12px;margin:0"></span>
                <span class="skeleton line" style="width:38%;height:10px;margin:6px 0 0"></span>
              </span>
            </div>
          {/each}
        {/if}
      </div>
      {#if libraryState.loaded}
        <p class="sub" style="margin-top:var(--s5)">
          No playlists yet — anything you save in Spotify shows up here.
        </p>
      {/if}
    </div>
  {/if}

  {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
</section>

<style>
  /*
   * Shelves reuse the exact card object and cover interactions from `.grid`.
   * The column count follows the pane width, so Made for you always stays
   * within two responsive rows; See all owns the unbounded view.
   */
  .grid.home-shelf {
    grid-template-columns: repeat(var(--shelf-columns), minmax(0, 1fr));
  }


  .home-first { margin-top: 0; }
  @media (max-width: 620px) {
    .home-shelf { gap: var(--s4); }
  }
</style>
