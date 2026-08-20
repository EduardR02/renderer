<script>
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { credits, closeCredits } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Icon from "./Icon.svelte";
  import Cover from "./Cover.svelte";

  let dialog = $state(null);
  let query = $state("");
  const titleId = "credits-dialog-title";

  /** A short list is its own index; a long one needs a way in. */
  const FILTERABLE_FROM = 12;
  /**
   * How long an opening waits for its payload before committing to a width.
   *
   * The sheet is 920px wide for a hundred contributors and 600px for a
   * two-writer single, and that choice has to be made *before* first paint:
   * it used to be a reactive class, so a cold-fetched short payload opened
   * wide and then slid narrower a beat later — the dialog correcting itself
   * in front of the reader. Below the ~150ms threshold where a delay is
   * perceptible as one, so the common cases (a cached payload, or a fast
   * reply) open already at the right size, and only a genuinely slow fetch
   * pays for the guess. Whatever is chosen here is frozen for the opening.
   */
  const WIDTH_GRACE_MS = 140;

  let compact = $state(false);

  function contributorCount(data) {
    return (data?.groups ?? []).reduce((sum, group) => sum + (group.contributors?.length ?? 0), 0);
  }

  $effect(() => {
    if (!dialog || !credits.open || dialog.open) return;
    let timer = 0;
    const show = () => {
      if (!dialog || dialog.open) return;
      query = "";
      compact = !credits.loading && contributorCount(credits.data) < FILTERABLE_FROM;
      dialog.showModal();
      /* Focus the SHEET, not the close button. Focusing Close drew a cyan
         focus ring on the one control that dismisses the thing you just
         opened, which is both a strange first impression and a strange first
         suggestion. The dialog carries `tabindex="-1"` so it can hold focus
         itself; Escape, the focus trap and tab order all still work, and the
         first Tab lands on the filter or the first name. */
      queueMicrotask(() => dialog?.focus());
    };
    // Re-entering when `loading` clears is the point: the payload landing
    // inside the grace window shows the sheet immediately, at the right width.
    if (credits.loading) timer = setTimeout(show, WIDTH_GRACE_MS);
    else show();
    return () => clearTimeout(timer);
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

  /** Subroles read as one quiet line under the name, not as a row of pills:
      at a hundred contributors, a hundred pills is the noise, not the data. */
  function roleLine(contributor) {
    return (contributor.subroles ?? []).map(formatRole).filter(Boolean).join(" · ");
  }

  const groups = $derived(credits.data?.groups ?? []);
  /** The licensor the credits came from, shown under the groups as Spotify does. */
  const source = $derived(credits.data?.source ?? "");
  const trackName = $derived(credits.data?.track_name || credits.track?.name || "Track credits");
  const artistLine = $derived((credits.track?.artist_names ?? []).join(", "));
  const total = $derived(
    groups.reduce((sum, group) => sum + (group.contributors?.length ?? 0), 0),
  );
  const filterable = $derived(total >= FILTERABLE_FROM);
  /* The sheet's header takes the record's own colour, the same way its page
     and the now-playing rail do — so a dialog opened over a red album is not
     the one grey rectangle in an otherwise coloured app. */
  const tone = $derived(
    coverTone(credits.track?.cover_url ?? "", credits.track?.album_id || credits.track?.uri || ""),
  );

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return groups;
    return groups
      .map((group) => {
        const groupHit = (group.title ?? "").toLowerCase().includes(q);
        const contributors = (group.contributors ?? []).filter(
          (c) =>
            groupHit ||
            (c.name ?? "").toLowerCase().includes(q) ||
            (c.subroles ?? []).some((r) => String(r).toLowerCase().includes(q)),
        );
        return { ...group, contributors };
      })
      .filter((group) => group.contributors.length);
  });
  const matches = $derived(
    filtered.reduce((sum, group) => sum + (group.contributors?.length ?? 0), 0),
  );
</script>


