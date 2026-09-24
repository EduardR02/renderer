<script module>
  import { boundedMisses } from "../lib/cover-work.js";

  /**
   * Urls the engine has answered with nothing, so the round trip is paid once
   * instead of once per tile.
   *
   * An empty answer is not a property of one tile: every tile that shows a url
   * shares it — the same record on a card and in a playlist's mosaic, the same
   * playlist in the rail and on its page — and resolving is an IPC round trip
   * per url per tile. Without this, a cover that cannot be fetched at all (a
   * delisted record, an engine that is not logged in) is asked for again by
   * every new tile that scrolls past. Bounded, because a session sees
   * thousands of urls and the memory must not grow with them — and a url is
   * only given up on once the empty answer has repeated recently (see
   * [`boundedMisses`]), because one empty answer can be the network, and two
   * of them a little later can be an outage.
   */
  const coverMisses = boundedMisses(512);

  /**
   * The top-layer pseudo-classes this engine understands, or `""`.
   *
   * A modal `<dialog>` and an open popover are laid out in the browser's top
   * layer, whose containing block is the viewport: the `.scroll` the sheet is
   * written inside stops being a containing block for anything in it, so an
   * observer rooted there measures a rectangle the tile is not laid out in and
   * reports `isIntersecting: false` for a tile that is plainly on screen. That
   * is the playlist cleanup sheet's preview rows: visible, with artwork, never
   * resolved.
   *
   * These selectors are how the top layer is asked about. An engine that does
   * not know one of them THROWS on the query rather than answering false,
   * which would take the whole resolve path down with it, so the ones it can
   * answer are found once, here. An empty answer leaves the scroller in
   * charge, exactly as before this existed.
   */
  const TOP_LAYER = [":modal", ":popover-open"]
    .filter((selector) => {
      try {
        document.documentElement.matches(selector);
        return true;
      } catch {
        return false;
      }
    })
    .join(", ");

  /**
   * The element a tile's intersection is measured in: the scroller it is
   * actually in, or `null` for the viewport.
   *
   * The rail and the main pane scroll independently, so the nearest `.scroll`
   * is the right root for an ordinary list, and a header or hero tile — with
   * no `.scroll` above it — wants the viewport anyway. A sheet in the top
   * layer is the other way round: the scroller it is written inside does not
   * clip it at all (see [`TOP_LAYER`]), so the viewport is the only honest
   * root there. A scroller inside the same top layer still counts: it clips
   * its own contents normally.
   */
  function observerRoot(node) {
    const scroller = node.closest(".scroll");
    if (!scroller || !TOP_LAYER) return scroller;
    const lifted = node.closest(TOP_LAYER);
    return lifted && !lifted.contains(scroller) ? null : scroller;
  }
</script>

