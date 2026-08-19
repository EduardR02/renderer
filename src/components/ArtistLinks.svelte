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
</script>

<span class={cls}>
  {#each names as name, i}
    {#if i > 0}<span class="sep">, </span>{/if}
    {#if linkIds[i]}
      <button class="artist-link" title="Go to {name}" onclick={() => navigate("artist", linkIds[i])}>
        {name}
      </button>
    {:else}
      <span>{name}</span>
    {/if}
  {/each}
</span>

<style>
  .artist-link {
    color: inherit;
    font: inherit;
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
