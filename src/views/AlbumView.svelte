<script>
  import { detail, api } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ArtistLinks from "../components/ArtistLinks.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { formatTotal } from "../lib/time.js";

  const album = $derived(detail.album);
  const tracks = $derived(album?.tracks ?? []);
  const artistIds = $derived(album?.artist_ids ?? []);
  // Keep old/partial payloads useful: the primary track id can still link the
  // first header artist, while missing parallel ids leave other names plain.
  const artistFallbackId = $derived(artistIds[0] || tracks[0]?.artist_id || "");

  /* An album always has a sleeve, so this is the page where content colour is
     least ambiguous: the header is simply the record's own colour. (The old
     `--h` here was dead — it set a variable the wash rule never read, so every
     album page washed the same default foam.) */
  const tone = $derived(coverTone(album?.cover_url ?? "", album?.id ?? ""));

  function playFrom(i) {
    if (tracks.length) api.playQueue(tracks, i).catch(() => {});
  }

  function shufflePlay() {
    if (!tracks.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(tracks, 0).catch(() => {});
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if !album}
    <header class="detail-head">
      <span class="art lg skeleton" style="width:184px;height:184px"></span>
      <div>
        <span class="skeleton line sm"></span>
        <span class="skeleton line lg"></span>
      </div>
    </header>
  {:else}
    <header class="detail-head">
      <Cover src={album.cover_url} id={album.id} name={album.name} size={184} lg raised />
      <div>
        <span class="tag">Album</span>
        <h1 class="detail-title">{album.name}</h1>
        <p class="detail-meta">
          <ArtistLinks
            class="who"
            names={album.artist_names}
            ids={artistIds}
            id={artistFallbackId}
          />
          {#if album.year}
            <span class="sep">/</span><span class="num">{album.year}</span>
          {/if}
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button class="play-lg" title="Play" onclick={() => playFrom(0)} disabled={!tracks.length}>
            <Icon name="play" size={19} />
          </button>
          <button class="btn-ghost" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={14} />Shuffle
          </button>
        </div>
      </div>
    </header>

    {#if tracks.length}
      <!-- No album column and no per-row thumbnail: the art is already the
           largest thing on the page, and every row would repeat it. -->
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} showAlbum={false} showArt={false} showPlays />
      </div>
    {:else}
      <div class="empty">
        <p class="h">No tracks on this release.</p>
        <p class="sub">Nothing came back from the engine for this album.</p>
      </div>
    {/if}
  {/if}
</section>
