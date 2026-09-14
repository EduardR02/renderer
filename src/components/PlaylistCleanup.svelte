<script>
  import { api } from "../lib/state.svelte.js";
  import { formatTime, formatExactTime } from "../lib/time.js";
  import {
    cleanupChoices, filterCleanupChoices, cleanupDuration, cleanupPreview, cleanupVersion,
  } from "../lib/playlist-cleanup.js";

  let { playlist, onClose } = $props();
  let dialog = $state(null);
  let firstInput = $state(null);
  let source = $state.raw(null);
  let sourceVersion = $state("");
  let rules = $state([]);
  let grouping = $state("all");
  let field = $state("artist");
  let query = $state("");
  let textOperator = $state("contains");
  let durationOperator = $state("shorter");
  let activeChoice = $state(-1);
  let suggestionsOpen = $state(false);
  let reviewing = $state(false);
  let busy = $state(false);
  let reloading = $state(false);
  let error = $state("");
  let draftError = $state("");
  let removed = $state(null);
  let nextRule = 0;
  const PREVIEW_PAGE_SIZE = 100;
  let previewPage = $state(0);

  const currentVersion = $derived(cleanupVersion(playlist));
  const stale = $derived(!!source && sourceVersion !== currentVersion);
  const tracks = $derived(source?.tracks ?? []);
  const incomplete = $derived(Number(source?.tracks_total) > tracks.length);
  const choices = $derived(field === "artist" || field === "album" ? cleanupChoices(tracks, field) : []);
  const suggestions = $derived(filterCleanupChoices(choices, query));
  const preview = $derived(cleanupPreview(tracks, rules, grouping));
  const pageCount = $derived(Math.max(1, Math.ceil(preview.rows.length / PREVIEW_PAGE_SIZE)));
  const page = $derived(Math.min(previewPage, pageCount - 1));
  const pageRows = $derived(preview.rows.slice(page * PREVIEW_PAGE_SIZE, (page + 1) * PREVIEW_PAGE_SIZE));
  const frozen = $derived(reviewing || busy || removed !== null || stale);
  const ready = $derived(rules.length > 0 && preview.rows.length > 0 && !stale && !incomplete && !!source?.snapshot_id);

  $effect(() => {
    if (!dialog || dialog.open) return;
    useLatest();
    dialog.showModal();
    queueMicrotask(() => firstInput?.focus());
  });

  function useLatest() {
    source = $state.snapshot(playlist);
    sourceVersion = cleanupVersion(playlist);
    reviewing = false;
    error = "";
    previewPage = 0;
    suggestionsOpen = false;
    activeChoice = -1;
  }

  function close() {
    if (!busy) onClose?.();
  }

  function nativeCancel(event) {
    event.preventDefault();
    if (suggestionsOpen) suggestionsOpen = false;
    else if (reviewing && !busy) reviewing = false;
    else close();
  }

  function changeField() {
    query = "";
    draftError = "";
    activeChoice = -1;
    suggestionsOpen = false;
  }

  function addRule(choice = null) {
    if (frozen) return;
    draftError = "";
    let rule;
    if (field === "artist" || field === "album") {
      if (!choice) {
        draftError = "Choose a suggestion from this playlist.";
        return;
      }
      rule = { field, choice, label: `${field === "artist" ? "Artist is" : "Album is"} ${choice.name}` };
    } else if (field === "song") {
      const text = query.trim();
      if (!text) {
        draftError = "Enter some song title text first.";
        return;
      }
      rule = { field, text, operator: textOperator, label: `Song ${textOperator === "contains" ? "contains" : "has whole words"} “${text}”` };
    } else {
      const duration = cleanupDuration(query);
      if (duration === null) {
        draftError = "Enter a positive duration, such as 3:30 or 210 seconds.";
        return;
      }
      rule = { field, duration, operator: durationOperator, label: `${durationOperator === "shorter" ? "Shorter" : "Longer"} than ${duration % 1000 ? formatExactTime(duration) : formatTime(duration)}` };
    }
    const same = rules.some((existing) => existing.field === rule.field && (
      rule.choice ? existing.choice?.key === rule.choice.key
        : existing.operator === rule.operator && existing.text === rule.text && existing.duration === rule.duration
    ));
    if (!same) rules = [...rules, { ...rule, id: nextRule++ }];
    previewPage = 0;
    query = "";
    suggestionsOpen = false;
    activeChoice = -1;
    queueMicrotask(() => firstInput?.focus());
  }

  function suggestionKey(event) {
    if (field !== "artist" && field !== "album") return;
    if (event.key === "Escape" && suggestionsOpen) {
      event.preventDefault();
      event.stopPropagation();
      suggestionsOpen = false;
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      suggestionsOpen = true;
      if (!suggestions.length) return;
      activeChoice = event.key === "ArrowDown"
        ? (activeChoice + 1) % suggestions.length
        : (activeChoice < 0 ? suggestions.length - 1 : (activeChoice - 1 + suggestions.length) % suggestions.length);
      queueMicrotask(() => document.getElementById(`cleanup-choice-${activeChoice}`)?.scrollIntoView({ block: "nearest" }));
    } else if (event.key === "Enter" && suggestionsOpen && activeChoice >= 0) {
      event.preventDefault();
      addRule(suggestions[activeChoice]);
    }
  }

  async function reload() {
    if (reloading || busy) return;
    reloading = true;
    error = "";
    try {
      // Browse returns its cache immediately and emits a fresh playlist later.
      // Never replace the live detail with that possibly older return value.
      await api.browsePlaylist(playlist.id);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause || "Could not reload this playlist.");
    } finally {
      reloading = false;
    }
  }

  async function remove() {
    if (busy || !reviewing || !ready || removed !== null) return;
    // Recheck synchronously at the destructive boundary, not just in the UI.
    if (sourceVersion !== cleanupVersion(playlist)) return;
    const count = preview.rows.length;
    busy = true;
    error = "";
    try {
      await api.removePlaylistTracks(source.id, preview.uris, source.snapshot_id);
      removed = count;
      reviewing = false;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause || "Could not remove these songs. Try again.");
    } finally {
      busy = false;
    }
  }