<script>
  import { resolveCoverUrl } from "../lib/state.svelte.js";
  import { identityTone } from "../lib/covertone.svelte.js";

  /**
   * Artwork in four tiers, falling back in order:
   *
   *   1. the entity's own cover
   *   2. a 2x2 mosaic of four distinct album covers
   *   3. a single cover, full-bleed, when fewer than four are distinct
   *      (a 1x2 or L-shaped 3-up reads as broken, so we never draw one)
   *   4. a generated identity tile keyed off the entity id
   *
   * Tier 4 is deterministic and needs no network, so the common case for a
   * playlist — Spotify's rootlist carries no cover at all — paints instantly
   * and still looks designed rather than absent. Tiers 1–3 wait on the
   * network; while they wait the tile holds a quiet neutral ground, never a
   * borrowed letter — a monogram that vanishes a beat later reads as a
   * mistake, not a placeholder.
   */
  let {
    src = "",
    srcs = [],
    id = "",
    name = "",
    size = 48,
    /**
     * Stretch to the container instead of taking a pixel size. The tile still
     * takes explicit dimensions — 100% rather than none — so it can never fall
     * back to shrink-wrapping the image's natural 300px in a container that
     * does not happen to constrain it. The container supplies `--tile` for the
     * monogram scale.
     */
    fill = false,
    /**
     * Show the source at its OWN proportions: width leads, height follows.
     *
     * Every other mode puts the image in a box of a known shape and crops to
     * it, which is right for a tile in a row of tiles. It is wrong for a lone
     * editorial photograph, where the frame the photographer chose is part of
     * the picture. `fill` cannot do this: it writes `height: 100%` inline, so
     * the tile keeps a height the image no longer fills and `.art`'s own
     * `background` paints the difference as a grey slab under the picture.
     *
     * Only meaningful for a real image; the generated monogram tile stays
     * square, because a letter has no proportions to respect.
     */
    natural = false,
    lg = false,
    circle = false,
    raised = false,
    class: cls = "",
  } = $props();

  /** Distinct candidates for the mosaic, capped at the four cells. */
  const pool = $derived([...new Set(srcs.filter(Boolean))].slice(0, 4));

  /** Tier selection. `src` always wins: it is the entity's own artwork. */
  const tier = $derived(src ? "single" : pool.length >= 4 ? "mosaic" : pool.length ? "single" : "gen");
  const primary = $derived(src || pool[0] || "");

  const letter = $derived((name.trim()[0] ?? "?").toUpperCase());
  /* One hash, one ring of hues, shared with the header wash — so a coverless
     playlist's tile and its page open on the same colour. See covertone. */
  const seedTone = $derived(identityTone(id || name));
  /** Resolved `cover://` urls, indexed the same as the source list. */
  let resolved = $state({});
  /** Sources whose resolution failed. */
  let failed = $state({});
  /** Sources decoded and on screen. Resolution only says a file is
      fetchable; until its pixels exist the tile holds the neutral
      `.art.pending` ground rather than anything resembling content. */
  let shown = $state({});
  /**
   * The url the single tier's <img> is holding, which only ever moves FORWARD.
   *
   * Taking a src away does not blank an <img>, it BREAKS it: the browser drops
   * the picture and draws its own glyph, 16px of it, in the middle of whatever
   * box the element has. That is the "no image" mark that flashed in the
   * artist gallery — one element, reused across the set, and every step handed
   * it `resolved[nextUrl]` while that was still undefined.
   *
   * Handing it the outgoing url instead costs nothing and closes the window
   * completely, because holding one picture until the next can replace it is
   * the platform's own behaviour: an <img> whose src changes keeps presenting
   * the image it already has until the new one has decoded, then swaps in a
   * single frame. Which is also the nicest thing a stepper could do.
   */
  let painted = $state("");
  $effect(() => {
    const local = resolved[primary];
    if (local) painted = local;
  });

  /**
   * The tile's own box: what the viewport question is asked about, and the
   * element that gets the <img>.
   */
  let art = $state(null);
  /**
   * Whether the tile has ever been on screen. One-way on purpose: a tile that
   * has been seen must not lose its cover when it scrolls back out of view.
   */
  let seen = $state(false);

  /**
   * Resolving is an IPC round trip per url — up to four for a mosaic — and it
   * happens at MOUNT, not at paint: `loading="lazy"` defers the browser's own
   * fetch for a row below the fold, but it has never deferred this. A rail of
   * 48 playlists measured 132 resolutions for the twelve rows on screen.
   *
   * So the tile's own container is what gets observed: a list resolves the
   * rows the viewer can see and pays for the rest as they arrive, while a
   * header or hero tile is on screen the moment it mounts and pays nothing
   * extra — one frame's wait, the same frame the observer's first callback is
   * delivered in.
   */
  $effect(() => {
    if (seen) return;
    const node = art;
    if (!node) return;
    /* No IntersectionObserver to ask: resolve at once, which is exactly what
       every tile did before this gate existed. */
    if (typeof IntersectionObserver !== "function") {
      seen = true;
      return;
    }
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) return;
        seen = true;
        observer.disconnect();
      },
      /* The scroller this tile is really in, or the viewport — including for a
         sheet in the top layer, where the scroller it is written inside clips
         nothing at all. See [`observerRoot`]. */
      { root: observerRoot(node) },
    );
    observer.observe(node);
    return () => observer.disconnect();
  });

  $effect(() => {
    if (!seen) return;
    const wanted = tier === "mosaic" ? pool : primary ? [primary] : [];
    for (const url of wanted) {
      if (url in resolved || url in failed) continue;
      /* A url the engine has already answered with nothing twice is left alone
         here too, and without a round trip: the tile takes the generated
         ground it would have taken had the answer come back empty again. */
      if (coverMisses.dead(url)) {
        failed[url] = true;
        continue;
      }
      resolveCoverUrl(url).then(
        (local) => {
          if (local) {
            resolved[url] = local;
            coverMisses.resolved(url);
          } else {
            failed[url] = true;
            coverMisses.miss(url);
          }
        },
        /* The resolve itself failed — the bridge was not there, the call never
           reached the engine — and that says nothing about the url: this tile
           falls back now and the next mount asks again. Remembering it here is
           what turned one flicker of the network into a cover that never came
           back. */
        () => {
          failed[url] = true;
        },
      );
    }
  });

  /** Every image of the selected tier has painted. */
  const revealed = $derived(
    tier === "mosaic" ? pool.every((u) => shown[u]) : !!shown[primary],
  );
  /** The tier can no longer complete — its own cover or a mosaic cell
      failed — so the generated identity tile takes over, which is where a
      failed cover has always landed. */
  const lost = $derived(
    tier === "mosaic" ? pool.some((u) => failed[u]) : !!failed[primary],
  );

  /**
   * Reveal an <img> once its pixels exist. A cache hit can finish decoding
   * before an onload handler could ever be bound, so completeness is
   * checked here, synchronously, where the listener cannot lose that race.
   *
   * On an UPDATE that check answers for the picture the element is still
   * holding rather than for the one just asked for, and that is deliberate:
   * `shown` gates the reveal fade, and a tile that already has pixels on
   * screen must not fade them out and back in because the entity behind them
   * changed. The swap itself is atomic (see `painted`), so there is no moment
   * to cover.
   */
  function bindArt(node, url) {
    let gate = null;
    const arm = (u) => {
      gate?.abort();
      gate = new AbortController();
      if (node.complete && node.naturalWidth > 0) {
        shown[u] = true;
        return;
      }
      const settle = () => (shown[u] = true);
      node.addEventListener("load", settle, { once: true, signal: gate.signal });
      node.addEventListener("error", settle, { once: true, signal: gate.signal });
    };
    arm(url);
    return { update: arm };
  }
