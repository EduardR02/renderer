<script>
  import { navigate } from "../lib/state.svelte.js";
  import { trackDrag } from "../lib/dnd.svelte.js";
  import Cover from "./Cover.svelte";
  import Icon from "./Icon.svelte";
  let { playlist, active = false, playing = false, depth = 0, onmenu, observeCover } = $props();
</script>

<button class="lib-row" class:active class:playing
  class:no-drop={trackDrag.active && trackDrag.sourcePlaylistId === playlist.id} data-pid={playlist.id}
  use:observeCover={playlist.id}
  style:padding-left={`${8 + depth * 16}px`}
  aria-haspopup="menu"
  onclick={() => navigate("playlist", playlist.id)}
  oncontextmenu={(event) => onmenu?.(event, playlist.id, playlist.name)}
  onkeydown={(event) => onmenu?.(event, playlist.id, playlist.name)}>
  <Cover src={playlist.cover_url} srcs={playlist.cover_urls ?? []} id={playlist.id} name={playlist.name} size={32} />
  <span class="lib-name">{playlist.name}</span>
  {#if trackDrag.active}
    <span class="lib-drop-hint" aria-hidden="true"><Icon name="plus" size={13} /></span>
  {:else if playlist.tracks_total}
    <span class="lib-count">{playlist.tracks_total}</span>
  {/if}
</button>
