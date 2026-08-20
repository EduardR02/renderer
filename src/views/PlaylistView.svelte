<script>
  import { detail, api, playback, navigate, route, promotePlaylist, togglePlay } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { formatTotal } from "../lib/time.js";

  const pl = $derived(detail.playlist);
  const tracks = $derived(pl?.tracks ?? []);

  const sortState = $state({ key: "order", direction: "asc" });

  function textSortValue(track, key) {
    if (key === "artist") {
      return (track?.artist_names ?? []).filter(Boolean).join(", ").trim();
    }
    if (key === "album") return String(track?.album_name ?? "").trim();
    return String(track?.name ?? "").trim();
  }

  function compareOptionalText(left, right, direction) {
    const leftMissing = !left;
    const rightMissing = !right;
    if (leftMissing || rightMissing) {
      if (leftMissing && rightMissing) return 0;
      // Missing metadata stays at the end in either direction.
      return leftMissing ? 1 : -1;
    }
    return left.localeCompare(right, undefined, { numeric: true, sensitivity: "base" }) * direction;
  }

  function compareOptionalNumber(left, right, direction) {
    const leftMissing = !Number.isFinite(left) || left <= 0;
    const rightMissing = !Number.isFinite(right) || right <= 0;
    if (leftMissing || rightMissing) {
      if (leftMissing && rightMissing) return 0;
      return leftMissing ? 1 : -1;
    }
    return (left - right) * direction;
  }

  const sortedTracks = $derived.by(() => {
    // No sort is the common case and the list can be thousands long; the
    // comparator below would run and decide nothing.
    if (sortState.key === null) return tracks;
    const direction = sortState.direction === "asc" ? 1 : -1;
    return tracks
      .map((track, originalIndex) => ({ track, originalIndex }))
      .sort((left, right) => {
        let comparison = 0;
        switch (sortState.key) {
          case "title":
          case "artist":
          case "album":
            comparison = compareOptionalText(
              textSortValue(left.track, sortState.key),
              textSortValue(right.track, sortState.key),
              direction,
            );
            break;
          case "added":
            comparison = compareOptionalNumber(
              Number(left.track?.added_at),
              Number(right.track?.added_at),
              direction,
            );
            break;
          case "duration":
            comparison = compareOptionalNumber(
              Number(left.track?.duration_ms),
              Number(right.track?.duration_ms),
              direction,
            );
            break;
          case "order":
          default:
            comparison = (left.originalIndex - right.originalIndex) * direction;
            break;
        }
        // Explicit original-index tiebreaking keeps equal/missing metadata
        // stable even on runtimes whose Array#sort implementation changes.
        return comparison || left.originalIndex - right.originalIndex;
      })
      .map(({ track }) => track);
  });

  /**
   * Ascending, then descending, then off.
   *
   * Without the third step a sort can be changed but never undone: the
   * playlist's own order is a real choice, and returning to it should not
   * require leaving the page.
   */
  function toggleSort(key) {
    if (sortState.key !== key) {
      sortState.key = key;
      sortState.direction = "asc";
    } else if (sortState.direction === "asc") {
      sortState.direction = "desc";
    } else {
      sortState.key = null;
      sortState.direction = "asc";
    }
  }

  /* Fallback for a playlist the backend has not swept yet: derive the mosaic
     candidates from the tracks we already have on screen. */
  const artPool = $derived([...new Set(tracks.map((t) => t.cover_url).filter(Boolean))].slice(0, 4));

  /**
   * The page's colour, read out of the artwork.
   *
   * A playlist's own cover if it has one; otherwise the first track's, because
   * Spotify's rootlist ships no playlist covers at all and a mosaic of four
   * sleeves has no single colour anyway — the first one is at least a real
   * colour from the record you are about to hear. With neither, `coverTone`
   * falls back to the same id-hashed identity hue the generated tile uses, so
   * the header and the tile still agree.
   */
  const tone = $derived(coverTone(pl?.cover_url || artPool[0] || "", pl?.id ?? ""));

  let renaming = $state(false);
  let nameDraft = $state("");
  let renameInput = $state(null);
  let renameSaving = $state(false);
  let renameError = $state("");
  let menuOpen = $state(false);
  let menuButton = $state(null);
  let menu = $state(null);
  let deleteOpen = $state(false);
  let deleting = $state(false);
  let deleteError = $state("");

  $effect(() => {
    if (!renaming) return;
    queueMicrotask(() => {
      renameInput?.focus();
      renameInput?.select();
    });
  });

  $effect(() => {
    if (!menuOpen) return;
    queueMicrotask(() => menu?.querySelector('[role="menuitem"]')?.focus());

    function onPointerDown(event) {
      if (!menu?.contains(event.target) && !menuButton?.contains(event.target)) menuOpen = false;
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });
  /**
   * Library ordering is most-recently-*played*, not most-recently-opened, and
   * a track URI cannot say which playlist it was played from — the same track
   * sits in many. This view is the only place that knows, so it reports it.
   */
  function markPlayed() {
    if (!pl) return;
    // Reorder synchronously so the click is reflected before the persistence
    // command returns. The backend remains the source of durable timestamps.
    promotePlaylist(pl.id);
    api.touchPlaylist(pl.id).catch(() => {});
  }

  function playFrom(i) {
    if (!pl) return;
    const queue = sortedTracks;
    if (!queue[i]) return;
    markPlayed();
    // The displayed order is a local projection; enqueue that exact array so
    // a click after sorting still targets the row the user selected.
    api.playQueue(queue, i).catch(() => {});
  }

  /**
   * Whether what is playing came from this playlist.
   *
   * Judged by the queue's contents rather than by a "current context" the
   * engine does not report: if the playing track is one of ours *and* the
   * queue is the same length, this playlist is what is on. Cheap, and wrong
   * only for the case of two identical-length playlists sharing the current
   * track — where either answer is defensible.
   */
  const playingThis = $derived.by(() => {
    const uri = playback.current_uri;
    if (!uri || !tracks.length) return false;
    if (playback.queue.length !== tracks.length) return false;
    return tracks.some((track) => track.uri === uri);
  });

  /**
   * Play, or pause/resume what this playlist already started.
   *
   * Restarting from track one when the playlist is already playing is the
   * behaviour of a button that has not been told what is going on: the glyph
   * says play while music from this very list is coming out of the speakers,
   * and pressing it throws away the listener's position.
   */
  function playOrToggle() {
    if (playingThis) {
      togglePlay();
      return;
    }
    const queue = sortedTracks;
    if (!queue.length) return;
    markPlayed();
    api.playQueue(queue, 0).catch(() => {});
  }

  function shufflePlay() {
    const queue = sortedTracks;
    if (!queue.length) return;
    markPlayed();
    api.setShuffle(true).catch(() => {});
    api.playQueue(queue, 0).catch(() => {});
  }

  function closeMenu(returnFocus = false) {
    menuOpen = false;
    if (returnFocus) queueMicrotask(() => menuButton?.focus());
  }

  function toggleMenu() {
    if (menuOpen) closeMenu(true);
    else menuOpen = true;
  }

  function onMenuKeyDown(event) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu(true);
      return;
    }
    if (event.key === "Tab") {
      closeMenu();
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const items = [...menu.querySelectorAll('[role="menuitem"]:not(:disabled)')];
    const current = items.indexOf(document.activeElement);
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? items.length - 1
          : event.key === "ArrowDown"
            ? (current + 1) % items.length
            : (current - 1 + items.length) % items.length;
    items[next]?.focus();
  }

  function startRename() {
    closeMenu();
    nameDraft = pl?.name ?? "";
    renameError = "";
    renaming = true;
  }

  function cancelRename() {
    if (renameSaving) return;
    renaming = false;
    nameDraft = "";
    renameError = "";
  }

  async function commitRename() {
    const n = nameDraft.trim();
    if (!pl || renameSaving) return;
    if (!n) {
      renameError = "Playlist name cannot be empty.";
      return;
    }
    if (n === pl.name) {
      cancelRename();
      return;
    }
    renameSaving = true;
    renameError = "";
    try {
      await api.renamePlaylist(pl.id, n);
      renaming = false;
    } catch (error) {
      renameError = error instanceof Error ? error.message : String(error || "Could not rename this playlist.");
    } finally {
      renameSaving = false;
    }
  }

  function requestDelete() {
    closeMenu();
    deleteError = "";
    deleteOpen = true;
  }

  async function deletePlaylist() {
    if (!pl || deleting) return;
    deleting = true;
    deleteError = "";
    try {
      await api.deletePlaylist(pl.id);
      deleteOpen = false;
      navigate("library");
    } catch (error) {
      deleteError = error instanceof Error ? error.message : String(error || "Could not delete this playlist.");
    } finally {
      deleting = false;
    }
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
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
        <span class="tag">Playlist</span>
        {#if renaming}
          <form
            class="rename-form"
            aria-label="Rename playlist"
            onsubmit={(e) => {
              e.preventDefault();
              commitRename();
            }}
          >
            <input
              class="rename"
              bind:this={renameInput}
              bind:value={nameDraft}
              aria-label="Playlist name"
              aria-invalid={!!renameError}
              disabled={renameSaving}
              onkeydown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  cancelRename();
                }
              }}
              spellcheck="false"
            />
            <div class="rename-controls">
              <button class="btn-accent" type="submit" disabled={renameSaving}>
                {renameSaving ? "Saving…" : "Save"}
              </button>
              <button class="btn-ghost" type="button" disabled={renameSaving} onclick={cancelRename}>Cancel</button>
            </div>
            {#if renameError}<p class="inline-error" role="alert">{renameError}</p>{/if}
          </form>
        {:else}
          <h1 class="detail-title">{pl.name}</h1>
        {/if}
        <p class="detail-meta">
          <span class="who">{pl.owner}</span>
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button
            class="play-lg"
            title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play"}
            onclick={playOrToggle}
            disabled={!tracks.length}
          >
            <Icon name={playingThis && playback.playing ? "pause" : "play"} size={19} />
          </button>
          <button class="btn-ghost" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={14} />Shuffle
          </button>
          <div class="playlist-menu-wrap">
            <button
              class="btn-icon"
              bind:this={menuButton}
              aria-label="Playlist actions"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-controls="playlist-actions-menu"
              title="Playlist actions"
              onclick={toggleMenu}
            >
              <Icon name="more" size={18} />
            </button>
            {#if menuOpen}
              <div
                id="playlist-actions-menu"
                class="menu playlist-actions-menu"
                role="menu"
                tabindex="-1"
                bind:this={menu}
                onkeydown={onMenuKeyDown}
              >
                <button class="menu-item" role="menuitem" onclick={startRename}>Rename playlist</button>
                <div class="menu-sep" role="separator"></div>
                <button class="menu-item danger" role="menuitem" onclick={requestDelete}>Delete playlist…</button>
              </div>
            {/if}
          </div>
        </div>
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList
          tracks={sortedTracks}
          {playFrom}
          playlistId={pl.id}
          showArtist
          showAdded
          sortKey={sortState.key}
          sortDirection={sortState.direction}
          onSort={toggleSort}
        />
      </div>
    {:else}
      <div class="empty">
        <p>No songs here yet.</p>
        <p class="sub">Find something in Search and add it to this playlist.</p>
      </div>
    {/if}
  {/if}
</section>
{#if deleteOpen}
  <ConfirmDialog
    open
    title="Delete playlist?"
    message={`“${pl?.name ?? "This playlist"}” will be removed from your library. This cannot be undone.`}
    confirmLabel="Delete playlist"
    busyLabel="Deleting…"
    busy={deleting}
    error={deleteError}
    onConfirm={deletePlaylist}
    onCancel={() => {
      deleteOpen = false;
      deleteError = "";
    }}
  />
{/if}
