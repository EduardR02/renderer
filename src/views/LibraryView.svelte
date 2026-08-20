<script>
  import { library, navigate } from "../lib/state.svelte.js";
  import { playPlaylistById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
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
    <div class="empty">
      <p>No playlists yet.</p>
      <p class="sub">They will appear here once your library loads.</p>
    </div>
  {/if}
</section>
