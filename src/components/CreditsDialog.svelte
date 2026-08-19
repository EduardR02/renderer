<script>
  import { credits, closeCredits } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";

  let dialog = $state(null);
  let closeButton = $state(null);
  const titleId = "credits-dialog-title";

  $effect(() => {
    if (!dialog || !credits.open || dialog.open) return;
    dialog.showModal();
    queueMicrotask(() => closeButton?.focus());
  });

  function close() {
    closeCredits();
  }

  function onNativeCancel(event) {
    event.preventDefault();
    close();
  }

  function onBackdropClick(event) {
    if (event.target === dialog) close();
  }

  /**
   * Contributors are plain text: there is currently nowhere correct to send
   * them.
   *
   * The wanted destination is the songwriter portal the official client links
   * to, `artists.spotify.com/songwriter/<id>`. That is a *different id space*
   * from the artist ids the credits endpoint returns, measured on Max Martin:
   *
   * | id                       | source            | songwriter portal |
   * |--------------------------|-------------------|-------------------|
   * | `1rjeVTt9Ra1ldvN7SpeK0G` | the credits reply | HTTP 500          |
   * | `1T7Hkfs6QmizPlOCzs08LS` | the official app  | his real profile  |
   *
   * So building the songwriter URL from the credit's artist id — what this did
   * originally — produced a broken link for every contributor. The official
   * client must resolve the mapping with a call we do not make; the credits
   * payload contains no songwriter id in any field.
   *
   * Linking to `open.spotify.com/artist/<id>` instead would resolve, but it is
   * not worth doing: every artist here is already one click away from the track
   * row, so it adds nothing and disguises the missing feature. `id` stays on
   * the payload for whenever the real endpoint is found.
   */

  function formatRole(role) {
    if (typeof role !== "string") return "";
    return role.trim().replace(/\b[a-z]/g, (letter) => letter.toUpperCase());
  }

  const groups = $derived(credits.data?.groups ?? []);
  /** The licensor the credits came from, shown under the groups as Spotify does. */
  const source = $derived(credits.data?.source ?? "");
  const trackName = $derived(credits.data?.track_name || credits.track?.name || "Track credits");
</script>


<dialog
  class="credits-dialog"
  bind:this={dialog}
  aria-labelledby={titleId}
  aria-busy={credits.loading}
  data-credits-surface
  oncancel={onNativeCancel}
  onclick={onBackdropClick}
>
  <div class="credits-sheet">
    <header class="credits-head">
      <div>
        <span class="eyebrow">Track credits</span>
        <h2 id={titleId}>{trackName}</h2>
      </div>
      <button class="btn-icon" bind:this={closeButton} aria-label="Close credits" title="Close" onclick={close}>
        <Icon name="x" size={16} />
      </button>
    </header>

    <div class="credits-body" aria-live="polite">
      {#if credits.loading}
        <div class="credits-loading" aria-label="Loading credits">
          <span class="skeleton line lg"></span>
          <span class="skeleton line sm"></span>
          <span class="skeleton line lg"></span>
        </div>
      {:else if credits.error}
        <div class="empty compact">
          <p class="h" role="alert">Credits unavailable</p>
          <p class="sub">{credits.error}</p>
        </div>
      {:else if groups.length}
        {#each groups as group, groupIndex (`${group.title}-${groupIndex}`)}
          <section class="credits-group">
            <h3>{group.title}</h3>
            <ul>
              {#each group.contributors ?? [] as contributor, index (`${contributor.id || contributor.uri || contributor.name}-${index}`)}
                <li>
                  <div class="credit-name">
                    <span class="credit-plain">{contributor.name}</span>
                  </div>
                  {#if contributor.subroles?.length}
                    <div class="credit-details" aria-label="Credit roles">
                      {#each contributor.subroles as role, roleIndex (`${role}-${roleIndex}`)}
                        {#if formatRole(role)}
                          <span class="credit-detail" title={role}>{formatRole(role)}</span>
                        {/if}
                      {/each}
                    </div>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/each}
      {:else}
        <div class="empty compact">
          <p class="h">No credits listed</p>
          <p class="sub">Spotify did not return contributor details for this track.</p>
        </div>
      {/if}
    </div>

    <footer class="credits-foot">
      {#if source}<span class="credit-source">Source: {source}</span>{/if}
      <button class="btn-ghost" onclick={close}>Close</button>
    </footer>
  </div>
</dialog>
