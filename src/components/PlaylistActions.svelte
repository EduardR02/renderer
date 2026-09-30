<script module>
  /**
   * An action asked for from outside the playlist's own page — the rail's
   * right-click menu — that needs the page to carry it out: renaming happens
   * in place of the title, the cleanup and the delete confirmation are
   * sheets over it. The rail navigates there and leaves the request here;
   * PlaylistView takes it once that playlist has loaded.
   */
  export const playlistRequest = $state({ id: null, action: null });
</script>

<script>
  import { library, navigate, session } from "../lib/state.svelte.js";
  import { isPinned, togglePin } from "../lib/pins.svelte.js";
  import { spotifyLink } from "../lib/spotify-link.js";
  import CopyLinkItem from "./CopyLinkItem.svelte";

  /**
   * THE playlist menu: the items of the playlist header's "…" and of the
   * rail's right-click menu, which are one menu with one wording.
   *
   * `playlist` is a library summary or the loaded playlist (id, name,
   * owner_id). On the playlist's page the host passes `onRename`,
   * `onCleanup` and `onDelete`; anywhere else those go through the page.
   * Liked Songs is a collection rather than a playlist, so its menu is its
   * pin and nothing else.
   */
  let { playlist, trackCount = null, close, onRename = null, onCleanup = null, onDelete = null } = $props();

  const id = $derived(playlist?.id ?? "");
  const count = $derived(trackCount ?? playlist?.tracks_total ?? 0);
  const liked = $derived(id === "liked");
  const editable = $derived(!liked && !!playlist?.owner_id && playlist.owner_id === session.username);
  const pinnable = $derived(liked || library.some((entry) => entry.id === id));

  function pin() {
    togglePin(id);
    close?.(true);
  }

  function run(action, local) {
    close?.();
    if (local) {
      local();
      return;
    }
    playlistRequest.id = id;
    playlistRequest.action = action;
    navigate("playlist", id);
  }
</script>

{#if pinnable}
  <button class="menu-item" role="menuitem" onclick={pin}>{isPinned(id) ? "Unpin" : "Pin to top"}</button>
{/if}
{#if editable}
  <button class="menu-item" role="menuitem" onclick={() => run("rename", onRename)}>Rename…</button>
  <button class="menu-item" role="menuitem" disabled={!count} onclick={() => run("cleanup", onCleanup)}>Remove songs by rules…</button>
{/if}
{#if !liked}
  <CopyLinkItem link={spotifyLink("playlist", id)} {close} />
{/if}
{#if editable}
  <div class="menu-sep" role="separator"></div>
  <button class="menu-item danger" role="menuitem" onclick={() => run("delete", onDelete)}>Delete playlist…</button>
{/if}
