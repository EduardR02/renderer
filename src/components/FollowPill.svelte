<script>
  import { untrack } from "svelte";
  import { followed, loadFollowedArtists, ui } from "../lib/state.svelte.js";
  import {
    personalConnected, watchPersonal, setFollowingArtist, followsUser, setFollowingUser,
  } from "../lib/personal.svelte.js";
  import Icon from "./Icon.svelte";

  /**
   * Follow, for an artist or a user: the one pill a header row may carry,
   * because it holds a state rather than performing an act.
   *
   * An artist's state is the engine's followed list — the one the rail's
   * Artists tab shows — read once a session and kept current by our own
   * writes, so opening an artist costs no check. A user's is the one follow
   * nothing local knows; it is asked once, when the profile opens.
   *
   * Writing needs the personal app. Without one there is no control at all,
   * no pill that sends you to Settings: only the state, when the app already
   * knows it (an artist in a list the rail has loaded), as a quiet mark.
   */
  let { artist = null, username = "" } = $props();

  $effect(() => watchPersonal());
  const connected = $derived(personalConnected());

  /* The pill is what needs the list; without the personal app nothing here
     asks for it. */
  $effect(() => {
    if (artist?.id && connected && !followed.loaded) untrack(() => loadFollowedArtists());
  });

  let user = $state(undefined);
  $effect(() => {
    const name = username;
    const ready = connected;
    user = undefined;
    if (!name || !ready) return;
    let active = true;
    followsUser(name)
      .then((value) => { if (active) user = value; })
      .catch(() => { if (active) user = null; });
    return () => { active = false; };
  });

  /** true / false once known; undefined while it is not; null when it cannot be. */
  const following = $derived(
    artist
      ? followed.loaded ? followed.artists.some((entry) => entry.id === artist.id)
        : followed.error ? null : undefined
      : user,
  );

  let busy = $state(false);
  async function toggle() {
    if (busy || typeof following !== "boolean") return;
    const next = !following;
    busy = true;
    try {
      if (artist) await setFollowingArtist(artist, next);
      else {
        await setFollowingUser(username, next);
        user = next;
      }
    } catch (reason) {
      ui.error = `Could not ${next ? "follow" : "unfollow"}. ${reason instanceof Error ? reason.message : String(reason ?? "")}`.trim();
    } finally {
      busy = false;
    }
  }
</script>

{#if connected && following !== null}
  <button
    class="pill follow"
    class:on={following === true}
    disabled={busy || following === undefined}
    aria-pressed={following === true}
    aria-busy={busy || following === undefined}
    title={following ? "Unfollow" : "Follow"}
    onclick={toggle}
  >
    {#if following}<Icon name="check" size={14} />Following{:else}Follow{/if}
  </button>
{:else if !connected && artist && following === true}
  <span class="pill follow on static" title="You follow this artist on Spotify">
    <Icon name="check" size={14} />Following
  </span>
{/if}

<style>
  /* Wide enough for either word, so the row never moves when the state
     lands or flips. */
  .follow { min-width: 112px; }
  /* Pending is a state, not a disabled control: it keeps most of its strength. */
  .follow[aria-busy="true"]:disabled { opacity: 0.7; }
  /* A fact, not a control: no lift under the pointer. */
  .follow.static { cursor: default; }
  .follow.static:hover { color: var(--fg-1); background: var(--raise-1); box-shadow: inset 0 0 0 1px var(--line-2); }
</style>
