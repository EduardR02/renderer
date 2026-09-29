<script>
  import { navigate } from "../lib/state.svelte.js";
  import { isPinned, togglePin } from "../lib/pins.svelte.js";
  import { trackDrag } from "../lib/dnd.svelte.js";
  import Cover from "./Cover.svelte";
  let { playlist, active = false, playing = false, depth = 0 } = $props();
</script>
<div class="lib-row playlist-pin-row" class:active class:playing
  class:no-drop={trackDrag.active && trackDrag.sourcePlaylistId === playlist.id} data-pid={playlist.id}
  style:padding-left={`${8 + depth * 16}px`}>
  <button class="pin-open" onclick={() => navigate("playlist", playlist.id)}>
    <Cover src={playlist.cover_url} srcs={playlist.cover_urls ?? []} id={playlist.id} name={playlist.name} size={32} />
    <span class="lib-name">{playlist.name}</span>
  </button>
  {#if trackDrag.active}<span class="lib-drop-hint" aria-hidden="true">+</span>
  {:else}<span class="pin-controls">
    {#if playlist.tracks_total}<span class="lib-count">{playlist.tracks_total}</span>{/if}
    <button class="pin-toggle" aria-label={isPinned(playlist.id) ? `Unpin ${playlist.name}` : `Pin ${playlist.name}`}
      title={isPinned(playlist.id) ? "Unpin from sidebar" : "Pin to top of sidebar"}
      onclick={() => togglePin(playlist.id)}>{isPinned(playlist.id) ? "◆" : "◇"}</button>
  </span>{/if}
</div>
<style>
  .playlist-pin-row { grid-template-columns:minmax(0,1fr) auto; }
  .pin-open { display:grid; grid-template-columns:32px minmax(0,1fr); align-items:center; gap:var(--s3); min-width:0; width:100%; background:none; border:0; color:inherit; cursor:pointer; text-align:left; padding:0; }
  .pin-controls { display:flex; align-items:center; gap:8px; }
  .pin-toggle { background:none; border:0; color:var(--fg-3); cursor:pointer; padding:4px; opacity:.55; }
  .playlist-pin-row:hover .pin-toggle,.pin-toggle:focus-visible { opacity:1; }
</style>
