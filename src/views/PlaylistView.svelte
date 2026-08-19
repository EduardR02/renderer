<script>
  import { detail, api, playback, navigate, route } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTotal } from "../lib/time.js";

  const pl = $derived(detail.playlist);
  const tracks = $derived(pl?.tracks ?? []);

  /* Same deterministic hash the artwork uses, so a playlist's header wash and
     its generated tile share one colour identity. */
  const hue = $derived.by(() => {
    const seed = pl?.id ?? "";
    let h = 0x811c9dc5;
    for (let i = 0; i < seed.length; i++) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h % 360;
  });

  /* Fallback for a playlist the backend has not swept yet: derive the mosaic
     candidates from the tracks we already have on screen. */
  const artPool = $derived([...new Set(tracks.map((t) => t.cover_url).filter(Boolean))].slice(0, 4));

  let renaming = $state(false);
  let nameDraft = $state("");

  function playFrom(i) {
    if (pl) api.playQueue(tracks, i).catch(() => {});
  }

  function playAll() {
    if (tracks.length) api.playQueue(tracks, 0).catch(() => {});
  }

  function shufflePlay() {
    api.setShuffle(true).catch(() => {});
    if (tracks.length) api.playQueue(tracks, 0).catch(() => {});
  }

  function startRename() {
    nameDraft = pl?.name ?? "";
    renaming = true;
  }

  function commitRename() {
    const n = nameDraft.trim();
    renaming = false;
    if (n && pl && n !== pl.name) api.renamePlaylist(pl.id, n).catch(() => {});
  }

  function removePlaylist() {
    if (!pl) return;
    if (confirm(`Delete playlist "${pl.name}"?`)) {
      api.deletePlaylist(pl.id).catch(() => {});
      navigate("library");
    }
  }
</script>

<section class="view page wash" style:--h={hue}>
  {#if !pl}
    <header class="detail-head">
      <span class="art lg skeleton" style="width:184px;height:184px"></span>
      <div>
        <span class="skeleton line sm"></span>
        <span class="skeleton line lg"></span>
      </div>
    </header>
  {:else}
    <header class="detail-head">
      <Cover
        src={pl.cover_url}
        srcs={pl.cover_urls?.length ? pl.cover_urls : artPool}
        id={pl.id}
        name={pl.name}
        size={184}
        lg
        raised
      />
      <div>
        <span class="eyebrow">Playlist</span>
        {#if renaming}
          <form
            onsubmit={(e) => {
              e.preventDefault();
              commitRename();
            }}
          >
            <input
              class="rename"
              bind:value={nameDraft}
              onblur={commitRename}
              onkeydown={(e) => e.key === "Escape" && (renaming = false)}
              spellcheck="false"
            />
          </form>
        {:else}
          <h1 class="detail-title" ondblclick={startRename} title="Double-click to rename">
            {pl.name}
          </h1>
        {/if}
        <p class="detail-meta">
          <span class="who">{pl.owner}</span>
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button class="play-lg" title="Play" onclick={playAll} disabled={!tracks.length}>
            <Icon name="play" size={19} />
          </button>
          <button class="btn-ghost" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={14} />Shuffle
          </button>
          <button class="btn-icon" title="Rename" onclick={startRename}>
            <Icon name="more" size={18} />
          </button>
          <button class="btn-icon danger" title="Delete playlist" onclick={removePlaylist}>
            <Icon name="x" size={16} />
          </button>
        </div>
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} playlistId={pl.id} />
      </div>
    {:else}
      <div class="empty">
        <p>No songs here yet.</p>
        <p class="sub">Find something in Search and add it to this playlist.</p>
      </div>
    {/if}
  {/if}
</section>
