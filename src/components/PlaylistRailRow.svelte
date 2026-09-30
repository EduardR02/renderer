<script>
  import { navigate } from "../lib/state.svelte.js";
  import { trackDrag } from "../lib/dnd.svelte.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  let { playlist, active = false, playing = false, pinned = false, depth = 0, onmenu, observeCover } = $props();
</script>

<button class="lib-row" class:active class:playing
  class:no-drop={trackDrag.active && trackDrag.sourcePlaylistId === playlist.id} data-pid={playlist.id}
  use:observeCover={playlist.id}
  style:padding-left={`${8 + depth * 16}px`}
  aria-haspopup="menu"
  onclick={() => navigate("playlist", playlist.id)}
  oncontextmenu={(event) => onmenu?.(event, playlist)}
  onkeydown={(event) => onmenu?.(event, playlist)}>
  <Cover src={playlist.cover_url} srcs={playlist.cover_urls ?? []} id={playlist.id} name={playlist.name} size={32} />
  <span class="lib-name">{playlist.name}</span>
  {#if trackDrag.active}
    <span class="lib-drop-hint" aria-hidden="true"><Icon name="plus" size={13} /></span>
  {:else}
    <span class="lib-tail">
      <!-- A small mark, not a heading: the row is where it is because it was
           pinned, and this says so without a section around it. -->
      {#if pinned}<span class="lib-pin" title="Pinned to top"><Icon name="pin" size={11} /></span>{/if}
      {#if playlist.tracks_total}<span class="lib-count">{playlist.tracks_total}</span>{/if}
    </span>
  {/if}
</button>

<style>
  .lib-tail { display: flex; align-items: center; gap: var(--s2); }
  .lib-pin { display: grid; place-items: center; color: var(--fg-3); }
</style>
