<script>
  import {
    api,
    navigate,
    playback,
    togglePlay,
    toggleLiked,
    isTrackLiked,
    library,
    openCredits,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import { formatTime } from "../lib/time.js";

  let {
    tracks = [],
    playFrom,
    showAlbum = true,
    showArt = true,
    /** Off for short embedded lists (search), where column heads are noise. */
    showHead = true,
    playlistId = null,
    /** Playlist tables may give artists and added dates their own columns. */
    showArtist = false,
    showAdded = false,
    /** Official desktop surfaces expose lifetime plays on albums/Popular. */
    showPlays = false,
    /** Read-only collections can suppress the local-only heart affordance. */
    showLike = true,
    /** Render every row and install no shared-pane windowing machinery. */
    disableWindowing = false,
    sortKey = null,
    sortDirection = "asc",
    onSort = null,
  } = $props();

  const addedDateFormatter = new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  const playCountFormatter = new Intl.NumberFormat();

  function formatAddedAt(value) {
    const timestamp = Number(value);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "—";
    const date = new Date(timestamp);
    if (!Number.isFinite(date.getTime())) return "—";
    return addedDateFormatter.format(date);
  }

  function formatPlayCount(value) {
    const count = Number(value);
    return Number.isFinite(count) && count > 0 ? playCountFormatter.format(count) : "";
  }

  function activateSort(key) {
    onSort?.(key);
  }

  function sortAriaLabel(key, label) {
    if (sortKey !== key) return `Sort by ${label}`;
    return `Sort by ${label}, currently ${sortDirection === "asc" ? "ascending" : "descending"}`;
  }

  /* One shared menu instance for the whole table rather than one per row. */
  const menu = $state({ open: false, x: 0, y: 0, track: null, index: -1 });
  const picker = $state({ open: false, x: 0, y: 0, track: null });

  /**
   * Which row is the playing one, as an index into `tracks`.
   *
   * `playback.current_index` indexes the QUEUE, not this list — this list is
   * whatever playlist/album/search result is on screen. Using it directly lit
   * up the row that merely sat at the same ordinal, so switching playlists kept
   * marking position N as playing even though it was a different song.
   * Identity is the only thing the two lists share, so resolve by URI.
   */
  const currentRow = $derived.by(() => {
    const uri = playback.current_uri;
    if (!uri) return -1;
    // When this list IS the playing context the index still agrees, and taking
    // it keeps duplicates resolving to the instance actually playing.
    const i = playback.current_index;
    if (i >= 0 && i < tracks.length && tracks[i]?.uri === uri) return i;
    return tracks.findIndex((t) => t.uri === uri);
  });

  function onRowDblClick(i) {
    if (i === currentRow && playback.playing) togglePlay();
    else playFrom(i);
  }

  function onPlayIconClick(i) {
    if (i === currentRow) togglePlay();
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

  /* ---------------- Windowed rendering ----------------
     Long page-level lists keep a fixed-height body and translate a keyed
     visible window through it. Absolute-index keys retain every overlapping
     row (including its Cover subtree) when the window advances, so a one-row
     slide only removes and adds the two boundary rows. The scroller is an
     ancestor (.scroll in App.svelte), not this component, so the offset has
     to be read from it rather than from a local scrollTop. */
  const ROW_H = 48; // must track --row-h
  /* Sliding the window one row at a time measured better than batching it into
     blocks of 8 (92 vs 86 fps, worst frame 18ms vs 27ms): the batched version
     does the same total work in rarer, bigger bursts, and it is the burst that
     misses the frame. Keep the updates small and frequent. */
  const OVERSCAN = 6;

  let bodyEl = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  /* Plain mirrors of the two above: measure() must not *read* reactive state,
     or the wiring effect below would re-subscribe on every scroll frame. */
  let curFirst = 0;
  let curLast = 0;

  /* A same-length playlist still needs a new window: identity changes reset
     the retained absolute-index range before the new rows are patched. */
  let seenTracks = null;
  let seenPlaylistId = null;
  let seenLength = -1;

  const visible = $derived(disableWindowing ? tracks : tracks.slice(firstRow, lastRow));

  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    const len = tracks.length;
    // Layout is clean during scroll, so these reads are cheap and — unlike a
    // cached offset — stay correct when the header above the list changes size.
    const above = scroller.getBoundingClientRect().top - bodyEl.getBoundingClientRect().top;
    /* Keep both ends inside the fixed-height body and never let `f` pass `l`.
       This also keeps the translated window valid if the shared scroller can
       continue below the list because another section follows it. */
    const f = Math.min(Math.max(0, Math.floor(above / ROW_H) - OVERSCAN), len);
    const l = Math.min(
      len,
      Math.max(f, Math.ceil((above + scroller.clientHeight) / ROW_H) + OVERSCAN),
    );
    if (f === curFirst && l === curLast) return;
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  function resetWindow(length, scroller) {
    if (scroller) scroller.scrollTop = 0;
    const initialLast = scroller
      ? Math.min(length, Math.ceil(scroller.clientHeight / ROW_H) + OVERSCAN)
      : length;
    curFirst = 0;
    curLast = initialLast;
    firstRow = 0;
    lastRow = initialLast;
  }

  function clampWindow(length) {
    const maxFirst = Math.max(0, length - 1);
    const f = Math.min(curFirst, maxFirst);
    const l = Math.min(Math.max(curLast, f), length);
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  /*
   * Reset before the new rows are patched into the DOM. Identity changes reset
   * the shared pane scroll and retained range; length-only changes preserve the
   * position but clamp the window so a shortened list cannot render a stale
   * blank range.
   */
  $effect.pre(() => {
    if (disableWindowing) return;
    const list = tracks;
    const length = list.length;
    const body = bodyEl;
    if (!body) return;

    const identityChanged = list !== seenTracks || playlistId !== seenPlaylistId;
    const lengthChanged = length !== seenLength;
    if (!identityChanged && !lengthChanged) return;

    seenTracks = list;
    seenPlaylistId = playlistId;
    seenLength = length;
    const scroller = body.closest(".scroll");
    if (identityChanged) resetWindow(length, scroller);
    else clampWindow(length);
  });

  $effect(() => {
    if (disableWindowing) return;
    // Re-runs when the list identity, its length, or its presentation context
    // changes; deliberately does not depend on firstRow/lastRow, which change
    // on every scroll frame.
    const list = tracks;
    list.length;
    playlistId;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) {
      // No scroll ancestor (embedded use): render everything, as before.
      curFirst = 0;
      curLast = tracks.length;
      firstRow = 0;
      lastRow = tracks.length;
      return;
    }
    let queued = false;
    const onScroll = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(() => measure(scroller));
    ro.observe(scroller);
    measure(scroller);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      ro.disconnect();
    };
  });

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

{#snippet trackRows(items, start)}
  {#each items as track, k (start + k)}
    {@const i = start + k}
    <div
      class="tl-row"
      class:current={i === currentRow}
      role="button"
      tabindex="-1"
      ondblclick={() => onRowDblClick(i)}
    >
      <span class="c-idx">
        <span class="n">{i + 1}</span>
        <span class="eq"><i></i><i></i><i></i><i></i></span>
        <button class="go" title="Play" onclick={() => onPlayIconClick(i)}>
          <Icon name={i === currentRow && playback.playing ? "pause" : "play"} size={12} />
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
        {#if !showArtist}
          <ArtistLinks
            class="t-artists"
            names={track.artist_names}
            ids={track.artist_ids ?? []}
            id={track.artist_id}
          />
        {/if}
      </span>

      {#if showArtist}
        <span class="c-artist">
          <ArtistLinks
            names={track.artist_names}
            ids={track.artist_ids ?? []}
            id={track.artist_id}
          />
        </span>
      {/if}

      {#if showAlbum}
        <!-- Always rendered, even when it repeats the title (single-track
             releases). Blanking those cells leaves holes that read as data
             that failed to load, which is worse than mild redundancy. -->
        <button class="c-album" onclick={() => track.album_id && navigate("album", track.album_id)}>
          {track.album_name}
        </button>
      {/if}

      {#if showAdded}
        <span class="c-added">{formatAddedAt(track.added_at)}</span>
      {/if}

      {#if showPlays}
        <span class="c-plays">{formatPlayCount(track.play_count)}</span>
      {/if}

      {#if showLike}
        <span class="c-like">
          <button
            class:liked={isTrackLiked(track.uri)}
            title={isTrackLiked(track.uri) ? "Remove from Liked Songs" : "Save to Liked Songs"}
            onclick={() => toggleLiked(track.uri)}
          >
            <Icon name={isTrackLiked(track.uri) ? "heart-filled" : "heart"} size={15} />
          </button>
        </span>
      {:else}
        <span aria-hidden="true"></span>
      {/if}

      <span class="c-time">{formatTime(track.duration_ms)}</span>

      <span class="c-more">
        <button title="More options" onclick={(e) => openRowMenu(e, track, i)}>
          <Icon name="more" size={15} />
        </button>
      </span>
    </div>
  {/each}
{/snippet}

<div
  class="tl"
  class:no-album={!showAlbum}
  class:album-page={!showArt}
  class:playlist-sort={showAdded}
  class:has-plays={showPlays}
  style="overflow-anchor: none"
>
  {#if showHead}
    <div class="tl-head">
      <button
        class="tl-sort-btn"
        class:active={sortKey === "order"}
        aria-label={sortAriaLabel("order", "original/custom order")}
        aria-pressed={sortKey === "order"}
        onclick={() => activateSort("order")}
      ># {#if sortKey === "order"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {#if showArt}<span></span>{/if}
      <button
        class="tl-sort-btn"
        class:active={sortKey === "title"}
        aria-label={sortAriaLabel("title", "title")}
        aria-pressed={sortKey === "title"}
        onclick={() => activateSort("title")}
      >Title {#if sortKey === "title"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {#if showArtist}
        <button
          class="tl-sort-btn"
          class:active={sortKey === "artist"}
          aria-label={sortAriaLabel("artist", "artist")}
          aria-pressed={sortKey === "artist"}
          onclick={() => activateSort("artist")}
        >Artist {#if sortKey === "artist"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if showAlbum}
        <button
          class="tl-sort-btn tl-col-album"
          class:active={sortKey === "album"}
          aria-label={sortAriaLabel("album", "album")}
          aria-pressed={sortKey === "album"}
          onclick={() => activateSort("album")}
        >Album {#if sortKey === "album"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if showAdded}
        <button
          class="tl-sort-btn tl-col-added"
          class:active={sortKey === "added"}
          aria-label={sortAriaLabel("added", "date added")}
          aria-pressed={sortKey === "added"}
          onclick={() => activateSort("added")}
        >Added {#if sortKey === "added"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if showPlays}<span class="tl-plays-head">Plays</span>{/if}
      <span></span>
      <button
        class="tl-sort-btn tl-sort-duration"
        class:active={sortKey === "duration"}
        aria-label={sortAriaLabel("duration", "duration")}
        aria-pressed={sortKey === "duration"}
        onclick={() => activateSort("duration")}
      ><span class="tl-sort-duration-label">Duration</span><Icon name="clock" size={13} />{#if sortKey === "duration"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      <span></span>
    </div>
  {/if}

  {#if disableWindowing}
    <div style="overflow-anchor: none">
      {@render trackRows(visible, 0)}
    </div>
  {:else}
    <div
      bind:this={bodyEl}
      style="overflow-anchor: none; position: relative"
      style:height="{tracks.length * ROW_H}px"
    >
      <div
        style="position: absolute; inset: 0 0 auto"
        style:transform="translateY({firstRow * ROW_H}px)"
      >
        {@render trackRows(visible, firstRow)}
      </div>
    </div>
  {/if}
</div>

{#if menu.open}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px">
    <button class="menu-item" onclick={() => { menu.open = false; api.addQueue(menu.track).catch(() => {}); }}>
      Add to queue
    </button>
    <button class="menu-item" onclick={openPicker}>Add to playlist…</button>
    {#if menu.track?.id}
      <button class="menu-item" onclick={() => { menu.open = false; openCredits(menu.track); }}>
        View credits
      </button>
    {/if}
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
