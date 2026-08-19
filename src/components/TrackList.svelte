<script>
  import { api, navigate, playback, togglePlay, toggleLiked, isTrackLiked, library } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import { formatTime } from "../lib/time.js";

  let {
    tracks = [],
    playFrom,
    showAlbum = true,
    showArt = true,
    playlistId = null,
  } = $props();

  /* One shared menu instance for the whole table rather than one per row. */
  const menu = $state({ open: false, x: 0, y: 0, track: null, index: -1 });
  const picker = $state({ open: false, x: 0, y: 0, track: null });

  function onRowDblClick(i) {
    if (i === playback.current_index && playback.playing) togglePlay();
    else playFrom(i);
  }

  function onPlayIconClick(i) {
    if (i === playback.current_index) togglePlay();
    else playFrom(i);
  }

  function openRowMenu(e, track, i) {
    e.stopPropagation();
    const r = e.currentTarget.getBoundingClientRect();
    menu.open = true;
    menu.x = Math.min(r.right - 216, window.innerWidth - 224);
    menu.y = r.bottom + 4;
    menu.track = track;
    menu.index = i;
    picker.open = false;
  }

  function openPicker() {
    if (!menu.track) return;
    picker.open = true;
    picker.track = menu.track;
    picker.x = Math.max(8, menu.x - 228);
    picker.y = menu.y;
  }

  function addToPlaylist(playlist) {
    if (!picker.track) return;
    api.addPlaylistTracks(playlist.id, [picker.track.uri]).catch(() => {});
    picker.open = false;
    menu.open = false;
  }

  $effect(() => {
    if (!menu.open && !picker.open) return;
    const onDown = (e) => {
      const el = e.target;
      if (el?.closest?.(".menu, .c-more")) return;
      menu.open = false;
      picker.open = false;
    };
    const onKey = (e) => {
      if (e.key === "Escape") {
        menu.open = false;
        picker.open = false;
      }
    };
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  });
</script>

<div class="tl" class:no-album={!showAlbum} class:album-page={!showArt}>
  <div class="tl-head">
    <span style="text-align:right">#</span>
    {#if showArt}<span></span>{/if}
    <span>Title</span>
    {#if showAlbum}<span>Album</span>{/if}
    <span></span>
    <span style="display:grid;justify-items:end"><Icon name="clock" size={13} /></span>
    <span></span>
  </div>

  {#each tracks as track, i}
    <div
      class="tl-row"
      class:current={track.uri === playback.current_uri}
      role="button"
      tabindex="-1"
      ondblclick={() => onRowDblClick(i)}
    >
      <span class="c-idx">
        <span class="n">{i + 1}</span>
        <span class="eq"><i></i><i></i><i></i><i></i></span>
        <button class="go" title="Play" onclick={() => onPlayIconClick(i)}>
          <Icon name={i === playback.current_index && playback.playing ? "pause" : "play"} size={12} />
        </button>
      </span>

      {#if showArt}
        <Cover
          src={track.cover_url}
          id={track.album_id || track.uri}
          name={track.album_name || track.name}
          size={36}
          class="c-art"
        />
      {/if}

      <span class="c-title">
        <span class="t-name">{track.name}</span>
        <span class="t-artists">{track.artist_names.join(", ")}</span>
      </span>

      {#if showAlbum}
        <!-- Always rendered, even when it repeats the title (single-track
             releases). Blanking those cells leaves holes that read as data
             that failed to load, which is worse than mild redundancy. -->
        <button class="c-album" onclick={() => track.album_id && navigate("album", track.album_id)}>
          {track.album_name}
        </button>
      {/if}

      <span class="c-like">
        <button
          class:liked={isTrackLiked(track.uri)}
          title={isTrackLiked(track.uri) ? "Remove from Liked Songs" : "Save to Liked Songs"}
          onclick={() => toggleLiked(track.uri)}
        >
          <Icon name={isTrackLiked(track.uri) ? "heart-filled" : "heart"} size={15} />
        </button>
      </span>

      <span class="c-time">{formatTime(track.duration_ms)}</span>

      <span class="c-more">
        <button title="More options" onclick={(e) => openRowMenu(e, track, i)}>
          <Icon name="more" size={15} />
        </button>
      </span>
    </div>
  {/each}
</div>

{#if menu.open}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px">
    <button class="menu-item" onclick={() => { menu.open = false; api.addQueue(menu.track).catch(() => {}); }}>
      Add to queue
    </button>
    <button class="menu-item" onclick={openPicker}>Add to playlist…</button>
    {#if menu.track?.album_id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("album", menu.track.album_id); }}>
        Go to album
      </button>
    {/if}
    {#if menu.track?.artist_id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("artist", menu.track.artist_id); }}>
        Go to artist
      </button>
    {/if}
    {#if playlistId}
      <div class="menu-sep"></div>
      <button
        class="menu-item danger"
        onclick={() => { menu.open = false; api.removePlaylistTracks(playlistId, [menu.track.uri]).catch(() => {}); }}
      >
        Remove from this playlist
      </button>
    {/if}
  </div>
{/if}

{#if picker.open}
  <div class="menu" style:left="{picker.x}px" style:top="{picker.y}px">
    {#each library as pl (pl.id)}
      <button class="menu-item" onclick={() => addToPlaylist(pl)}>{pl.name}</button>
    {/each}
  </div>
{/if}

<style>
  .menu {
    position: fixed;
    z-index: 200;
    max-height: 60vh;
    overflow-y: auto;
  }
</style>
