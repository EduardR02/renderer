<script>
  import { detail, api, navigate } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTotal } from "../lib/time.js";

  const album = $derived(detail.album);
  const tracks = $derived(album?.tracks ?? []);
  const artistId = $derived(tracks[0]?.artist_id ?? "");

  /* Same FNV-1a the artwork uses, so the header wash and the generated tile
     land on one colour identity for this album. */
  const hue = $derived.by(() => {
    const seed = album?.id ?? "";
    let h = 0x811c9dc5;
    for (let i = 0; i < seed.length; i++) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h % 360;
  });

  function playFrom(i) {
    if (tracks.length) api.playQueue(tracks, i).catch(() => {});
  }

  function shufflePlay() {
    if (!tracks.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(tracks, 0).catch(() => {});
  }
</script>

<section class="view page wash" style:--h={hue}>
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
        <span class="eyebrow">Album</span>
        <h1 class="detail-title">{album.name}</h1>
        <p class="detail-meta">
          {#if artistId}
            <button class="who" onclick={() => navigate("artist", artistId)}>
              {album.artist_names.join(", ")}
            </button>
          {:else}
            <span class="who">{album.artist_names.join(", ")}</span>
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
        <TrackList {tracks} {playFrom} showAlbum={false} showArt={false} />
      </div>
    {:else}
      <div class="empty">
        <p class="h">No tracks on this release.</p>
        <p class="sub">Nothing came back from the engine for this album.</p>
      </div>
    {/if}
  {/if}
</section>
