<script>
  import { resolveCoverUrl } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";

  let { src, alt = "", rounded = 4, circle = false, iconSize = 28, style = "", class: cls = "" } =
    $props();

  let display = $state(null);
  let failed = $state(false);

  $effect(() => {
    const s = src;
    display = null;
    failed = false;
    if (!s) return;
    resolveCoverUrl(s).then((u) => {
      if (src !== s) return;
      if (u) display = u;
      else failed = true;
    });
  });

  const radius = $derived(circle ? "50%" : `${rounded}px`);
</script>

<div class="cover {cls}" {style} style:border-radius={radius}>
  {#if display && !failed}
    <img src={display} alt={alt} loading="lazy" draggable="false" onerror={() => (failed = true)} />
  {:else}
    <div class="cover-fallback">
      <Icon name="note" size={iconSize} />
    </div>
  {/if}
</div>

<style>
  .cover {
    position: relative;
    overflow: hidden;
    flex: none;
    background: var(--bg-cover-fallback);
  }
  .cover img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .cover-fallback {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #7a7a7a;
  }
</style>
