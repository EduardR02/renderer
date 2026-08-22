<script>
  import {
    api,
    navigate,
    playback,
    togglePlay,
    toggleLiked,
    isTrackLiked,
    library,
    promotePlaylist,
    openCredits,
    ui,
    openTrackEditor,
  } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import { formatTime } from "../lib/time.js";
  import { observeStuck } from "../lib/sticky.js";
  import { rowWindow } from "../lib/virtual.js";

  let {
    tracks = [],
    playFrom,
    showAlbum = true,
    showArt = true,
    /** Off for short embedded lists (search), where column heads are noise. */
    showHead = true,
    playlistId = null,
    /** Compact source context copied onto tracks added to the queue. */
    queueContext = null,
    /** Playlist tables may give the added date its own column. */
    showAdded = false,
    /** Official desktop surfaces expose lifetime plays on albums/Popular. */
    showPlays = false,
    /** Read-only collections can suppress the local-only heart affordance. */
    showLike = true,
    /** Recommendation rows may forbid adding when the source playlist is read-only. */
    allowAddToPlaylist = true,
    /** Render every row and install no shared-pane windowing machinery. */
    disableWindowing = false,
    sortKey = null,
    sortDirection = "asc",
    onSort = null,
    /** Optional always-visible trailing action used by compact recommendation lists. */
    rowActionLabel = null,
    onRowAction = null,
    /** Optional per-row guard for an always-visible trailing action. */
    rowActionDisabled = null,
  } = $props();

  const queueSource = $derived(
    String(queueContext || (playlistId ? `playlist:${playlistId}` : "")).trim(),
  );

  /* =====================================================================
     COLUMNS — one source of truth, in the component that renders the cells.

     This used to be nine `--cols` declarations in app.css, duplicated across
     two media-query blocks, and it was broken: inside those blocks
     `.has-inspector .tl:not(.no-album):not(.album-page)` scores 0-4-0 and
     `.has-inspector .tl.playlist-sort` only 0-3-0, so a playlist table got the
     SIX-column template while it was still rendering nine cells. Grid does
     exactly what it is told with that — cells seven, eight and nine wrap onto
     an implicit second row inside a 48px-high box, which is why the kebab
     landed on top of the next row's play icon and flickered under the pointer.
     That was reachable at any window narrower than 1276px with the now-playing
     rail open, which is most of the sizes this app is used at.

     A template that can disagree with the cells is the bug, so the two are now
     computed from the same booleans, here, and the CSS just reads `--cols`.

     THE ARTIST IS NOT A COLUMN. It used to be one on playlist tables, and it
     was costing the table a whole elastic track to repeat information that the
     title cell can carry for free on its second line — which is what every
     other list in this app, and the official client, already do. The width it
     was taking is why titles and album names in a playlist ran out of room and
     had to be dropped early. It now always renders under the title, at every
     width, so the row shape is the same everywhere instead of changing under
     you at 560px.

     Sorting by artist survives the column: the title head is two controls
     rather than one, because the cell it names genuinely holds two things.

     The DROP ORDER is a design decision and is written out below rather than
     encoded in breakpoints scattered through a stylesheet. Space is taken from
     CONTEXT before IDENTITY:

       1. Added      — when you saved it matters least
       2. Album      — where it came from
       3. Plays      — how popular it is
       4. Artwork    — last, and only in the extreme (a 940px window with the
                       inspector open leaves the pane 332px wide)

     Nothing ever wraps and nothing is ever squeezed to zero: every elastic
     column is a minmax(0, …) and every cell truncates.
     ===================================================================== */
  const COL = {
    /* Widths, in the order the cells appear in the row. */
    idx: "28px",
    art: "36px",
    plays: "108px",
    like: "32px",
    time: "52px",
    more: "28px",
  };

  /* Pane width, in CSS px, below which each column stops earning its keep.
     Measured against the pane rather than the window — see `ui.paneWidth`. */
  const NEEDS = { added: 780, album: 640, plays: 540, art: 430 };
  /* Below this the 16px column gap becomes 12px: at a narrow pane the gaps are
     a bigger share of the row than any single column. */
  const DENSE_BELOW = 780;

  const pane = $derived(ui.paneWidth || 1200);
  const colArt = $derived(showArt && pane >= NEEDS.art);
  const colAlbum = $derived(showAlbum && pane >= NEEDS.album);
  const colAdded = $derived(showAdded && pane >= NEEDS.added);
  const colPlays = $derived(showPlays && pane >= NEEDS.plays);
  const dense = $derived(pane < DENSE_BELOW);

  const cols = $derived.by(() => {
    const list = [COL.idx];
    if (colArt) list.push(COL.art);
    /* The title carries the row. It gets the largest share when it shares the
       elastic space, and all of it when it does not. */
    const elastic = (colAlbum ? 1 : 0) + (colAdded ? 1 : 0);
    list.push(elastic ? "minmax(0, 2.2fr)" : "minmax(0, 1fr)");
    if (colAlbum) list.push("minmax(0, 1.6fr)");
    /* A date has a natural minimum — "12 Aug 2026" — and truncating it to
       "12 Au…" tells you nothing, so this column has a floor rather than a
       share it can be squeezed out of. */
    if (colAdded) list.push("minmax(84px, 0.8fr)");
    if (colPlays) list.push(COL.plays);
    list.push(COL.like, COL.time, rowActionLabel ? "56px" : COL.more);
    return list.join(" ");
  });

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

  /**
   * Puts a popover in the browser's TOP LAYER, and it is the whole fix for the
   * row menu disappearing behind the page.
   *
   * These menus are `position: fixed`, but fixed positioning does not take an
   * element out of its ancestors' PAINT order, and this one is rendered inside
   * the page: a detail page wraps its content in `.wash`, which gives every
   * child `z-index: 1` and the header `z-index: 2`, so the menu was competing
   * inside a local stacking context it could never win. Opening upward near the
   * top of a list put it under the header; opening downward near the bottom put
   * it under the "Recommended" section, which is simply a LATER child of the
   * same wash at the same z-index. No number fixes that — `z-index: 200` was
   * already there and lost to a `z-index: 1` sibling, because the comparison
   * that decides it happens one level up.
   *
   * The top layer is outside every stacking context and every clip on the page,
   * so there is nothing left to lose to. `manual` rather than `auto` because
   * the menu and its submenus have to coexist (an `auto` popover light-dismisses
   * its siblings) and because dismissal is already handled below.
   */
  function topLayer(node) {
    node.showPopover();
    return { destroy: () => node.isConnected && node.hidePopover() };
  }

  function closeMenus() {
    menu.open = false;
    picker.open = false;
    artistPicker.open = false;
  }

  /* One shared menu instance for the whole table rather than one per row.
     `maxH` is part of the position, not decoration: see openRowMenu. */
  const menu = $state({
    open: false, x: 0, top: null, bottom: null, maxH: 0,
    track: null, index: -1, copied: false,
    editDefined: false, editEnabled: false, editLoading: false,
  });
  const picker = $state({ open: false, x: 0, top: null, bottom: null, maxH: 0, track: null });
  /* Same shape and same placement rules as the playlist picker — a second
     surface hanging off one row of the menu, rather than N rows inside it. */
  const artistPicker = $state({ open: false, x: 0, top: null, bottom: null, maxH: 0, artists: [] });

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
    if (tracks[i]?.unavailable) return;
    if (i === currentRow && playback.playing) togglePlay();
    else playFrom(i);
  }

  function onPlayIconClick(i) {
    if (tracks[i]?.unavailable) return;
    if (i === currentRow) togglePlay();
    else playFrom(i);
  }

  /**
   * Roughly how tall the menu can get: seven items at 30px, separator, and
   * the 4px padding. An estimate is enough — it only decides which SIDE of the
   * button the menu opens on, and being a few pixels out changes nothing.
   * Measuring the real height would mean rendering it offscreen first, which
   * is a layout read and a frame of flicker for no gain.
   */
  const MENU_MAX_H = 232;
  /* No menu is ever laid out shorter than this. Below about four rows a menu
     stops being a list and becomes a peephole, and scrolling one item at a
     time is worse than covering a little of the row you opened it from. */
  const MENU_MIN_H = 132;

  /**
   * The band a fixed-position popover may occupy, in WINDOW coordinates.
   *
   * The top of that band is not the top of the window: the topbar is sticky at
   * `z-index: 30` and carries a `backdrop-filter`, so anything drawn under it
   * is both blurred and unclickable. The old clamp was `Math.max(8, …)`, i.e.
   * 8px from the WINDOW — and the bar ends 60px down (an 8px app gutter plus
   * its own 52px), so that clamp parked the menu 52px inside the glass. That is
   * exactly the reported bug: open the menu on the first row of a playlist
   * scrolled to the top, it flips upward, hits the clamp, and its first two
   * items are drawn behind the bar where nothing can click them.
   *
   * Measured rather than derived from `--topbar-h`, because the bar's offset
   * from the window depends on the shell's gutter and padding as well as its
   * own height, and a rect is the honest answer to "where does it actually
   * end". One layout read, in a click handler — never in a scroll handler.
   */
  function popoverBand() {
    const bar = document.querySelector(".topbar");
    const top = (bar ? bar.getBoundingClientRect().bottom : 0) + 8;
    return { top, bottom: window.innerHeight - 8 };
  }

  /**
   * Places a popover against an anchor rect, inside the band.
   *
   * Returns the edge it is PINNED BY rather than a `y`, and that is the whole
   * trick. Opening downward pins the top to the anchor's bottom; opening
   * upward pins the BOTTOM to the anchor's top, so the menu grows away from
   * the row it belongs to and always touches it, whatever its content height
   * turns out to be. Computing a `y` for the upward case instead means
   * subtracting a GUESSED height, which is what put the old menu 8px from the
   * top of the window — under the topbar — whenever the guess was too big.
   *
   * `maxH` is then simply the room on the chosen side. It is a ceiling, not a
   * size: a menu that fits renders at its natural height, and one that does
   * not scrolls inside the space it has, so no item is ever unreachable at any
   * window height.
   *
   * `desiredH` only decides which side to prefer.
   */
  function placePopover(anchor, desiredH) {
    const band = popoverBand();
    const roomBelow = band.bottom - (anchor.bottom + 4);
    const roomAbove = anchor.top - 4 - band.top;
    if (roomBelow >= desiredH || roomBelow >= roomAbove) {
      /* The clamp matters even opening DOWNWARD: a row can be partly under the
         sticky bar (keyboard focus, a programmatic scroll), and its anchor rect
         is then above the band's top edge. */
      return {
        top: Math.max(band.top, anchor.bottom + 4),
        bottom: null,
        maxH: Math.max(MENU_MIN_H, roomBelow),
      };
    }
    return {
      top: null,
      bottom: window.innerHeight - (anchor.top - 4),
      maxH: Math.max(MENU_MIN_H, roomAbove),
    };
  }

  function openRowMenu(e, track, i) {
    e.stopPropagation();
    /* The trigger is a TOGGLE. The dismiss handler below deliberately ignores
       pointer-downs on the kebab — otherwise it would close the menu a moment
       before this click reopened it, and the button would look dead — which
       leaves closing on a second press to this line. Pressing a different row's
       kebab still just moves the menu to that row. */
    if (menu.open && menu.index === i) {
      closeMenus();
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    const placed = placePopover(r, MENU_MAX_H);
    menu.open = true;
    menu.x = Math.min(r.right - 216, window.innerWidth - 224);
    menu.top = placed.top;
    menu.bottom = placed.bottom;
    menu.maxH = placed.maxH;
    menu.track = track;
    menu.index = i;
    menu.copied = false;
    picker.open = false;
    artistPicker.open = false;
    menu.editDefined = false;
    menu.editEnabled = false;
    menu.editLoading = !!playlistId;
    if (playlistId && track?.id) {
      api.getTrackEdit(track.id, playlistId)
        .then((status) => {
          if (!menu.open || menu.track?.id !== track.id) return;
          menu.editDefined = !!status?.definition;
          menu.editEnabled = !!status?.enabled;
        })
        .catch(() => {})
        .finally(() => {
          if (menu.open && menu.track?.id === track.id) menu.editLoading = false;
        });
    }
  }

  async function toggleEditedVersion() {
    if (!playlistId || !menu.track?.id || !menu.editDefined || menu.editLoading) return;
    const enabled = !menu.editEnabled;
    menu.editLoading = true;
    try {
      await api.setPlaylistTrackEditEnabled(playlistId, menu.track.id, enabled);
      menu.editEnabled = enabled;
    } finally {
      menu.editLoading = false;
    }
  }

  function openPicker(e) {
    if (!menu.track) return;
    /* The picker is as tall as the library, so it is placed against its own
       trigger and given the same band as the menu rather than inheriting a
       position that was computed for a much shorter list. */
    const placed = placePopover(e.currentTarget.getBoundingClientRect(), 60 * 6);
    picker.open = true;
    picker.track = menu.track;
    picker.x = Math.max(8, menu.x - 228);
    picker.top = placed.top;
    picker.bottom = placed.bottom;
    picker.maxH = placed.maxH;
  }

  /** One artist: go. Several: hang a submenu off this row. */
  function goToArtist(e) {
    if (menuArtists.length === 1) {
      menu.open = false;
      navigate("artist", menuArtists[0].id);
      return;
    }
    const placed = placePopover(e.currentTarget.getBoundingClientRect(), 40 * menuArtists.length);
    artistPicker.open = true;
    artistPicker.artists = menuArtists;
    artistPicker.x = Math.max(8, menu.x - 228);
    artistPicker.top = placed.top;
    artistPicker.bottom = placed.bottom;
    artistPicker.maxH = placed.maxH;
  }

  /* ---------------- Copy link ----------------
     The public URL for a track is `open.spotify.com/track/<id>`, and the id is
     the one already on the row — derived from `id`, or from the trailing
     segment of `spotify:track:<id>` when only the URI is present. Never
     assembled from anything else: a guessed URL that resolves to the wrong page
     is worse than no menu item.

     `navigator.clipboard` is available because WebView2 serves the app from a
     localhost origin, which is a secure context. No clipboard PLUGIN is
     installed and this deliberately does not add one. The execCommand path is
     the fallback for the case where the async API is refused (it can reject on
     a document that is not focused), and it is the only thing left that works
     there. */
  function trackLink(track) {
    const id = track?.id || String(track?.uri ?? "").split(":").pop();
    return id ? `https://open.spotify.com/track/${id}` : "";
  }

  async function writeClipboard(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      try {
        const field = document.createElement("textarea");
        field.value = text;
        field.setAttribute("readonly", "");
        field.style.cssText = "position:fixed;top:-1000px;opacity:0";
        document.body.appendChild(field);
        field.select();
        const ok = document.execCommand("copy");
        field.remove();
        return ok;
      } catch {
        return false;
      }
    }
  }

  let copyResetTimer = 0;
  /* The menu STAYS OPEN on copy, because the confirmation is on the item
     itself and dismissing it would take the only feedback with it. It closes
     shortly after, which is also what reverts the label. */
  async function copyTrackLink() {
    const link = trackLink(menu.track);
    if (!link) return;
    menu.copied = await writeClipboard(link);
    if (!menu.copied) return;
    clearTimeout(copyResetTimer);
    copyResetTimer = setTimeout(() => {
      menu.open = false;
      menu.copied = false;
    }, 900);
  }

  /**
   * Which artists this row's menu can send you to.
   *
   * "Go to artist" read `track.artist_id`, the PRIMARY artist, so a collab
   * silently offered exactly one of its artists and no way to reach the rest.
   * The row already carries the full parallel `artist_ids`/`artist_names`
   * pair — it is what the artist line under the title links with — so the
   * fix is to read the list that is already there.
   *
   * Naming every artist as its own row was the first attempt and it was wrong:
   * a three-way collaboration pushed three near-identical lines into a menu
   * that already has seven, and the artists are reachable from the line under
   * the title anyway. So the menu keeps ONE row whatever the track. With a
   * single artist it navigates; with several it opens the same kind of submenu
   * the playlist picker uses, which costs one row instead of N.
   */
  const menuArtists = $derived.by(() => {
    const track = menu.track;
    if (!track) return [];
    const names = track.artist_names ?? [];
    const ids = track.artist_ids ?? [];
    const linkable = names
      .map((name, i) => ({ name, id: ids[i] ?? "" }))
      .filter((artist) => artist.id && artist.name);
    if (linkable.length) return linkable;
    return track.artist_id ? [{ name: names[0] ?? "artist", id: track.artist_id }] : [];
  });

  async function addToPlaylist(playlist) {
    if (!picker.track) return;
    const uri = picker.track.uri;
    /* Close both menus before waiting on IPC so the picker never holds the
       pointer hostage while the add is in flight. */
    picker.open = false;
    artistPicker.open = false;
    menu.open = false;
    try {
      await api.addPlaylistTracks(playlist.id, [uri]);
      // An add is library activity, not playback. Failed adds never reach
      // either promotion or the activity command.
      promotePlaylist(playlist.id);
      api.touchPlaylistActivity(playlist.id).catch(() => {});
    } catch {
      // A failed add must update neither timestamp.
    }
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

  let rootEl = $state(null);
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
    /* The window arithmetic is shared with the history list; the clamping it
       does also keeps this window valid when the shared scroller continues
       below the table because another section follows it. */
    const { first: f, last: l } = rowWindow(bodyEl, scroller, ROW_H, OVERSCAN, tracks.length);
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

  /* The column heads are type on the page until rows start passing under
     them; see observeStuck for why this is an observer and not a scroll
     timeline or a scroll handler. */
  let headSentinel = $state(null);
  let headStuck = $state(false);
  $effect(() => observeStuck(headSentinel, (stuck) => (headStuck = stuck)));

  $effect(() => {
    if (!menu.open && !picker.open && !artistPicker.open) return;
    const onDown = (e) => {
      const el = e.target;
      /* The kebab is exempt so that its own click can toggle; see openRowMenu. */
      if (el?.closest?.(".menu, .c-more")) return;
      closeMenus();
    };
    const onKey = (e) => {
      if (e.key === "Escape") closeMenus();
    };
    /* A menu is placed against a row, from that row's rect, once. Scrolling or
       resizing moves the row and not the menu, so the anchoring is stale the
       moment either happens — and a menu that follows a scrolling row is a
       per-frame layout read on the scroller, which this app does not do
       anywhere. Dismissing is both cheaper and what the pointer is asking for. */
    const scroller = rootEl?.closest(".scroll");
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", closeMenus);
    scroller?.addEventListener("scroll", closeMenus, { passive: true });
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", closeMenus);
      scroller?.removeEventListener("scroll", closeMenus);
    };
  });