<dialog
  class="credits-dialog"
  class:compact
  bind:this={dialog}
  tabindex="-1"
  aria-labelledby={titleId}
  aria-busy={credits.loading}
  data-credits-surface
  oncancel={onNativeCancel}
  onclick={onBackdropClick}
  style:--tone-wash={tone.wash}
  style:--tone-glow={tone.glow}
>
  <div class="credits-sheet">
    <header class="credits-head">
      <div class="credits-identity">
        <Cover
          src={credits.track?.cover_url}
          id={credits.track?.album_id || credits.track?.uri || trackName}
          name={credits.track?.album_name || trackName}
          size={64}
          lg
        />
        <div class="credits-title-copy">
          <span class="tag credit">Credits</span>
          <h2 id={titleId}>{trackName}</h2>
          {#if artistLine}<p>{artistLine}</p>{/if}
        </div>
      </div>
      <button class="btn-icon" aria-label="Close credits" title="Close" onclick={close}>
        <Icon name="x" size={16} />
      </button>
    </header>

    {#if !credits.loading && !credits.error && total}
      <div class="credits-tools">
        <p class="credits-summary">
          <span class="tnum">{total}</span>
          {total === 1 ? "contributor" : "contributors"}
          <span class="credits-sep" aria-hidden="true"></span>
          <span class="tnum">{groups.length}</span>
          {groups.length === 1 ? "role" : "roles"}
        </p>
        {#if filterable}
          <label class="credits-filter">
            <Icon name="search" size={13} />
            <input
              type="text"
              placeholder="Find a name or role"
              bind:value={query}
              aria-label="Filter credits"
            />
            {#if query}
              <button class="credits-filter-clear" aria-label="Clear filter" onclick={() => (query = "")}>
                <Icon name="x" size={12} />
              </button>
            {/if}
          </label>
        {/if}
      </div>
    {/if}

    <div class="credits-body" aria-live="polite">
      {#if credits.loading}
        <div class="credits-loading" aria-label="Loading credits">
          <span class="skeleton line sm"></span>
          <span class="skeleton line lg"></span>
          <span class="skeleton line lg"></span>
        </div>
      {:else if credits.error}
        <div class="empty compact">
          <p class="h" role="alert">Credits unavailable</p>
          <p class="sub">{credits.error}</p>
        </div>
      {:else if filtered.length}
        {#each filtered as group, groupIndex (`${group.title}-${groupIndex}`)}
          <section class="credits-group">
            <!-- Sticky, so the role a name belongs to is still on screen a
                 hundred rows into a long payload. -->
            <h3 class="credits-group-head">
              <span class="credits-group-title">{group.title}</span>
              <span class="credits-group-count tnum">{group.contributors.length}</span>
            </h3>
            <ul class="credits-people">
              {#each group.contributors as contributor, index (`${contributor.id || contributor.uri || contributor.name}-${index}`)}
                {@const roles = roleLine(contributor)}
                <li>
                  {#if contributor.url}
                    <button
                      class="credit-link"
                      aria-label={`Open ${contributor.name} in your browser`}
                      onclick={() => openContributor(contributor.url)}
                    >
                      <span class="credit-name">{contributor.name}</span>
                      <Icon name="fwd" size={11} />
                    </button>
                  {:else}
                    <span class="credit-name credit-plain">{contributor.name}</span>
                  {/if}
                  {#if roles}<span class="credit-roles">{roles}</span>{/if}
                </li>
              {/each}
            </ul>
          </section>
        {/each}
      {:else if query}
        <div class="empty compact">
          <p class="h">No match for “{query}”</p>
          <p class="sub">Nothing in these {total} contributors matches that name or role.</p>
        </div>
      {:else}
        <div class="empty compact">
          <p class="h">No credits listed</p>
          <p class="sub">Spotify did not return contributor details for this track.</p>
        </div>
      {/if}
    </div>

    <footer class="credits-foot">
      {#if query && filtered.length}
        <span class="credit-source"><span class="tnum">{matches}</span> of <span class="tnum">{total}</span> shown</span>
      {:else if source}
        <span class="credit-source">{source}</span>
      {/if}
      <button class="btn-ghost" onclick={close}>Close</button>
    </footer>
  </div>
</dialog>