</script>

<dialog
  class="confirm-dialog cleanup-dialog"
  bind:this={dialog}
  aria-labelledby="cleanup-title"
  aria-describedby="cleanup-description"
  aria-busy={busy}
  oncancel={nativeCancel}
  onclick={(event) => { if (event.target === dialog) close(); }}
>
  <div class="dialog-sheet cleanup-sheet">
    <header class="dialog-copy">
      <span class="tag">Playlist cleanup</span>
      <h2 id="cleanup-title">{removed !== null ? "Cleanup complete" : reviewing ? "Remove these songs?" : "Find songs to remove"}</h2>
      <p id="cleanup-description">
        {#if removed !== null}
          Removed {removed} {removed === 1 ? "playlist entry" : "playlist entries"} from “{source.name}”.
          The playlist refreshes automatically.
        {:else if reviewing}
          Remove these {preview.rows.length} entries from “{source.name}”? This cannot be undone here.
          Your liked songs and other playlists stay unchanged.
        {:else}
          Choose rules for “{source?.name ?? playlist.name}”. Nothing is removed until you review and confirm.
        {/if}
      </p>
    </header>

    {#if removed === null}
      {#if stale && !busy}
        <div class="cleanup-notice" role="alert">
          <p>This playlist changed. Your preview is out of date; nothing more can be removed from it.</p>
          <button class="btn-ghost" onclick={useLatest}>Review latest playlist</button>
        </div>
      {:else if !source?.snapshot_id || incomplete}
        <div class="cleanup-notice" role="status">
          <p>{incomplete ? "The full playlist is not loaded yet." : "Waiting for a verified playlist revision."} Reload before reviewing removals.</p>
          <button class="btn-ghost" disabled={reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
        </div>
      {/if}

      <fieldset disabled={frozen} class="cleanup-rules">
        <legend class="sr-only">Removal rules</legend>
        <label class="cleanup-grouping">
          Match
          <select aria-label="Combine rules" bind:value={grouping} onchange={() => { previewPage = 0; }}>
            <option value="all">all rules (AND)</option>
            <option value="any">any rule (OR)</option>
          </select>
        </label>
        <p class="cleanup-help">{grouping === "all" ? "A song must meet every rule. Use this to narrow your selection." : "A song can meet any rule. Use this for several artists or albums."}</p>

        {#if rules.length}
          <ul class="cleanup-chips" aria-label="Active removal rules">
            {#each rules as rule (rule.id)}
              <li>
                <span>{rule.label}</span>
                <button type="button" aria-label={`Remove rule: ${rule.label}`} onclick={() => { rules = rules.filter((item) => item.id !== rule.id); }}>×</button>
              </li>
            {/each}
          </ul>
        {/if}

        <form class="cleanup-builder" onsubmit={(event) => { event.preventDefault(); addRule(); }}>
          <label>
            Rule
            <select aria-label="Rule type" bind:value={field} onchange={changeField}>
              <option value="artist">Artist is</option>
              <option value="album">Album is</option>
              <option value="song">Song title</option>
              <option value="duration">Duration</option>
            </select>
          </label>
          {#if field === "song"}
            <label>
              Match text
              <select aria-label="Song text match" bind:value={textOperator}>
                <option value="contains">Contains text</option>
                <option value="words">Whole words</option>
              </select>
            </label>
          {:else if field === "duration"}
            <label>
              Compare
              <select aria-label="Duration comparison" bind:value={durationOperator}>
                <option value="shorter">Shorter than</option>
                <option value="longer">Longer than</option>
              </select>
            </label>
          {/if}
          <div class="cleanup-input-wrap">
            <label for="cleanup-value">{field === "artist" || field === "album" ? `Find ${field} in this playlist` : field === "song" ? "Song title text" : "Time (m:ss or seconds)"}</label>
            <input
              id="cleanup-value"
              bind:this={firstInput}
              bind:value={query}
              role={field === "artist" || field === "album" ? "combobox" : undefined}
              aria-autocomplete={field === "artist" || field === "album" ? "list" : undefined}
              aria-expanded={field === "artist" || field === "album" ? suggestionsOpen : undefined}
              aria-controls={field === "artist" || field === "album" ? "cleanup-suggestions" : undefined}
              aria-activedescendant={suggestionsOpen && activeChoice >= 0 ? `cleanup-choice-${activeChoice}` : undefined}
              aria-invalid={!!draftError}
              autocomplete="off"
              placeholder={field === "duration" ? "3:30" : field === "song" ? "e.g. live" : `Type an ${field} name…`}
              oninput={() => { activeChoice = -1; suggestionsOpen = field === "artist" || field === "album"; draftError = ""; }}
              onfocus={() => { suggestionsOpen = field === "artist" || field === "album"; }}
              onblur={() => { suggestionsOpen = false; }}
              onkeydown={suggestionKey}
            />
            {#if suggestionsOpen && !frozen && (field === "artist" || field === "album")}
              <div class="cleanup-suggestions" id="cleanup-suggestions" role="listbox" aria-label={`${field === "artist" ? "Artists" : "Albums"} in this playlist`}>
                {#each suggestions as choice, index (choice.key)}
                  {@const highlightStart = choice.name.toLowerCase().indexOf(query.trim().toLowerCase())}
                  {@const highlightEnd = highlightStart + query.trim().length}
                  <button
                    type="button"
                    role="option"
                    id={`cleanup-choice-${index}`}
                    aria-selected={activeChoice === index}
                    tabindex="-1"
                    onpointerdown={(event) => event.preventDefault()}
                    onclick={() => addRule(choice)}
                  >
                    <span>
                      {#if query.trim() && highlightStart >= 0}
                        {choice.name.slice(0, highlightStart)}<mark>{choice.name.slice(highlightStart, highlightEnd)}</mark>{choice.name.slice(highlightEnd)}
                      {:else}{choice.name}{/if}
                      {#if choice.hint}<small>{field === "artist" ? `Including ${choice.hint}` : choice.hint}</small>{/if}
                    </span>
                    <span class="num">{choice.count} {choice.count === 1 ? "entry" : "entries"}</span>
                  </button>
                {:else}
                  <p>No {field === "artist" ? "artists" : "albums"} in this playlist match.</p>
                {/each}
              </div>
            {/if}
          </div>
          {#if field === "song" || field === "duration"}
            <button type="submit" class="btn-ghost cleanup-add">Add rule</button>
          {/if}
        </form>
        {#if draftError}<p class="dialog-error" role="alert">{draftError}</p>{/if}
        <p class="cleanup-help">{field === "artist" || field === "album" ? "Select a suggestion, or use ↑ / ↓ and Enter. All suggestions come from this playlist." : field === "song" ? "Text is case-insensitive and literal. Whole words matches “live”, not “alive”." : "Duration comparisons are strict. Songs with unknown duration do not match."}</p>
      </fieldset>

      <section class="cleanup-preview" aria-labelledby="cleanup-preview-title">
        <div class="cleanup-preview-heading">
          <h3 id="cleanup-preview-title">{reviewing ? "Entries to remove" : "Removal preview"}</h3>
          <span class="num" role="status" aria-live="polite">{preview.rows.length} of {tracks.length} entries</span>
        </div>
        {#if preview.rows.length > preview.uris.length}
          <p class="cleanup-notice">Includes {preview.rows.length - preview.uris.length} duplicate {preview.rows.length - preview.uris.length === 1 ? "entry" : "entries"}. Every copy of each selected song will be removed from this playlist.</p>
        {/if}
        {#if preview.extraCount}
          <p class="cleanup-notice">{preview.extraCount} {preview.extraCount === 1 ? "entry has" : "entries have"} different details but is another copy of a selected song. These are marked below and will also be removed.</p>
        {/if}
        {#if preview.missingCount}
          <p class="cleanup-help">{preview.missingCount} matching {preview.missingCount === 1 ? "entry has" : "entries have"} no Spotify URI and cannot be removed. They are not included in the removal count.</p>
        {/if}
        <div class="cleanup-results" role="region" aria-label="Playlist entries that will be removed">
          {#if preview.rows.length}
            <table>
              <thead><tr><th scope="col">#</th><th scope="col">Song / artist</th><th scope="col">Album</th><th scope="col">Time</th></tr></thead>
              <tbody>
                {#each pageRows as row (row.index)}
                  <tr class:cleanup-extra={row.extra}>
                    <td class="num">{row.index + 1}</td>
                    <td><strong>{row.track.name || "Untitled song"}</strong><span>{(row.track.artist_names ?? []).join(", ") || "Unknown artist"}</span>{#if row.extra}<small>Another copy · also removed</small>{/if}</td>
                    <td>{row.track.album_name || "Unknown album"}</td>
                    <td class="num">{row.track.duration_ms > 0 ? formatTime(row.track.duration_ms) : "—"}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {:else}
            <p class="cleanup-empty">{rules.length ? "No removable songs match. Adjust your rules or try “any rule”." : "Add a rule to see exactly what would be removed."}</p>
          {/if}
        </div>
        {#if pageCount > 1}
          <nav class="cleanup-pagination" aria-label="Removal preview pages">
            <button class="btn-ghost" disabled={page === 0} onclick={() => { previewPage = page - 1; }}>Previous</button>
            <span class="num" role="status">{page * PREVIEW_PAGE_SIZE + 1}–{Math.min((page + 1) * PREVIEW_PAGE_SIZE, preview.rows.length)} of {preview.rows.length}</span>
            <button class="btn-ghost" disabled={page === pageCount - 1} onclick={() => { previewPage = page + 1; }}>Next</button>
          </nav>
        {/if}
      </section>
    {/if}

    {#if error}
      <div class="dialog-error cleanup-error" role="alert">
        <p>{error}</p>
        <button class="btn-ghost" disabled={busy || reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
      </div>
    {/if}
    <footer class="dialog-actions">
      {#if removed !== null}
        <button class="btn-accent" onclick={close}>Done</button>
      {:else}
        <button class="btn-ghost" disabled={busy} onclick={close}>Cancel</button>
        {#if reviewing}
          <button class="btn-ghost" disabled={busy} onclick={() => { reviewing = false; error = ""; }}>Edit rules</button>
          <button class="btn-danger" disabled={busy || !ready} onclick={remove}>{busy ? "Removing…" : `${error ? "Retry removing" : "Remove"} ${preview.rows.length} entries`}</button>
        {:else}
          <button class="btn-accent" disabled={!ready} onclick={() => { reviewing = true; suggestionsOpen = false; }}>Review {preview.rows.length} removals</button>
        {/if}
      {/if}
    </footer>
  </div>
</dialog>

<style>
  .cleanup-dialog { width: min(800px, calc(100vw - var(--s8))); max-height: calc(100dvh - var(--s8)); overflow: auto; }
  .cleanup-sheet { display: flex; flex-direction: column; gap: var(--s5); }
  .cleanup-sheet .dialog-actions { margin-top: 0; flex-wrap: wrap; position: sticky; bottom: 0; z-index: 5; background: var(--bg-sheet); padding-block: var(--s3); }
  .cleanup-rules { border: 0; padding: 0; margin: 0; min-width: 0; }
  .cleanup-grouping { display: flex; align-items: center; gap: var(--s2); }
  .cleanup-help { color: var(--fg-2); font-size: var(--t-12); margin-top: var(--s2); }
  .cleanup-builder { display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--s2); margin-top: var(--s4); }
  .cleanup-builder label, .cleanup-input-wrap > label { display: flex; flex-direction: column; gap: var(--s2); color: var(--fg-1); font-size: var(--t-12); }
  select, input { min-height: 36px; border: 1px solid var(--line-2); border-radius: var(--r2); background: var(--bg-1); color: var(--fg); padding: 0 var(--s3); font: inherit; }
  input { width: 100%; margin-top: var(--s2); }
  select:focus-visible, input:focus-visible, button:focus-visible, .cleanup-results:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .cleanup-input-wrap { flex: 1 1 220px; position: relative; min-width: 0; }
  .cleanup-add { height: 36px; }
  .cleanup-chips { display: flex; flex-wrap: wrap; gap: var(--s2); list-style: none; padding: 0; margin: var(--s3) 0 0; }
  .cleanup-chips li { display: flex; align-items: center; gap: var(--s2); border: 1px solid var(--line-2); border-radius: var(--r2); padding-left: var(--s3); background: var(--bg-1); font-size: var(--t-12); max-width: 100%; }
  .cleanup-chips li span { overflow-wrap: anywhere; }
  .cleanup-chips button { padding: var(--s2) var(--s3); color: var(--fg-1); font-size: 18px; }
  .cleanup-suggestions { position: absolute; top: 100%; left: 0; right: 0; z-index: 4; max-height: 240px; overflow-y: auto; background: var(--bg-sheet); border: 1px solid var(--line-2); border-radius: var(--r2); box-shadow: 0 12px 30px #0008; }
  .cleanup-suggestions button { width: 100%; text-align: left; display: flex; align-items: center; justify-content: space-between; gap: var(--s3); padding: var(--s3); font-size: var(--t-13); }
  .cleanup-suggestions button:hover, .cleanup-suggestions button[aria-selected="true"] { background: var(--bg-2); }
  .cleanup-suggestions small { display: block; color: var(--fg-2); font-size: 10px; margin-top: 2px; }
  .cleanup-suggestions .num { white-space: nowrap; color: var(--fg-2); font-size: var(--t-12); }
  .cleanup-suggestions mark { background: transparent; color: var(--accent); font-weight: var(--w-bold); }
  .cleanup-pagination { display: flex; align-items: center; justify-content: space-between; gap: var(--s2); margin-top: var(--s2); font-size: var(--t-12); }
  .cleanup-suggestions > p { padding: var(--s3); color: var(--fg-2); font-size: var(--t-12); }
  .cleanup-preview-heading { display: flex; justify-content: space-between; align-items: baseline; gap: var(--s3); margin-bottom: var(--s3); }
  .cleanup-preview-heading h3 { font-size: var(--t-14); font-weight: var(--w-bold); }
  .cleanup-preview-heading > span { color: var(--fg-1); font-size: var(--t-12); }
  .cleanup-results { max-height: 290px; overflow: auto; border: 1px solid var(--line); border-radius: var(--r2); }
  table { width: 100%; border-collapse: collapse; font-size: var(--t-12); text-align: left; }
  th { position: sticky; top: 0; z-index: 1; background: var(--bg-sheet); color: var(--fg-2); font-weight: normal; }
  td, th { padding: var(--s2) var(--s3); border-bottom: 1px solid var(--line); }
  td { color: var(--fg-1); overflow-wrap: anywhere; }
  td:first-child, td:last-child { white-space: nowrap; }
  td strong { display: block; color: var(--fg); font-weight: var(--w-med); }
  td span, td small { display: block; margin-top: 2px; }
  td small { color: var(--love); }
  .cleanup-extra { background: color-mix(in srgb, var(--love) 5%, transparent); }
  .cleanup-empty { padding: var(--s5); color: var(--fg-2); font-size: var(--t-13); }
  .cleanup-notice { padding: var(--s3); background: var(--bg-1); border-left: 2px solid var(--love); font-size: var(--t-12); color: var(--fg-1); margin-bottom: var(--s2); }
  .cleanup-notice button, .cleanup-error button { margin-top: var(--s2); }
  .dialog-error { color: var(--love); font-size: var(--t-13); }
  @media (max-width: 600px) {
    .cleanup-dialog { width: calc(100vw - var(--s4)); max-height: calc(100dvh - var(--s4)); }
    .cleanup-sheet { padding: var(--s4); }
    .cleanup-builder > label { flex: 1; }
    .cleanup-input-wrap { flex-basis: 100%; }
    .cleanup-results { max-height: 230px; }
    td, th { padding: var(--s2); }
  }
</style>
