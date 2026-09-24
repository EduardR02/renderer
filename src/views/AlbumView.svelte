<script>
  import { detail, api, ui, navigate, retryDetail } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ArtistLinks from "../components/ArtistLinks.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { spotifyLink, writeClipboard } from "../lib/spotify-link.js";
  import { formatTotal } from "../lib/time.js";
  import { detailArtSize } from "../lib/layout.js";

  const album = $derived(detail.album);
  /* The sleeve gives way before the title does when the pane is narrow. */
  const artSize = $derived(detailArtSize(ui.paneWidth));
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
  let shuffleBusy = $state(false);
  let actionError = $state("");
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
  $effect(() => {
    album?.id;
    actionError = "";
    shuffleBusy = false;
    menuOpen = false;
    copyState = "idle";
  });

  /* The menu hangs off its button the way the playlist header's does: focus
     the first item on open, close on Escape/Tab or a pointerdown outside it,
     and give the button its focus back when it closes. */
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

  /**
   * The link is the album's own id — the page this one is drawn from — so a
   * paste into our search lands back here.
   *
   * The menu STAYS OPEN on copy, because the confirmation is on the item
   * itself and dismissing it would take the only feedback with it. A copy that
   * landed closes it shortly after, through the same close the keyboard paths
   * use, so the button gets its focus back. A refused write stays up and says
   * so: the label that never changes is the dead control the confirmation
   * exists to prevent, and the item is also the retry.
   */
  async function copyLink() {
    const link = spotifyLink("album", album?.id);
    if (!link) return;
    copyState = (await writeClipboard(link)) ? "copied" : "failed";
    clearTimeout(copyTimer);
    if (copyState === "copied") copyTimer = setTimeout(() => closeMenu(true), 900);
  }



  function playFrom(i) {
    if (tracks.length) api.playQueue(tracks, i, `album:${album?.id ?? ""}`).catch(() => {});
  }

  async function shufflePlay() {
    if (!tracks.length || shuffleBusy) return;
    const id = album?.id ?? "";
    const queue = [...tracks];
    shuffleBusy = true;
    actionError = "";
    try {
      await api.setShuffle(true);
      await api.playQueue(queue, 0, `album:${id}`);
    } catch (reason) {
      if (album?.id === id) {
        actionError = String(reason || "Could not shuffle this album.");
      }
    } finally {
      if (album?.id === id) shuffleBusy = false;
    }
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if detail.error && !album}
    <!-- The request failed, so this page stays a frame with an explanation in
         it rather than a skeleton that never resolves. -->
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">Album</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This release could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="btn-ghost" onclick={retryDetail}>Try again</button>
        <button class="btn-ghost" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if !album}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <!-- The frame the record arrives into: tag, title, meta line and the
           two controls, each at the size of the thing that replaces it, so
           nothing on the page moves when the payload lands. -->
      <div>
        <span class="skeleton line sm" style="width:56px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(420px,70%)"></span>
        <span class="skeleton line sm" style="width:180px"></span>
        <div class="actions">
          <span class="skeleton" style="width:48px;height:48px;border-radius:var(--rf)"></span>
          <span class="skeleton" style="width:104px;height:32px;border-radius:var(--r2)"></span>
        </div>
      </div>
    </header>
    <div class="tl" style="margin-top:var(--s6);--cols:28px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 8 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{58 - ((i * 7) % 22)}%"></span>
            <span class="sk b" style="width:{28 - ((i * 5) % 10)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover src={album.cover_url} id={album.id} name={album.name} size={artSize} lg raised />
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
            <Icon name="play" size={22} />
          </button>
          <button class="btn-ghost" onclick={shufflePlay} disabled={!tracks.length || shuffleBusy}>
            <Icon name="shuffle" size={14} />{shuffleBusy ? "Starting…" : "Shuffle"}
          </button>
          <div class="head-menu-wrap">
            <button
              class="btn-icon"
              bind:this={menuButton}
              aria-label="Album actions"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-controls="album-actions-menu"
              title="Album actions"
              onclick={toggleMenu}
            >
              <Icon name="more" size={18} />
            </button>
            {#if menuOpen}
              <div
                id="album-actions-menu"
                class="menu head-menu glass-overlay"
                role="menu"
                tabindex="-1"
                bind:this={menu}
                onkeydown={onMenuKeyDown}
              >
                <!-- The confirmation lives on the item, which is why the menu
                     does not close on click: a copy with no feedback is
                     indistinguishable from a dead control. -->
                <button
                  class="menu-item"
                  role="menuitem"
                  class:done={copyState === "copied"}
                  class:failed={copyState === "failed"}
                  onclick={copyLink}
                >
                  {copyState === "copied" ? "Link copied" : copyState === "failed" ? "Copy failed" : "Copy link"}
                </button>
              </div>
            {/if}
          </div>
          {#if actionError}<span class="inline-error" role="alert">{actionError}</span>{/if}
        </div>
      </div>
    </header>

    {#if tracks.length}
      <!-- No album column and no per-row thumbnail: the art is already the
           largest thing on the page, and every row would repeat it. -->
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} showAlbum={false} showArt={false} showPlays queueContext={`album:${album?.id ?? ""}`} />
      </div>
    {:else}
      <div class="empty">
        <p class="h">No tracks on this release.</p>
        <p class="sub">Nothing came back from the engine for this album.</p>
      </div>
    {/if}
  {/if}
</section>

<style>
  .head-menu-wrap { position: relative; }
  /* Right-aligned because this button ends the row, and a 216px menu opening
     to the right of it would leave the pane. */
  .head-menu { position: absolute; z-index: 1; top: calc(100% + var(--s1)); right: 0; }
  /* A copy that landed: the same label-and-colour pairing the row menu uses.
     A refused write takes the failure colour, so the two are never read as
     the same event. */
  .menu-item.done { color: var(--accent); }
  .menu-item.failed { color: var(--love); }
</style>
