<script>
  import { api, navigate, playback, togglePlay, toggleLiked, isTrackLiked, library } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import { formatTime } from "../lib/time.js";

  let {
    tracks = [],
    playFrom,
    showAlbum = true,
    playlistId = null,
  } = $props();

  /* Shared context menu (single instance for the whole table) */
  const menu = $state({ open: false, x: 0, y: 0, track: null, index: -1 });
  const picker = $state({ open: false, x: 0, y: 0, track: null });

  const cols = $derived(
    showAlbum
      ? "24px minmax(0, 4fr) minmax(0, 3fr) 44px 76px"
      : "24px minmax(0, 1fr) 44px 76px"
  );

  function onRowDblClick(i) {
    if (i === playback.current_index && playback.playing) {
      togglePlay();
      return;
    }
    playFrom(i);
  }

  function onPlayIconClick(i) {
    if (i === playback.current_index) togglePlay();
    else playFrom(i);
  }

  function openRowMenu(e, track, i) {
    e.stopPropagation();
    const r = e.currentTarget.getBoundingClientRect();
    menu.open = true;
    menu.x = Math.min(r.right - 220, window.innerWidth - 228);
    menu.y = r.bottom + 4;
    menu.track = track;
    menu.index = i;
    picker.open = false;
  }

  function openPicker() {
    const t = menu.track;
    if (!t) return;
    picker.open = true;
    picker.track = t;
    picker.x = Math.max(8, menu.x - 232);
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
      if (el && typeof el.closest === "function" && el.closest(".track-menu, .picker-menu, .row-more")) {
        return;
      }
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

<div class="tracklist" style:--cols={cols}>
  <div class="tl-head">
    <span class="c-idx">#</span>
    <span>Title</span>
    {#if showAlbum}<span>Album</span>{/if}
    <span class="c-time"><Icon name="clock" size={13} /></span>
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
        <span class="idx-num">{i + 1}</span>
        <span class="idx-current">
          <Icon name={playback.playing ? "pause" : "play"} size={15} />
        </span>
        <button class="idx-play" title="Play" onclick={() => onPlayIconClick(i)}>
          <Icon name="play" size={15} />
        </button>
      </span>

      <span class="c-title">
        <span class="t-name">{track.name}</span>
        <span class="dash">—</span>
        <span class="t-artists">{track.artist_names.join(", ")}</span>
      </span>

      {#if showAlbum}<span class="c-album">{track.album_name}</span>{/if}

      <span class="c-time">{formatTime(track.duration_ms)}</span>

      <span class="c-actions">
        <button
          class="row-btn like"
          class:liked={isTrackLiked(track.uri)}
          title={isTrackLiked(track.uri) ? "Remove from Liked Songs" : "Save to Liked Songs"}
          onclick={() => toggleLiked(track.uri)}
        >
          <Icon name={isTrackLiked(track.uri) ? "heart-filled" : "heart"} size={15} />
        </button>
        <button
          class="row-btn more"
          class:active={menu.open && menu.index === i}
          title="More options"
          onclick={(e) => openRowMenu(e, track, i)}
        >
          <Icon name="more" size={15} />
        </button>
      </span>
    </div>
  {/each}
</div>

{#if menu.open && menu.track}
  <div class="track-menu" role="menu" style:left={menu.x + "px"} style:top={menu.y + "px"}>
    <button role="menuitem" onclick={() => { menu.open = false; playFrom(menu.index); }}>
      Play
    </button>
    <button role="menuitem" onclick={() => { menu.open = false; api.addQueue(menu.track).catch(() => {}); }}>
      Add to queue
    </button>
    <button role="menuitem" onclick={openPicker}>Add to playlist…</button>
    {#if playlistId}
      <div class="menu-divider"></div>
      <button
        role="menuitem"
        class="danger"
        onclick={() => {
          menu.open = false;
          api.removePlaylistTracks(playlistId, [menu.track.uri]).catch(() => {});
        }}
      >
        Remove from this playlist
      </button>
      <button
        role="menuitem"
        disabled={menu.index <= 0}
        onclick={() => {
          menu.open = false;
          api.reorderPlaylistTracks(playlistId, menu.index, menu.index - 1).catch(() => {});
        }}
      >
        Move up
      </button>
      <button
        role="menuitem"
        disabled={menu.index >= tracks.length - 1}
        onclick={() => {
          menu.open = false;
          api.reorderPlaylistTracks(playlistId, menu.index, menu.index + 1).catch(() => {});
        }}
      >
        Move down
      </button>
    {/if}
    <div class="menu-divider"></div>
    {#if menu.track.artist_id}
      <button
        role="menuitem"
        onclick={() => { menu.open = false; navigate("artist", menu.track.artist_id); }}
      >
        Go to artist
      </button>
    {/if}
    {#if menu.track.album_id}
      <button
        role="menuitem"
        onclick={() => { menu.open = false; navigate("album", menu.track.album_id); }}
      >
        Go to album
      </button>
    {/if}
  </div>
{/if}

{#if picker.open && picker.track}
  <div class="picker-menu" role="menu" style:left={picker.x + "px"} style:top={picker.y + "px"}>
    <div class="picker-title">Add to playlist</div>
    {#if library.length}
      {#each library as pl}
        <button role="menuitem" onclick={() => addToPlaylist(pl)}>{pl.name}</button>
      {/each}
    {:else}
      <button role="menuitem" disabled>No playlists yet</button>
    {/if}
  </div>
{/if}

<style>
  .tracklist {
    display: flex;
    flex-direction: column;
    padding: 0 var(--space-2);
  }
  .tl-head,
  .tl-row {
    display: grid;
    grid-template-columns: var(--cols);
    align-items: center;
    gap: var(--space-4);
  }
  .tl-head {
    height: 36px;
    padding: 0 var(--space-3);
    font-size: var(--font-xs);
    color: var(--text-secondary);
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  }
  .tl-row {
    height: 56px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-sm);
    font-size: var(--font-md);
    transition: background-color var(--transition-fast);
  }
  .tl-row:hover {
    background: var(--bg-highlight);
  }
  .tl-row.current .t-name {
    color: var(--accent);
  }
  .c-idx {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-secondary);
    font-size: var(--font-sm);
    font-variant-numeric: tabular-nums;
  }
  .idx-current {
    display: none;
    color: var(--accent);
  }
  .idx-play {
    display: none;
    align-items: center;
    justify-content: center;
    color: var(--text-primary);
    padding: 4px;
  }
  .tl-row:hover .idx-num {
    display: none;
  }
  .tl-row.current .idx-num {
    display: none;
  }
  .tl-row.current .idx-current {
    display: flex;
  }
  .tl-row:hover .idx-current {
    display: none;
  }
  .tl-row:hover .idx-play {
    display: flex;
  }
  .c-title {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .t-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .t-artists,
  .dash {
    flex: none;
    color: var(--text-secondary);
    font-size: var(--font-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 60%;
  }
  .c-album {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-secondary);
    font-size: var(--font-sm);
  }
  .c-time {
    color: var(--text-secondary);
    font-size: var(--font-sm);
    font-variant-numeric: tabular-nums;
  }
  .c-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-1);
  }
  .row-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    border-radius: var(--radius-full);
    color: var(--text-secondary);
    opacity: 0;
    transition: color var(--transition-fast), opacity var(--transition-fast);
  }
  .row-btn:hover {
    color: var(--text-primary);
  }
  .row-btn.liked {
    color: var(--accent);
    opacity: 1;
  }
  .row-btn.more.active {
    color: var(--text-primary);
    opacity: 1;
  }
  .tl-row:hover .row-btn {
    opacity: 1;
  }

  .track-menu,
  .picker-menu {
    position: fixed;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    min-width: 212px;
    padding: 4px;
    border-radius: var(--radius-sm);
    background: var(--bg-menu);
    box-shadow: 0 16px 24px rgba(0, 0, 0, 0.6), 0 2px 8px rgba(0, 0, 0, 0.4);
  }
  .track-menu button,
  .picker-menu button {
    display: flex;
    align-items: center;
    height: 36px;
    padding: 0 12px;
    border-radius: 2px;
    font-size: var(--font-md);
    text-align: left;
    white-space: nowrap;
    transition: background-color var(--transition-fast);
  }
  .track-menu button:hover:not(:disabled),
  .picker-menu button:hover:not(:disabled) {
    background: var(--bg-menu-hover);
  }
  .track-menu button.danger {
    color: var(--danger);
  }
  .menu-divider {
    height: 1px;
    margin: 4px 8px;
    background: rgba(255, 255, 255, 0.12);
  }
  .picker-title {
    padding: 8px 12px 6px;
    font-size: var(--font-xs);
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.8px;
    color: var(--text-secondary);
  }
  .picker-menu {
    max-height: 320px;
    overflow-y: auto;
  }
</style>
