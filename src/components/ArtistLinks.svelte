<script>
  import { navigate } from "../lib/state.svelte.js";

  /**
   * A track's artists, each navigable on its own.
   *
   * Takes `ids` parallel to `names` when the payload has them, and falls back
   * to `id` — the primary artist — when it does not. In the fallback only the
   * first name links; the rest render as plain text rather than as links that
   * would all lead to the wrong artist.
   */
  let { names = [], ids = [], id = "", class: cls = "" } = $props();

  /** Index-for-index with `names`; empty string where there is no id to link. */
  const linkIds = $derived(
    names.map((_, i) => (ids.length ? (ids[i] ?? "") : i === 0 ? id : ""))
  );

  function activate(event, linkId) {
    if (event.type === "keydown" && event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    event.stopPropagation();
    navigate("artist", linkId);
  }
</script>

<!--
  These are SPANS, not buttons, and that is the whole reason this markup looks
  the way it does.

  Every container these sit in truncates with `text-overflow: ellipsis` — the
  artist sub-line in a track row, the credit line in a player bar, the `.who`
  cell in a detail header. An ellipsis is only ever drawn where the overflow
  point falls in inline TEXT: a `<button>` is an atomic inline-level box
  whatever its `display`, so the browser sliced it straight through a glyph and
  drew nothing to say the line had been cut. With four credited artists in a
  narrow column that is what you saw, on every row.

  A span with `role="link"` and a key handler is a real control to the
  accessibility tree and ordinary text to the line breaker, which is exactly
  the combination this needs.
-->
<span class={cls}>
  {#each names as name, i}
    {#if i > 0}<span class="sep">, </span>{/if}
    {#if linkIds[i]}
      <span
        class="artist-link"
        role="link"
        tabindex="0"
        title="Go to {name}"
        onclick={(event) => activate(event, linkIds[i])}
        onkeydown={(event) => activate(event, linkIds[i])}
      >{name}</span>
    {:else}
      <span>{name}</span>
    {/if}
  {/each}
</span>

<style>
  .artist-link {
    cursor: pointer;
    transition: color var(--d1) var(--ease);
  }
  .artist-link:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  .sep {
    white-space: pre;
  }
</style>