</script>

{#if tier !== "gen" && !lost}
  <span
    bind:this={art}
    class="art {cls}"
    class:lg
    class:circle
    class:raised
    class:mosaic={tier === "mosaic"}
    class:natural
    class:pending={!revealed}
    class:ready={revealed}
    style:width={fill || natural ? "100%" : `${size}px`}
    style:height={natural ? null : fill ? "100%" : `${size}px`}
  >
    <!-- width/height attributes as well as the CSS above: a load that fails
         still reserves the identical box, so nothing reflows around it. -->
    <!-- loading="lazy": the sidebar and queue render covers far below the
         fold; offscreen ones must not be fetched or decoded until scrolled
         to. decoding="async": the visible slice paints without waiting for
         the rest of the decode queue. Both are safe because every image has
         explicit dimensions — nothing can reflow around a late decode. -->
    {#if tier === "mosaic"}
      {#each pool as url (url)}
        {#if resolved[url]}
          <img use:bindArt={url} class:fresh={!shown[url]} src={resolved[url]} alt="" width={Math.round(size / 2)} height={Math.round(size / 2)} draggable="false" loading="lazy" decoding="async" />
        {/if}
      {/each}
    {:else if painted}
      <!-- No picture, no picture element, the same rule the mosaic keeps above.
           A srcless <img> is not an empty box either: it lays out the 16px
           broken-image placeholder, which the reveal fade happened to hide at
           opacity 0 — and `prefers-reduced-motion` turns that fade off. -->
      <img use:bindArt={primary} class:fresh={!shown[primary]} src={painted} alt={name} width={size} height={size} draggable="false" loading="lazy" decoding="async" />
    {/if}
  </span>
{:else}
  <span
    class="art gen {cls}"
    class:lg
    class:circle
    class:raised
    style:width={fill ? "100%" : `${size}px`}
    style:height={fill ? "100%" : `${size}px`}
    style:--tone-a={seedTone.tileA}
    style:--tone-b={seedTone.tileB}
    style:--tile={fill ? null : `${size}px`}
    data-letter={letter}
    role="img"
    aria-label={name}
  ></span>
{/if}
