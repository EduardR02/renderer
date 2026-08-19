<script>
  import { detail, api, playback, navigate, route, promotePlaylist } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
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

  /* Same deterministic palette mapping as generated artwork, so the header
     wash and identity tile stay related without introducing extra hues. */
  const washTone = $derived.by(() => {
    const seed = pl?.id ?? "";
    let h = 0x811c9dc5;
    for (let i = 0; i < seed.length; i++) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return ["var(--rose)", "var(--foam)", "var(--love)"][h % 3];
  });

  /* Fallback for a playlist the backend has not swept yet: derive the mosaic
     candidates from the tracks we already have on screen. */
  const artPool = $derived([...new Set(tracks.map((t) => t.cover_url).filter(Boolean))].slice(0, 4));

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

  function playAll() {
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

<section class="view page wash" style:--wash={washTone}>
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
          <button class="play-lg" title="Play" onclick={playAll} disabled={!tracks.length}>
            <Icon name="play" size={19} />
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
    busy={deleting}
    error={deleteError}
    onConfirm={deletePlaylist}
    onCancel={() => {
      deleteOpen = false;
      deleteError = "";
    }}
  />
{/if}
