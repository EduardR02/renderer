<script>
  import { library, libraryState, navigate } from "../lib/state.svelte.js";
  import { playPlaylistById, playLikedSongs } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import LikedMark from "../components/LikedMark.svelte";
  import Icon from "../components/Icon.svelte";

  const greeting = $derived.by(() => {
    const h = new Date().getHours();
    if (h < 5) return "Good night";
    if (h < 12) return "Good morning";
    if (h < 18) return "Good afternoon";
    return "Good evening";
  });

  /**
   * Home takes the colour of whatever you played last.
   *
   * The library rail is ordered most-recently-played first, so `library[0]` is
   * a real answer to "what is this person listening to", not a decoration
   * picked at random — and it means the home screen is a different colour on
   * different days, which is the only thing a greeting and a grid of squares
   * were never going to be on their own.
   */
  const tone = $derived(
    coverTone(library[0]?.cover_url || library[0]?.cover_urls?.[0] || "", library[0]?.id ?? "home"),
  );

  let busy = $state("");
  let error = $state("");

  /* The play button on a card used to be a decorative <span> that did nothing
     when clicked — a control that looks like a control and is not one. A card
     only knows an id, so playing it means fetching the playlist first. */
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

  /* Same shape, different endpoint: Liked Songs is not in `library` and has no
     id, so it gets its own handler rather than a special case inside `play`. */
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

  {#if library.length}
    <div class="section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        <!-- =============================================================
             LIKED SONGS, FIRST, AND IT IS THE ROSE OBJECT ON THIS PAGE.

             The collection every account has was reachable only from the rail,
             which is why the home screen — the first thing the app opens on —
             had no rose in it at all. Rose cannot be recovered by tinting more
             small type; the palette's own rule is that it needs AREA. A ~200px
             tile on the landing page is the largest surface in the app that is
             legitimately "yours", so this is where it goes.

             It leads the grid rather than sorting into it because it is not a
             playlist: it is not in `library`, it cannot be renamed or deleted,
             and it never moves.
             ============================================================= -->
        <div class="card liked-card">
          <div class="card-art">
            <LikedMark fill />
            <button class="card-open" aria-label="Open Liked Songs" onclick={() => navigate("liked")}
            ></button>
            <!-- Rose, like the button on the collection page itself: playing
                 your own saved songs is the one action in the app whose
                 subject and whose actor are the same. -->
            <button
              class="card-play saved"
              aria-label="Play Liked Songs"
              title="Play Liked Songs"
              disabled={!!busy}
              onclick={playLiked}
            >
              <Icon name={busy === "liked" ? "more" : "play"} size={15} />
            </button>
          </div>
          <button class="card-copy" onclick={() => navigate("liked")}>
            <span class="card-name">Liked Songs</span>
            <span class="card-sub">Your collection</span>
          </button>
        </div>
        {#each library as pl (pl.id)}
          {@const tone = coverTone(pl.cover_url || pl.cover_urls?.[0] || "", pl.id)}
          <!-- The hover lift is the playlist's own colour. On a page with no
               header wash it is the only content colour there is, and forty
               covers each throwing their own light is what stops a grid of
               squares reading as a file browser. -->
          <div class="card" style:--tone-glow={tone.glow}>
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
              <span class="card-sub">
                {pl.tracks_total ? `${pl.tracks_total} songs` : "Playlist"}
              </span>
            </button>
          </div>
        {/each}
      </div>
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
    </div>
  {:else}
    <!-- The library arrives asynchronously and this is what you look at while
         it does, so it is a frame rather than a sentence: the same heading, the
         Liked Songs tile (which is real and needs nothing loaded), and card-
         shaped holes at exactly the geometry the covers land in. Static,
         because a shimmer animates for as long as the wait lasts and this app
         exists to not do that.

         Once the library HAS answered and is genuinely empty, the holes go and
         the sentence stays — an empty library is a fact, not a wait. -->
    <div class="section" style="margin-top:0">
      <div class="section-head"><h2 class="section-title">Your library</h2></div>
      <div class="grid">
        <div class="card liked-card">
          <div class="card-art">
            <LikedMark fill />
            <button class="card-open" aria-label="Open Liked Songs" onclick={() => navigate("liked")}
            ></button>
          </div>
          <button class="card-copy" onclick={() => navigate("liked")}>
            <span class="card-name">Liked Songs</span>
            <span class="card-sub">Your collection</span>
          </button>
        </div>
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
</section>
