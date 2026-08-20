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
   * and still looks designed rather than absent.
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

  $effect(() => {
    const wanted = tier === "mosaic" ? pool : primary ? [primary] : [];
    for (const url of wanted) {
      if (url in resolved) continue;
      resolveCoverUrl(url).then((local) => {
        if (local) resolved[url] = local;
      });
    }
  });

  const ready = $derived(tier === "mosaic" ? pool.every((u) => resolved[u]) : !!resolved[primary]);
</script>

{#if tier !== "gen" && ready}
  <span
    class="art {cls}"
    class:lg
    class:circle
    class:raised
    class:mosaic={tier === "mosaic"}
    style:width={fill ? "100%" : `${size}px`}
    style:height={fill ? "100%" : `${size}px`}
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
        <img src={resolved[url]} alt="" width={Math.round(size / 2)} height={Math.round(size / 2)} draggable="false" loading="lazy" decoding="async" />
      {/each}
    {:else}
      <img src={resolved[primary]} alt={name} width={size} height={size} draggable="false" loading="lazy" decoding="async" />
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
