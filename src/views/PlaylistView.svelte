<script module>
  /**
   * ONE collator, not one per comparison.
   *
   * `String#localeCompare` builds a collator on every call when it is handed
   * an options object, and the text sort made tens of thousands of those calls
   * to order a 5,000-row playlist. Collation is pure and these options never
   * change, so a single instance serves every sort — module scope rather than
   * the component's own script, because opening the next playlist should not
   * build another one.
   */
  const SORT_COLLATOR = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
</script>

<script>
  import {
    detail,
    api,
    playback,
    navigate,
    route,
    promotePlaylist,
    togglePlay,
    ui,
    session,
    retryDetail,
    loadPlaylistRecommendations,
    removePlaylistRecommendation,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import PlaylistCleanup from "../components/PlaylistCleanup.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { spotifyLink, writeClipboard } from "../lib/spotify-link.js";
  import { formatTotal } from "../lib/time.js";
  import { detailArtSize } from "../lib/layout.js";

  const pl = $derived(detail.playlist);
  /* The cover gives way before the title does when the pane is narrow. */
  const artSize = $derived(detailArtSize(ui.paneWidth));
  const tracks = $derived(pl?.tracks ?? []);
  const editable = $derived(
    !!pl?.owner_id && !!session.username && pl.owner_id === session.username,
  );

  const sortState = $state({ key: "order", direction: "asc" });
  const SORT_STORAGE_PREFIX = "playlist-sort:";
  const SORT_KEYS = new Set(["order", "title", "artist", "album", "added", "duration"]);
  const SORT_DIRECTIONS = new Set(["asc", "desc"]);

  function defaultSort() {
    return { key: "order", direction: "asc" };
  }

  function readSort(id) {
    const fallback = defaultSort();
    if (!id) return fallback;
    try {
      const saved = JSON.parse(localStorage.getItem(`${SORT_STORAGE_PREFIX}${id}`) ?? "null");
      if (
        !saved ||
        typeof saved !== "object" ||
        (saved.key !== null && !SORT_KEYS.has(saved.key)) ||
        !SORT_DIRECTIONS.has(saved.direction)
      ) {
        return fallback;
      }
      return { key: saved.key, direction: saved.direction };
    } catch {
      return fallback;
    }
  }

  function writeSort(id, key, direction) {
    if (
      !id ||
      (key !== null && !SORT_KEYS.has(key)) ||
      !SORT_DIRECTIONS.has(direction)
    ) {
      return;
    }
    try {
      localStorage.setItem(
        `${SORT_STORAGE_PREFIX}${id}`,
        JSON.stringify({ key, direction }),
      );
    } catch {
      /* private mode / storage disabled: sorting remains session-local */
    }
  }

  $effect(() => {
    if (route.name !== "playlist" || !route.id) return;
    const saved = readSort(route.id);
    sortState.key = saved.key;
    sortState.direction = saved.direction;
  });

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
    return SORT_COLLATOR.compare(left, right) * direction;
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

  /**
   * The sort key for one row, built ONCE per row instead of inside the
   * comparator.
   *
   * The comparator used to rebuild both operands' keys on every comparison —
   * tens of thousands of string builds to sort a 5,000-row playlist, measured
   * at ~80ms for an artist sort. A key is a function of the row alone, so it
   * belongs in the same pass that decorates the row with its original index:
   * the work is then proportional to the rows, and the comparator does
   * nothing but compare.
   */
  function sortKeyFor(track, key) {
    switch (key) {
      case "title":
      case "artist":
      case "album":
        return textSortValue(track, key);
      case "added":
        return Number(track?.added_at);
      case "duration":
        return Number(track?.duration_ms);
      default:
        return 0;
    }
  }

  const sortedTracks = $derived.by(() => {
    // No sort is the common case and the list can be thousands long; the
    // comparator below would run and decide nothing.
    if (sortState.key === null || (sortState.key === "order" && sortState.direction === "asc")) return tracks;
    const sortKey = sortState.key;
    const direction = sortState.direction === "asc" ? 1 : -1;
    return tracks
      .map((track, originalIndex) => ({
        track,
        originalIndex,
        sortValue: sortKeyFor(track, sortKey),
      }))
      .sort((left, right) => {
        let comparison = 0;
        switch (sortKey) {
          case "title":
          case "artist":
          case "album":
            comparison = compareOptionalText(left.sortValue, right.sortValue, direction);
            break;
          case "added":
          case "duration":
            comparison = compareOptionalNumber(left.sortValue, right.sortValue, direction);
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
    if (!SORT_KEYS.has(key)) return;
    if (sortState.key !== key) {
      sortState.key = key;
      sortState.direction = "asc";
    } else if (sortState.direction === "asc") {
      sortState.direction = "desc";
    } else {
      sortState.key = null;
      sortState.direction = "asc";
    }
    writeSort(route.id, sortState.key, sortState.direction);
  }

  /**
   * Manual reorder is meaningful only against the playlist's own sequence,
   * and BOTH "#" directions show it: ascending is storage order itself,
   * descending the same sequence read backwards — reorderTracks translates
   * view positions back to storage coordinates for it. Unsorted (key null)
   * is storage order too. Any other projection is a view, not storage:
   * dragging inside it would move rows the server never placed there, so
   * the table keeps its drag-out (add/move elsewhere) but takes no drops.
   */
  const ownOrderShown = $derived(sortState.key === null || sortState.key === "order");

  /**
   * Applies the landed position immediately — the row must be where the drop
   * put it the moment the ghost dissolves — then lets the backend confirm.
   *
   * `from`/`to` are VIEW indices. Under "# descending" the view mirrors
   * storage, so both ends translate (view v lives at n-1-v) before the
   * splice and the backend MOV touch storage coordinates; a two-slot drag
   * at the top of the view is then the same two-slot move, read from the
   * other end. The `playlist` refresh that follows replaces `tracks`
   * wholesale anyway, because its row order differs; the revert below only
   * covers the window where Spotify refused the MOV and no refresh has
   * landed yet.
   */
  async function reorderTracks(from, to) {
    const list = detail.playlist?.tracks;
    if (!Array.isArray(list) || from === to || from < 0 || to < 0) return;
    if (from >= list.length || to >= list.length) return;
    const mirrored = sortState.key === "order" && sortState.direction === "desc";
    const last = list.length - 1;
    const actualFrom = mirrored ? last - from : from;
    const actualTo = mirrored ? last - to : to;
    const [moved] = list.splice(actualFrom, 1);
    list.splice(actualTo, 0, moved);
    try {
      await api.reorderPlaylistTracks(pl.id, actualFrom, actualTo);
    } catch {
      const [taken] = list.splice(actualTo, 1);
      list.splice(actualFrom, 0, taken);
    }
  }

  /* Fallback for a playlist the backend has not swept yet: derive the mosaic
     candidates from the tracks we already have on screen. */
  const artPool = $derived.by(() => {
    if (pl?.cover_url) return [];
    const covers = [];
    const seen = new Set();
    for (const track of tracks) {
      const cover = track?.cover_url;
      if (!cover || seen.has(cover)) continue;
      seen.add(cover);
      covers.push(cover);
      if (covers.length === 4) break;
    }
    return covers;
  });

  /**
   * The page's colour, read out of the artwork.
   *
   * A playlist's own cover if it has one; otherwise the whole mosaic, the same
   * list and in the same order the header's `<Cover>` is about to draw from.
   * It used to be `artPool[0]`, one arbitrary cell of four, which is how this
   * page opened brown beside three tiles that were not — `coverTone` now reads
   * every cell and keeps the one that most strongly has a colour. With
   * neither, it falls back to the same id-hashed identity hue the generated
   * tile uses, so the header and the tile still agree.
   */
  const tone = $derived(
    coverTone(pl?.cover_url || (pl?.cover_urls?.length ? pl.cover_urls : artPool), pl?.id ?? ""),
  );

  let renaming = $state(false);
  let nameDraft = $state("");
  let renameInput = $state(null);
  let renameSaving = $state(false);
  let renameError = $state("");
  let menuOpen = $state(false);
  let menuButton = $state(null);
  let menu = $state(null);
  /* Idle, landed, refused — the copy item's three faces. */
  let copyState = $state("idle");
  let copyTimer = 0;
  /* The confirmation outlives the menu by design, so the timer has to die with
     the page: a navigation inside its window would otherwise leave it writing
     to a component that is gone. */
  $effect(() => () => clearTimeout(copyTimer));
  let deleteOpen = $state(false);
  let deleting = $state(false);
  let deleteError = $state("");
  let cleanupId = $state(null);

  $effect(() => {
    if (cleanupId && (route.name !== "playlist" || route.id !== cleanupId || pl?.id !== cleanupId || !editable)) {
      cleanupId = null;
    }
  });
  let recommendations = $state([]);
  const recommendationAdds = $state({});
  const recommendationState = $state({
    id: null,
    revision: "",
    requested: false,
    loading: false,
    hidden: false,
  });
  let recommendationFooter = $state(null);

  $effect(() => {
    if (!renaming || !editable) return;
    queueMicrotask(() => {
      renameInput?.focus();
      renameInput?.select();
    });
  });

  /* The menu belongs to the page, not to an owner: every playlist has one.
     Only the items inside it are gated by `editable`. */
  $effect(() => {
    if (!menuOpen) return;
    queueMicrotask(() => menu?.querySelector('[role="menuitem"]')?.focus());

    function onPointerDown(event) {
      if (!menu?.contains(event.target) && !menuButton?.contains(event.target)) menuOpen = false;
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  });

  // Recommendation tracks live in the session cache. A different playlist or
  // snapshot starts a new lazy demand without carrying rows across routes.
  $effect(() => {
    const id = pl?.id ?? null;
    const revision = pl?.snapshot_id ?? "";
    if (recommendationState.id === id && recommendationState.revision === revision) return;
    recommendationState.id = id;
    recommendationState.revision = revision;
    recommendationState.requested = false;
    recommendationState.loading = false;
    recommendationState.hidden = false;
    recommendations = [];
  });

  $effect(() => {
    const node = recommendationFooter;
    const id = pl?.id;
    const revision = pl?.snapshot_id ?? "";
    if (!node || !id || recommendationState.hidden || recommendationState.requested) return;
    const root = node.closest(".scroll");
    if (!root) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) requestRecommendations(id, revision);
      },
      { root, rootMargin: "0px 0px 480px 0px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  async function requestRecommendations(
    expectedId = pl?.id,
    expectedRevision = pl?.snapshot_id ?? "",
    { force = false } = {},
  ) {
    const revision = expectedRevision ?? "";
    if (
      !expectedId ||
      recommendationState.id !== expectedId ||
      recommendationState.revision !== revision ||
      recommendationState.loading ||
      (recommendationState.requested && !force)
    ) return;
    recommendationState.requested = true;
    recommendationState.loading = true;
    recommendationState.hidden = false;
    try {
      const tracks = await loadPlaylistRecommendations(expectedId, revision, { force });
      if (
        recommendationState.id !== expectedId ||
        recommendationState.revision !== revision ||
        route.name !== "playlist" ||
        route.id !== expectedId
      ) return;
      recommendations = tracks;
      recommendationState.hidden = recommendations.length === 0;
    } catch {
      // An optional footer failure never invalidates the playlist. Leave an
      // existing successful set visible during a failed forced refresh.
      if (recommendationState.id === expectedId && recommendationState.revision === revision) {
        recommendationState.hidden = recommendations.length === 0;
      }
    } finally {
      if (recommendationState.id === expectedId && recommendationState.revision === revision) {
        recommendationState.loading = false;
      }
    }
  }


  function playRecommendation(index) {
    if (!recommendations[index]) return;
    api.playQueue(recommendations, index, `playlist:${pl?.id ?? ""}`).catch(() => {});
  }

  function recommendationAddKey(id, uri) {
    return `${id}\u0000${uri}`;
  }

  function recommendationAddDisabled(track) {
    const id = recommendationState.id;
    const uri = String(track?.uri ?? "").trim();
    return !!id && !!uri && !!recommendationAdds[recommendationAddKey(id, uri)];
  }

  async function addRecommendation(track) {
    const id = pl?.id;
    const revision = pl?.snapshot_id ?? "";
    const uri = String(track?.uri ?? "").trim();
    if (!id || !editable || !uri) return;
    const key = recommendationAddKey(id, uri);
    if (recommendationAdds[key]) return;
    recommendationAdds[key] = true;
    try {
      await api.addPlaylistTracks(id, [uri]);
      removePlaylistRecommendation(id, revision, uri);
      // A recommendation add is library activity, never playback.
      promotePlaylist(id);
      api.touchPlaylistActivity(id).catch(() => {});
      if (recommendationState.id === id && recommendationState.revision === revision) {
        recommendations = recommendations.filter((candidate) => candidate.uri !== uri);
        recommendationState.hidden = recommendations.length === 0;
      }
    } catch {
      // Keep the recommendation in place so a failed direct Add can be retried.
    } finally {
      delete recommendationAdds[key];
    }
  }
  /**
   * A successful queue command is the playback event for this local history.
   * The view knows the source playlist; the engine only receives track URIs and
   * cannot infer which followed playlist they came from.
   */
  function markPlayed() {
    if (!pl) return;
    promotePlaylist(pl.id, { played: true });
    api.touchPlaylist(pl.id).catch(() => {});
  }

  function playQueue(queue, index, options = undefined) {
    // Do not promote or persist a failed play. This keeps `last_played`
    // exclusive to actual playback, rather than an attempted command.
    // `options` is only ever `{ automaticStart: true }` from the header;
    // a clicked row passes nothing and plays exactly what was clicked.
    return api.playQueue(queue, index, `playlist:${pl?.id ?? ""}`, options).then(() => {
      markPlayed();
    });
  }

  function playFrom(i) {
    if (!pl) return;
    const queue = sortedTracks;
    if (!queue[i]) return;
    // The displayed order is a local projection; enqueue that exact array so
    // a click after sorting still targets the row the user selected.
    playQueue(queue, i).catch(() => {});
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

  /* ---------------- Skips ----------------
     The playlist's local skip preference arrives on the browse payload as
     `excluded_track_ids` and stays live in the detail store: TrackList patches
     THIS array when a row menu toggles, so this Set, the marks on screen and
     the header's idea of where to begin move together with no refetch. */
  const excludedTrackIds = $derived(pl?.excluded_track_ids ?? null);

  const skippedSet = $derived.by(
    () => new Set((excludedTrackIds ?? []).map(String)),
  );

  /**
   * Where AUTOMATIC playback may begin: the first row that is neither skipped
   * nor unavailable. Direct plays never consult this — clicking a skipped row
   * still plays that row, which is what makes the preference a preference.
   */
  const automaticStartIndex = $derived(
    sortedTracks.findIndex(
      (track) => !track.unavailable && !skippedSet.has(String(track.id)),
    ),
  );

  /**
   * Said once, calmly, beside the buttons when automatic playback has nowhere
   * legal to start. Derived rather than set-and-cleared, so including a row
   * again takes the sentence away without anyone remembering to.
   */
  const automaticStartBlocked = $derived(
    pl && tracks.length > 0 && automaticStartIndex < 0
      ? "Every song in this playlist is skipped or unavailable."
      : "",
  );

  /**
   * Play, or pause/resume what this playlist already started.
   *
   * Automatic starts begin at the first row the skip preference allows; a
   * start with nowhere legal to begin does nothing here — the sentence beside
   * the buttons explains why — and never falls through to an excluded row.
   */
  function playOrToggle() {
    if (playingThis) {
      togglePlay();
      return;
    }
    const queue = sortedTracks;
    if (!queue.length || automaticStartIndex < 0) return;
    playQueue(queue, automaticStartIndex, { automaticStart: true }).catch(() => {});
  }

  function shufflePlay() {
    const queue = sortedTracks;
    if (!queue.length || automaticStartIndex < 0) return;
    api.setShuffle(true).catch(() => {});
    playQueue(queue, automaticStartIndex, { automaticStart: true }).catch(() => {});
  }

  /**
   * Closes the menu. The button gets its focus back only when the menu was
   * holding it: the copy confirmation's timer fires while the pointer may be
   * anywhere by then, and a close that fires behind the user's back must not
   * pull the caret out of whatever they moved to.
   */
  function closeMenu(returnFocus = false) {
    const menuHadFocus = !!menu?.contains(document.activeElement);
    menuOpen = false;
    if (returnFocus && menuHadFocus) queueMicrotask(() => menuButton?.focus());
  }

  function toggleMenu() {
    if (menuOpen) {
      closeMenu(true);
      return;
    }
    /* A copy still counting down must not close the menu that replaces it. */
    clearTimeout(copyTimer);
    copyState = "idle";
    menuOpen = true;
  }

  /**
   * Sharing is not editing, so this is the one item every playlist gets,
   * ours and everybody else's alike — and the reason a playlist the user does
   * not own has an actions button at all. The link is the page's own id, which
   * is also the thing our search resolves back to here.
   *
   * The menu STAYS OPEN on copy, because the confirmation is on the item
   * itself and dismissing it would take the only feedback with it. A copy that
   * landed closes it shortly after, through the same close the keyboard paths
   * use, so the button gets its focus back. A refused write stays up and says
   * so: the label that never changes is the dead control the confirmation
   * exists to prevent, and the item is also the retry.
   */
  async function copyLink() {
    const link = spotifyLink("playlist", pl?.id);
    if (!link) return;
    copyState = (await writeClipboard(link)) ? "copied" : "failed";
    clearTimeout(copyTimer);
    if (copyState === "copied") copyTimer = setTimeout(() => closeMenu(true), 900);
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
    if (!editable) return;
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
    if (!pl || !editable || renameSaving) return;
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
    cleanupId = null;
    if (!editable) return;
    closeMenu();
    deleteError = "";
    deleteOpen = true;
  }

  async function deletePlaylist() {
    if (!pl || !editable || deleting) return;
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
  {#if detail.error && !pl}
    <!-- The request failed, so this page stays a frame with an explanation in
         it rather than a skeleton that never resolves. -->
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">Playlist</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This playlist could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="btn-ghost" onclick={retryDetail}>Try again</button>
        <button class="btn-ghost" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !pl}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <!-- Tag, title, meta and the two controls, each at the size of the thing
           that replaces it, so nothing moves when the playlist lands. -->
      <div>
        <span class="skeleton line sm" style="width:62px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(440px,72%)"></span>
        <span class="skeleton line sm" style="width:200px"></span>
        <div class="actions">
          <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
          <span class="skeleton" style="width:104px;height:32px;border-radius:var(--r2)"></span>
        </div>
      </div>
    </header>
    <div
      class="tl"
      style="margin-top:var(--s6);--cols:28px 36px minmax(0,1fr) 52px"
      aria-hidden="true"
    >
      {#each Array.from({ length: 9 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
            <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover
        src={pl.cover_url}
        srcs={pl.cover_urls?.length ? pl.cover_urls : artPool}
        id={pl.id}
        name={pl.name}
        size={artSize}
        lg
        raised
      />
      <div>
        <span class="tag">Playlist</span>
        {#if renaming && editable}
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
                {#if editable}
                  <button class="menu-item" role="menuitem" onclick={startRename}>Rename playlist</button>
                  <button class="menu-item" role="menuitem" disabled={!tracks.length} onclick={() => { closeMenu(); cleanupId = pl.id; }}>Remove songs by rules…</button>
                {/if}
                <!-- The confirmation lives on the item, which is why the menu
                     does not close on click: a copy with no feedback is
                     indistinguishable from a dead control. Sharing is not
                     editing, so this item is the whole menu for a playlist
                     that is not ours. -->
                <button
                  class="menu-item"
                  role="menuitem"
                  class:done={copyState === "copied"}
                  class:failed={copyState === "failed"}
                  onclick={copyLink}
                >
                  {copyState === "copied" ? "Link copied" : copyState === "failed" ? "Copy failed" : "Copy link"}
                </button>
                {#if editable}
                  <div class="menu-sep" role="separator"></div>
                  <button class="menu-item danger" role="menuitem" onclick={requestDelete}>Delete playlist…</button>
                {/if}
              </div>
            {/if}
          </div>
        </div>
        {#if automaticStartBlocked}
          <!-- Calm by design: grey type under the controls, not a red banner.
               Nothing failed — the listener chose this, and including any row
               again takes it away. -->
          <p class="start-blocked" role="alert">{automaticStartBlocked}</p>
        {/if}
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList
          tracks={sortedTracks}
          {playFrom}
          playlistId={editable ? pl.id : null}
          queueContext={`playlist:${pl?.id ?? ""}`}
          showAdded
          sortKey={sortState.key}
          sortDirection={sortState.direction}
          onSort={toggleSort}
          excludedTrackIds={excludedTrackIds}
          onReorder={editable && ownOrderShown ? reorderTracks : null}
        />
      </div>
    {:else}
      <div class="empty">
        <p>No songs here yet.</p>
        <p class="sub">Find something in Search and add it to this playlist.</p>
      </div>
    {/if}

    <div bind:this={recommendationFooter} aria-hidden={recommendationState.hidden ? "true" : undefined}>
      {#if !recommendationState.hidden}
        <section class="section" aria-labelledby="playlist-recommendations-title">
          <div class="section-head">
            <h2 class="section-title" id="playlist-recommendations-title">Recommended</h2>
            {#if recommendationState.requested}
              <button
                class="link-more"
                disabled={recommendationState.loading}
                onclick={() => requestRecommendations(pl.id, pl.snapshot_id, { force: true })}
              >
                {recommendationState.loading ? "Refreshing…" : "Refresh"}
              </button>
            {:else}
              <button class="link-more" onclick={() => requestRecommendations(pl.id, pl.snapshot_id)}>
                Load recommendations
              </button>
            {/if}
          </div>
          {#if recommendationState.loading && !recommendations.length}
            <div class="tl" style="--cols:28px 36px minmax(0,1fr) 52px" aria-label="Loading recommendations">
              {#each Array.from({ length: 4 }) as _, i (i)}
                <div class="sk-row">
                  <span class="sk" style="width:12px"></span>
                  <span class="sk art"></span>
                  <span class="sk-stack">
                    <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
                    <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
                  </span>
                  <span class="sk" style="width:28px;justify-self:end"></span>
                </div>
              {/each}
            </div>
          {:else if recommendations.length}
            <TrackList
              tracks={recommendations}
              playFrom={playRecommendation}
              showHead={false}
              queueContext={`playlist:${pl?.id ?? ""}`}
              allowAddToPlaylist={false}
              disableWindowing
              rowActionLabel={editable ? "Add" : null}
              rowActionDisabled={editable ? recommendationAddDisabled : null}
              onRowAction={editable ? addRecommendation : null}
            />
          {/if}
        </section>
      {/if}
    </div>
  {/if}
</section>
{#if deleteOpen && editable}
  <ConfirmDialog
    open={deleteOpen}
    title="Delete this playlist?"
    message={`"${pl?.name ?? "This playlist"}" and its ${tracks.length} songs are removed from your library.`}
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
{#if cleanupId && cleanupId === pl?.id && route.name === "playlist" && route.id === cleanupId && editable}
  {#key cleanupId}
    <PlaylistCleanup playlist={pl} onClose={() => { cleanupId = null; queueMicrotask(() => menuButton?.focus()); }} />
  {/key}
{/if}

<style>
  /* A copy that landed: the same label-and-colour pairing the row menu uses,
     and the one colour this app has for a thing you just did. A refused write
     takes the failure colour, so the two are never read as the same event. */
  .menu-item.done { color: var(--accent); }
  .menu-item.failed { color: var(--love); }
</style>
