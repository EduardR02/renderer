<script>
  /**
   * References a symbol from the sprite that `IconSprite.svelte` renders once
   * at the app root, so an icon costs two DOM nodes instead of a full path
   * set. That matters most in the track list, where every row carries three.
   */
  let { name, size = 24 } = $props();

  // Call sites keep the descriptive names; the sprite uses short ids.
  const ALIASES = {
    previous: "prev",
    forward: "fwd",
    "heart-filled": "heart-f",
    volume: "vol",
  };

  const id = $derived(`#i-${ALIASES[name] ?? name}`);
</script>

<!--
  Sized three ways on purpose. The width/height ATTRIBUTES reserve the box
  before any stylesheet is applied; the inline CSS pins it against anything a
  sheet might say later; `flex: none` stops a flex parent shrinking it. A
  missing symbol id, an unparsed sprite, or a sprite that never mounted then
  costs an empty square — never a row that changes size.
-->
<svg
  class="icon"
  width={size}
  height={size}
  style:width="{size}px"
  style:height="{size}px"
  aria-hidden="true"
  focusable="false"
>
  <use href={id} />
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>
