<script>
  import { writeClipboard } from "../lib/spotify-link.js";

  /**
   * "Copy link", the one item every shareable thing's menu carries.
   *
   * The confirmation lives on the item, which is why the menu does not close
   * on click: a copy with no feedback is indistinguishable from a dead
   * control. A copy that landed closes the menu shortly after, through the
   * menu's own close, so the trigger gets its focus back; a refused write
   * stays up and says so — the item is also the retry. The item dies with its
   * menu, and so do its state and its timer.
   */
  let { link, close } = $props();

  let copied = $state("idle");
  let timer = 0;
  $effect(() => () => clearTimeout(timer));

  async function copy() {
    if (!link) return;
    const ok = await writeClipboard(link);
    copied = ok ? "copied" : "failed";
    clearTimeout(timer);
    if (ok) timer = setTimeout(() => close?.(true), 900);
  }
</script>

<button
  class="menu-item"
  role="menuitem"
  class:done={copied === "copied"}
  class:failed={copied === "failed"}
  disabled={!link}
  onclick={copy}
>
  {copied === "copied" ? "Link copied" : copied === "failed" ? "Copy failed" : "Copy link"}
</button>
