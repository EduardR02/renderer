<script>
  import { openUrl } from "@tauri-apps/plugin-opener";
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
   * Opens a contributor's page and leaves the dialog up.
   *
   * The URL is whatever the service supplied on the contributor — for writers,
   * their `artists.spotify.com/songwriter/<id>` page. It is never constructed
   * here, and cannot be: that id space is not the artist one. Max Martin
   * arrives in the credits payload as artist `1rjeVTt9Ra1ldvN7SpeK0G`, which
   * 500s on the songwriter portal, while his real songwriter page is
   * `1T7Hkfs6QmizPlOCzs08LS`. Nothing maps between them — the official client
   * opens a server-supplied `url` too, for exactly this reason.
   *
   * Closing on click was wrong: a credits list is read across, several names at
   * a time, and dismissing it after the first throws away the reading position.
   * The browser takes focus anyway.
   */
  async function openContributor(url) {
    try {
      await openUrl(url);
    } catch (error) {
      console.error("Could not open the contributor page", error);
    }
  }

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
                    {#if contributor.url}
                      <button
                        class="credit-link"
                        aria-label={`Open ${contributor.name} in your browser`}
                        onclick={() => openContributor(contributor.url)}
                      >{contributor.name}</button>
                    {:else}
                      <span class="credit-plain">{contributor.name}</span>
                    {/if}
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