</script>

{#snippet trackRows(items, start)}
  {#each items as track, k (start + k)}
    {@const i = start + k}
    <div
      class="tl-row"
      class:current={i === currentRow}
      class:unavailable={track.unavailable}
      role="button"
      aria-disabled={track.unavailable ? "true" : undefined}
      title={track.unavailable ? (track.unavailable_reason || "Unavailable") : undefined}
      tabindex="-1"
      ondblclick={() => onRowDblClick(i)}
    >
      <span class="c-idx">
        <span class="n">{i + 1}</span>
        <span class="eq"><i></i><i></i><i></i><i></i></span>
        <button
          class="go"
          title={track.unavailable ? (track.unavailable_reason || "Unavailable") : "Play"}
          disabled={track.unavailable}
          onclick={() => onPlayIconClick(i)}
        >
          <Icon name={i === currentRow && playback.playing ? "pause" : "play"} size={12} />
        </button>
      </span>

      {#if colArt}
        <Cover
          src={track.cover_url}
          id={track.album_id || track.uri}
          name={track.album_name || track.name}
          size={36}
          class="c-art"
        />
      {/if}

      <!-- Two lines, always: the name, then who made it. The artist is not a
           column any more (see the block at the top of this file). -->
      <span class="c-title">
        <span class="t-name">{track.name}</span>
        <span class="t-sub">
          {#if track.cached}
            <!-- Status, not action. It sits before the names because that is
                 where the official client puts it and because it qualifies the
                 whole line rather than any one artist. -->
            <!-- 13px, not 10. At 10 the punched arrow inside the disc was too
                 small to resolve as an arrow at all — it read as a dot, which
                 is a mark that says "something" rather than "downloaded". -->
            <span class="t-cached" title="Downloaded — this plays from the local cache">
              <Icon name="cached" size={13} />
              <span class="sr-only">Downloaded</span>
            </span>
          {/if}
          <ArtistLinks
            class="t-artists"
            names={track.artist_names}
            ids={track.artist_ids ?? []}
            id={track.artist_id}
          />
        </span>
      </span>

      {#if colAlbum}
        <!-- Always rendered, even when it repeats the title (single-track
             releases). Blanking those cells leaves holes that read as data
             that failed to load, which is worse than mild redundancy. -->
        <button class="c-album" onclick={() => track.album_id && navigate("album", track.album_id)}>
          {track.album_name}
        </button>
      {/if}

      {#if colAdded}
        <span class="c-added">{formatAddedAt(track.added_at)}</span>
      {/if}

      {#if colPlays}
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
        {#if rowActionLabel && onRowAction}
          <button
            style="width:56px;opacity:1"
            title={rowActionDisabled?.(track, i) ? "Adding…" : rowActionLabel}
            disabled={!!rowActionDisabled?.(track, i)}
            onclick={() => onRowAction(track, i)}
          >{rowActionDisabled?.(track, i) ? "Adding…" : rowActionLabel}</button>
        {:else}
          <button title="More options" onclick={(e) => openRowMenu(e, track, i)}>
            <Icon name="more" size={15} />
          </button>
        {/if}
      </span>
    </div>
  {/each}
{/snippet}

<div
  class="tl"
  class:dense
  bind:this={rootEl}
  style="overflow-anchor: none"
  style:--cols={cols}
>
  {#if showHead}
    <!-- Parked where the head starts sticking; see the observer above. -->
    <div class="tl-head-sentinel" bind:this={headSentinel} aria-hidden="true"></div>
    <div class="tl-head" class:stuck={headStuck}>
      <button
        class="tl-sort-btn"
        class:active={sortKey === "order"}
        aria-label={sortAriaLabel("order", "original/custom order")}
        aria-pressed={sortKey === "order"}
        onclick={() => activateSort("order")}
      ># {#if sortKey === "order"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {#if colArt}<span></span>{/if}
      <!-- ONE grid cell holding TWO controls, because the cell it names holds
           two things now. Splitting the head this way is what keeps sorting by
           artist available after the artist column was folded under the title,
           without adding a separate sort control to every page that has a
           table. Still exactly one cell — the `--cols` invariant above depends
           on the head and the row agreeing on their count. -->
      <span class="tl-title-head">
        <button
          class="tl-sort-btn"
          class:active={sortKey === "title"}
          aria-label={sortAriaLabel("title", "title")}
          aria-pressed={sortKey === "title"}
          onclick={() => activateSort("title")}
        >Title {#if sortKey === "title"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
        <span class="tl-head-sep" aria-hidden="true"></span>
        <button
          class="tl-sort-btn tl-artist-sort"
          class:active={sortKey === "artist"}
          aria-label={sortAriaLabel("artist", "artist")}
          aria-pressed={sortKey === "artist"}
          onclick={() => activateSort("artist")}
        >Artist {#if sortKey === "artist"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      </span>
      {#if colAlbum}
        <button
          class="tl-sort-btn"
          class:active={sortKey === "album"}
          aria-label={sortAriaLabel("album", "album")}
          aria-pressed={sortKey === "album"}
          onclick={() => activateSort("album")}
        >Album {#if sortKey === "album"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if colAdded}
        <button
          class="tl-sort-btn"
          class:active={sortKey === "added"}
          aria-label={sortAriaLabel("added", "date added")}
          aria-pressed={sortKey === "added"}
          onclick={() => activateSort("added")}
        >Added {#if sortKey === "added"}<span class="tl-sort-indicator" aria-hidden="true">{sortDirection === "asc" ? "↑" : "↓"}</span>{/if}</button>
      {/if}
      {#if colPlays}<span class="tl-plays-head">Plays</span>{/if}
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
  <div
    class="menu"
    popover="manual"
    use:topLayer
    style:left="{menu.x}px"
    style:top={menu.top === null ? null : menu.top + "px"}
    style:bottom={menu.bottom === null ? null : menu.bottom + "px"}
    style:max-height="{menu.maxH}px"
  >
    <button class="menu-item" onclick={() => { menu.open = false; api.addQueue(menu.track, queueSource).catch(() => {}); }}>
      Add to queue
    </button>
    {#if allowAddToPlaylist}
      <button class="menu-item" onclick={openPicker}>Add to playlist…</button>
    {/if}
    {#if trackLink(menu.track)}
      <!-- The confirmation lives on the item, which is why the menu does not
           close on click: a copy with no feedback is indistinguishable from a
           dead control, and a toast system for one line of text is not worth
           having. The check is foam — this is a thing you just did. -->
      <!-- No icon. It was the only item in the menu that had one, so its label
           started 21px to the right of every other label and the column of text
           read as broken. The confirmation is the label itself changing, plus
           the foam colour, which is enough to show the copy happened. -->
      <button class="menu-item" class:done={menu.copied} onclick={copyTrackLink}>
        {menu.copied ? "Link copied" : "Copy link"}
      </button>
    {/if}
    {#if menu.track?.id}
      <button class="menu-item" onclick={() => { menu.open = false; openCredits(menu.track); }}>
        View credits
      </button>
    {/if}
    {#if menu.track?.id}
      <button
        class="menu-item"
        onclick={() => {
          menu.open = false;
          openTrackEditor(menu.track, playlistId);
        }}
      >
        Edit playback…
      </button>
    {/if}
    {#if playlistId && menu.editDefined}
      <button
        class="menu-item"
        disabled={menu.editLoading}
        onclick={toggleEditedVersion}
      >
        {menu.editEnabled ? "✓ Use edited version" : "Use edited version"}
      </button>
    {/if}
    {#if menu.track?.id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("radio", menu.track.id); }}>
        Go to song radio
      </button>
    {/if}
    {#if menu.track?.album_id}
      <button class="menu-item" onclick={() => { menu.open = false; navigate("album", menu.track.album_id); }}>
        Go to album
      </button>
    {/if}
    <!-- One row whatever the track. A collaboration opens a submenu rather than
         printing an extra near-identical line per artist. -->
    {#if menuArtists.length}
      <button class="menu-item" onclick={goToArtist}>
        {menuArtists.length > 1 ? "Go to artist…" : "Go to artist"}
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
  <div
    class="menu"
    popover="manual"
    use:topLayer
    style:left="{picker.x}px"
    style:top={picker.top === null ? null : picker.top + "px"}
    style:bottom={picker.bottom === null ? null : picker.bottom + "px"}
    style:max-height="{picker.maxH}px"
  >
    {#each library as pl (pl.id)}
      <button class="menu-item" onclick={() => addToPlaylist(pl)}>{pl.name}</button>
    {/each}
  </div>
{/if}

{#if artistPicker.open}
  <div
    class="menu"
    popover="manual"
    use:topLayer
    style:left="{artistPicker.x}px"
    style:top={artistPicker.top === null ? null : artistPicker.top + "px"}
    style:bottom={artistPicker.bottom === null ? null : artistPicker.bottom + "px"}
    style:max-height="{artistPicker.maxH}px"
  >
    {#each artistPicker.artists as artist (artist.id)}
      <button
        class="menu-item"
        onclick={() => {
          menu.open = false;
          artistPicker.open = false;
          navigate("artist", artist.id);
        }}
      >{artist.name}</button>
    {/each}
  </div>
{/if}

<style>
  /* `max-height` is set INLINE, from the room actually available above or
     below the anchor — see placePopover. It used to be `60vh` here, which is a
     number that knows nothing about where the menu was put, so a menu that
     flipped upward near the top of the list was clamped to the window and slid
     under the topbar's glass with its first items unreachable. */
  /* `inset` and `margin` are overrides of the UA's own `[popover]` rule, which
     sets `inset: 0; margin: auto` to centre a popover in the viewport. The
     three declarations that follow it are the position; the UA's `right: 0`
     would otherwise stretch every menu to the right edge of the window. */
  .menu {
    position: fixed;
    inset: auto;
    margin: 0;
    overflow-y: auto;
  }
  .menu-item.done { color: var(--accent); }
  .menu-item.done :global(.icon) { color: var(--accent); }
</style>
