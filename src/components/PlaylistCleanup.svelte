<script>
  import Icon from "./Icon.svelte";
  import Select from "./Select.svelte";
  import { api } from "../lib/state.svelte.js";
  import { formatTime, formatExactTime } from "../lib/time.js";
  import {
    cleanupChoices, filterCleanupChoices, cleanupDuration, cleanupPreview, cleanupVersion,
  } from "../lib/playlist-cleanup.js";

  /**
   * The four menus are the app's own listbox. A native <select> paints its
   * button — and unavoidably its popup — with OS chrome: light grey, its own
   * font, its own radius. On this sheet it was the one shape nothing in the
   * design system could reach.
   */
  const GROUPINGS = [
    { value: "all", label: "all rules (AND)" },
    { value: "any", label: "any rule (OR)" },
  ];
  const FIELDS = [
    { value: "artist", label: "Artist is" },
    { value: "album", label: "Album is" },
    { value: "song", label: "Song title" },
    { value: "duration", label: "Duration" },
  ];
  const TEXT_MATCHES = [
    { value: "contains", label: "Contains text" },
    { value: "words", label: "Whole words" },
  ];
  const DURATION_MATCHES = [
    { value: "shorter", label: "Shorter than" },
    { value: "longer", label: "Longer than" },
  ];
  /**
   * The suggestion list's preferred height, mirroring its max-height. The list
   * is dropped above the field instead of below it when the scrolling body
   * cannot give it that much room, and is capped to whichever side it lands
   * on: the body clips what leaves it, and a menu with invisible rows is worse
   * than a short one. The floor is two rows plus their captions — below that
   * the list stops being a list.
   */
  const SUGGESTION_ROOM = 200;
  const SUGGESTION_MIN = 120;

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
  let suggestionsUp = $state(false);
  let suggestionsRoom = $state(SUGGESTION_ROOM);
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

  function changeField(value) {
    field = value;
    query = "";
    draftError = "";
    activeChoice = -1;
    suggestionsOpen = false;
  }

  /** Measures only when a list is about to open, never per keystroke: which
      side it hangs on and how tall it may be are properties of the scroll
      position, not of the query. */
  function openSuggestions() {
    if (field !== "artist" && field !== "album") return;
    let up = false;
    let room = SUGGESTION_ROOM;
    const wrap = firstInput?.closest(".cleanup-input-wrap");
    const body = wrap?.closest(".cleanup-body");
    if (wrap && body) {
      const wrapRect = wrap.getBoundingClientRect();
      const bodyRect = body.getBoundingClientRect();
      const below = bodyRect.bottom - parseFloat(getComputedStyle(body).paddingBottom) - wrapRect.bottom;
      const above = wrapRect.top - bodyRect.top;
      up = below < SUGGESTION_ROOM && above > below;
      room = Math.max(SUGGESTION_MIN, Math.min(SUGGESTION_ROOM, (up ? above : below) - 4));
    }
    suggestionsUp = up;
    suggestionsRoom = room;
    suggestionsOpen = true;
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
  <div class="cleanup-sheet">
    <header class="cleanup-head">
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
      {#if removed === null}
        <button type="button" class="btn-icon cleanup-close" aria-label="Close" title="Close" disabled={busy} onclick={close}>
          <Icon name="x" size={16} />
        </button>
      {/if}
    </header>

    {#if removed === null}
      <div class="cleanup-body">
        {#if stale && !busy}
          <div class="cleanup-notice alert" role="alert">
            <p>This playlist changed. Your preview is out of date; nothing more can be removed from it.</p>
            <button type="button" class="btn-ghost" onclick={useLatest}>Review latest playlist</button>
          </div>
        {:else if !source?.snapshot_id || incomplete}
          <div class="cleanup-notice" role="status">
            <p>{incomplete ? "The full playlist is not loaded yet." : "Waiting for a verified playlist revision."} Reload before reviewing removals.</p>
            <button type="button" class="btn-ghost" disabled={reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
          </div>
        {/if}

        <fieldset class="cleanup-rules" disabled={frozen}>
          <div class="cleanup-band">
            <h3 class="caps">Rules</h3>
            <span class="cleanup-match">
              <span class="caps" aria-hidden="true">Match</span>
              <Select
                label="Match rules"
                options={GROUPINGS}
                value={grouping}
                disabled={frozen}
                onchange={(value) => { grouping = value; previewPage = 0; }}
              />
            </span>
          </div>
          <p class="cleanup-help">{grouping === "all" ? "A song must meet every rule. Use this to narrow your selection." : "A song can meet any rule. Use this for several artists or albums."}</p>

          {#if rules.length}
            <ul class="chips cleanup-chips" aria-label="Active removal rules">
              {#each rules as rule (rule.id)}
                <li class="chip">
                  <span class="label" title={rule.label}>{rule.label}</span>
                  <button
                    type="button"
                    class="x"
                    title="Remove rule"
                    aria-label={`Remove rule: ${rule.label}`}
                    onclick={() => { rules = rules.filter((item) => item.id !== rule.id); }}
                  >
                    <Icon name="x" size={11} />
                  </button>
                </li>
              {/each}
            </ul>
          {/if}

          <form class="cleanup-builder" onsubmit={(event) => { event.preventDefault(); addRule(); }}>
            <div class="cleanup-field">
              <span class="caps" aria-hidden="true">Rule</span>
              <Select label="Rule type" options={FIELDS} value={field} disabled={frozen} onchange={changeField} />
            </div>
            {#if field === "song"}
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Match text</span>
                <Select label="Match text" options={TEXT_MATCHES} value={textOperator} disabled={frozen} onchange={(value) => (textOperator = value)} />
              </div>
            {:else if field === "duration"}
              <div class="cleanup-field">
                <span class="caps" aria-hidden="true">Compare</span>
                <Select label="Compare duration" options={DURATION_MATCHES} value={durationOperator} disabled={frozen} onchange={(value) => (durationOperator = value)} />
              </div>
            {/if}
            <div class="cleanup-field cleanup-input-wrap">
              <label class="caps" for="cleanup-value">{field === "artist" || field === "album" ? `Find ${field} in this playlist` : field === "song" ? "Song title text" : "Time (m:ss or seconds)"}</label>
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
                oninput={() => { activeChoice = -1; openSuggestions(); draftError = ""; }}
                onfocus={openSuggestions}
                onblur={() => { suggestionsOpen = false; }}
                onkeydown={suggestionKey}
              />
              {#if suggestionsOpen && !frozen && (field === "artist" || field === "album")}
                <div class="cleanup-suggestions" class:up={suggestionsUp} style:max-height="{suggestionsRoom}px" id="cleanup-suggestions" role="listbox" aria-label={`${field === "artist" ? "Artists" : "Albums"} in this playlist`}>
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
                      <span class="tnum">{choice.count} {choice.count === 1 ? "entry" : "entries"}</span>
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
          {#if draftError}<p class="inline-error" role="alert">{draftError}</p>{/if}
          <p class="cleanup-help">{field === "artist" || field === "album" ? "Select a suggestion, or use ↑ / ↓ and Enter. All suggestions come from this playlist." : field === "song" ? "Text is case-insensitive and literal. Whole words matches “live”, not “alive”." : "Duration comparisons are strict. Songs with unknown duration do not match."}</p>
        </fieldset>

        <section class="cleanup-preview" aria-labelledby="cleanup-preview-title">
          <div class="cleanup-band">
            <h3 class="caps" id="cleanup-preview-title">{reviewing ? "Entries to remove" : "Removal preview"}</h3>
            <span class="cleanup-count tnum" role="status">{preview.rows.length} of {tracks.length} entries</span>
          </div>
          {#if preview.rows.length > preview.uris.length || preview.extraCount || preview.missingCount}
            <ul class="cleanup-notes">
              {#if preview.rows.length > preview.uris.length}
                <li class="warn">
                  <span class="dot" aria-hidden="true"></span>
                  Includes {preview.rows.length - preview.uris.length} duplicate {preview.rows.length - preview.uris.length === 1 ? "entry" : "entries"}. Every copy of each selected song will be removed from this playlist.
                </li>
              {/if}
              {#if preview.extraCount}
                <li class="warn">
                  <span class="dot" aria-hidden="true"></span>
                  {preview.extraCount} {preview.extraCount === 1 ? "entry has" : "entries have"} different details but is another copy of a selected song. These are marked below and will also be removed.
                </li>
              {/if}
              {#if preview.missingCount}
                <li>
                  <span class="dot" aria-hidden="true"></span>
                  {preview.missingCount} matching {preview.missingCount === 1 ? "entry has" : "entries have"} no Spotify URI and cannot be removed. They are not included in the removal count.
                </li>
              {/if}
            </ul>
          {/if}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex
               A hundred rows a page against a window of six: without focus the
               rest of a page is unreachable from the keyboard, since the
               pagination only ever moves in hundred-row steps. Same deliberate
               exception Select.svelte makes for its own list. -->
          <div class="cleanup-results" role="region" tabindex="0" aria-label="Playlist entries that will be removed">
            {#if preview.rows.length}
              <table>
                <colgroup>
                  <col class="cleanup-col-idx" />
                  <col />
                  <col class="cleanup-col-album" />
                  <col class="cleanup-col-time" />
                </colgroup>
                <thead>
                  <tr>
                    <th class="caps" scope="col">#</th>
                    <th class="caps" scope="col">Song / artist</th>
                    <th class="caps" scope="col">Album</th>
                    <th class="caps" scope="col">Time</th>
                  </tr>
                </thead>
                <tbody>
                  {#each pageRows as row (row.index)}
                    <tr class:cleanup-extra={row.extra}>
                      <td class="cleanup-idx tnum">{row.index + 1}</td>
                      <td class="cleanup-song">
                        <strong>{row.track.name || "Untitled song"}</strong>
                        <span>{(row.track.artist_names ?? []).join(", ") || "Unknown artist"}</span>
                        {#if row.extra}<small>Another copy · also removed</small>{/if}
                      </td>
                      <td class="cleanup-album">{row.track.album_name || "Unknown album"}</td>
                      <td class="cleanup-time tnum">{row.track.duration_ms > 0 ? formatTime(row.track.duration_ms) : "—"}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {:else}
              <div class="empty cleanup-empty">
                {#if rules.length}
                  <p class="h">No removable songs match</p>
                  <p class="sub">Adjust your rules, or switch Match to “any rule”.</p>
                {:else}
                  <p class="h">Nothing to preview yet</p>
                  <p class="sub">Add a rule above to see every entry it would remove.</p>
                {/if}
              </div>
            {/if}
          </div>
          {#if pageCount > 1}
            <nav class="cleanup-pagination" aria-label="Removal preview pages">
              <button class="btn-ghost" disabled={page === 0} onclick={() => { previewPage = page - 1; }}>Previous</button>
              <span class="cleanup-count tnum" role="status">{page * PREVIEW_PAGE_SIZE + 1}–{Math.min((page + 1) * PREVIEW_PAGE_SIZE, preview.rows.length)} of {preview.rows.length}</span>
              <button class="btn-ghost" disabled={page === pageCount - 1} onclick={() => { previewPage = page + 1; }}>Next</button>
            </nav>
          {/if}
        </section>
      </div>
    {/if}

    {#if error}
      <div class="cleanup-error" role="alert">
        <p>{error}</p>
        <button type="button" class="btn-ghost" disabled={busy || reloading} onclick={reload}>{reloading ? "Reloading…" : "Reload playlist"}</button>
      </div>
    {/if}

    <footer class="dialog-actions cleanup-foot">
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
  /* THREE BANDS — head, scrolling body, actions — instead of one long sheet
     with a sticky footer.

     The sticky footer is what drew the buttons over the content. It sat at
     z-index 5 while the suggestion list opens at 4, so the action row painted
     across the artist suggestions underneath it; and because `sticky` was
     resolved against the dialog's own scroller, it also slid over the tail of
     the preview table. A band that never moves cannot cover anything, and both
     popups now live inside the band that scrolls — `overflow-y: auto` clips
     them at the actions rather than under them. */
  .cleanup-dialog {
    display: flex; flex-direction: column;
    width: min(860px, calc(100vw - var(--s8)));
    max-height: calc(100dvh - var(--s8));
    overflow: hidden;
  }
  .cleanup-sheet { display: flex; flex-direction: column; flex: 1; min-height: 0; padding: 0; }

  /* --- head. Fixed, so the thing you are about to do never scrolls away. */
  .cleanup-head {
    position: relative; flex: none;
    padding: var(--s5) var(--s8) var(--s4) var(--s6);
    border-bottom: 1px solid var(--line);
  }
  .cleanup-head h2 {
    margin-top: var(--s2);
    font-family: var(--font-display); font-size: var(--t-23);
    font-weight: var(--w-bold); letter-spacing: -0.02em; line-height: 1.15;
  }
  .cleanup-head > p { margin-top: var(--s2); max-width: 66ch; color: var(--fg-1); font-size: var(--t-13); }
  .cleanup-close { position: absolute; top: var(--s4); right: var(--s5); color: var(--fg-2); }

  /* --- body. The only scroller. The rule builder is fixed and the preview
     takes the slack, so a crowded rule list scrolls the body while a short one
     leaves the dialog exactly as tall as its content. */
  .cleanup-body {
    flex: 1 1 auto; min-height: 0; overflow-y: auto;
    display: flex; flex-direction: column; gap: var(--s4);
    padding: var(--s4) var(--s6) var(--s5);
  }

  /* A blocking notice is a plate with its action on the same line as its
     sentence: stacked, the two made a tall box with a stranded button. */
  .cleanup-notice {
    flex: none; display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4); padding: var(--s3) var(--s4);
    border-radius: var(--r2); background: var(--bg-2);
    color: var(--fg-1); font-size: var(--t-12);
  }
  .cleanup-notice.alert { background: var(--danger-wash); }
  .cleanup-notice p { min-width: 0; }
  .cleanup-notice button { flex: none; }

  .cleanup-rules { flex: none; border: 0; padding: 0; margin: 0; min-width: 0; }

  /* A section header: the field label at one end, the control or the count it
     governs at the other. This is the dialog's whole hierarchy — no boxes. */
  .cleanup-band {
    display: flex; align-items: center; justify-content: space-between;
    gap: var(--s3); min-height: 24px;
  }
  .cleanup-band .caps { margin: 0; }
  .cleanup-match { display: flex; align-items: center; gap: var(--s2); }
  .cleanup-count { color: var(--count); font-weight: var(--w-med); font-size: var(--t-12); }

  .cleanup-help { margin-top: var(--s2); color: var(--fg-2); font-size: var(--t-12); }

  /* The chips are the app's shared pill (see .chip in app.css) rather than a
     local rectangle: same height, same fill, same round × plate as the search
     filters. `title` carries the label a 220px cap ellipsises. */
  .cleanup-chips { margin-top: var(--s3); }

  .cleanup-builder { display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--s3); margin-top: var(--s4); }
  .cleanup-field { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .cleanup-field .caps { margin: 0; }
  /* 34px tall, like every control it stands beside in this row. */
  .cleanup-input-wrap { flex: 1 1 260px; position: relative; }
  .cleanup-input-wrap input {
    width: 100%; height: 34px; padding: 0 var(--s3);
    border: 1px solid var(--line-2); border-radius: var(--r2);
    background: var(--bg-2); color: var(--fg);
    font: inherit; font-size: var(--t-13);
  }
  .cleanup-input-wrap input::placeholder { color: var(--fg-3); }
  .cleanup-add { height: 34px; }

  /* The app's one menu material — the same fill, radius, hairline and shadow
     as .sel-list and .menu, so the two dropdowns on this sheet are one object. */
  .cleanup-suggestions {
    position: absolute; z-index: 3; top: calc(100% + var(--s1)); left: 0; right: 0;
    max-height: 200px; overflow-y: auto; padding: var(--s1);
    border: 1px solid var(--line-2); border-radius: var(--r2);
    background: var(--bg-2);
    box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.85), 0 2px 6px rgba(0, 0, 0, 0.5);
  }
  .cleanup-suggestions.up { top: auto; bottom: calc(100% + var(--s1)); }
  .cleanup-suggestions button {
    display: flex; align-items: center; justify-content: space-between; gap: var(--s3);
    width: 100%; min-height: 32px; padding: var(--s1) var(--s2) var(--s1) var(--s3);
    border-radius: var(--r1); color: var(--fg-1); font-size: var(--t-13); text-align: left;
  }
  /* Same active plate as the listbox: hover and the arrow-key position are the
     same fact, so they are the same colour. */
  .cleanup-suggestions button:hover,
  .cleanup-suggestions button[aria-selected="true"] { background: var(--bg-4); color: var(--fg); }
  .cleanup-suggestions button > span:first-child { min-width: 0; }
  .cleanup-suggestions small {
    display: block; margin-top: 1px;
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-2);
  }
  .cleanup-suggestions .tnum { white-space: nowrap; color: var(--count); font-size: var(--t-12); font-weight: var(--w-med); }
  .cleanup-suggestions mark { background: transparent; color: var(--accent); font-weight: var(--w-bold); }
  .cleanup-suggestions > p { padding: var(--s3); color: var(--fg-2); font-size: var(--t-12); }

  /* --- preview. The rule that opens it leads with the accent and fades into
     the ordinary hairline, the way every structural rule in this app does.
     Shrinkable on purpose: on a short window the table is what gives way, so
     its own paging row stays above the actions instead of below the fold. */
  .cleanup-preview {
    display: flex; flex-direction: column;
    flex: 0 1 auto; min-height: 0;
    padding-top: var(--s4);
    border-top: 1px solid transparent;
    border-image: var(--rule-accent) 1;
  }
  .cleanup-notes { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s3); list-style: none; padding: 0; }
  .cleanup-notes li {
    display: flex; align-items: center; gap: var(--s2);
    color: var(--fg-1); font-size: var(--t-12);
  }
  .cleanup-notes .dot { flex: none; width: 5px; height: 5px; border-radius: 50%; background: var(--fg-3); }
  .cleanup-notes .warn .dot { background: var(--love); }

  /* Capped, then scrolled: 250 entries must not push the actions off the
     bottom, and the head stays put while the rows move under it. The inset
     rule closes the region and marks the row a scroll cut mid-height as a
     cut, rather than as a clipping accident. */
  .cleanup-results {
    flex: 1 1 auto; min-height: 96px; max-height: min(320px, 32dvh); overflow: auto;
    margin-top: var(--s3);
    box-shadow: inset 0 -1px 0 var(--line);
  }
  .cleanup-results table { width: 100%; border-collapse: collapse; table-layout: fixed; text-align: left; }
  .cleanup-results th {
    height: 28px; padding: 0 var(--s3);
    background: var(--bg-sheet);
    position: sticky; top: 0; z-index: 1;
    box-shadow: inset 0 -1px 0 var(--line-2);
    text-align: left;
  }
  .cleanup-results td { padding: var(--s2) var(--s3); vertical-align: middle; border-bottom: 1px solid var(--line); }
  /* Flush with the section labels above: the table's outer edges are the
     dialog's text edge, so only the inner columns get padding. */
  .cleanup-results th:first-child, .cleanup-results td:first-child { padding-left: 0; }
  .cleanup-results th:last-child, .cleanup-results td:last-child { padding-right: 0; }
  .cleanup-col-idx { width: 48px; }
  .cleanup-col-album { width: 30%; }
  .cleanup-col-time { width: 62px; }
  /* A column head sits over its values, and both of these are right-aligned. */
  .cleanup-results th:first-child, .cleanup-results th:last-child { text-align: right; }

  .cleanup-idx {
    font-family: var(--font-number); font-size: var(--t-12); font-weight: var(--w-med);
    color: var(--fg-3); text-align: right;
  }
  .cleanup-song strong {
    display: block; color: var(--fg); font-size: var(--t-13); font-weight: var(--w-med);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .cleanup-song span {
    display: block; margin-top: 1px;
    font-family: var(--font-small); font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .cleanup-song small { display: block; margin-top: 1px; color: var(--love); font-size: var(--t-11); font-weight: var(--w-med); }
  .cleanup-album { color: var(--fg-2); font-size: var(--t-12); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cleanup-time { color: var(--fg-2); font-family: var(--font-number); font-size: var(--t-12); text-align: right; }
  /* Love is the palette's "you cannot undo it", so the rows that go as a
     second copy of a selected song are washed in it rather than left to a
     small caption to explain. */
  tr.cleanup-extra { background: var(--danger-wash); }

  /* Reserved height, message centred in it. The block is the whole preview
     until rows exist, so it holds the space the table will take — and that
     space is what the suggestion list above it drops into. */
  .cleanup-empty { display: flex; flex-direction: column; justify-content: center; min-height: 132px; padding: 0; }
  .cleanup-pagination { display: flex; align-items: center; justify-content: space-between; gap: var(--s3); margin-top: var(--s3); }

  /* The failure of the one irreversible action, edge to edge: it belongs to
     the whole sheet, not to the section the rules happen to be in. */
  .cleanup-error {
    flex: none; display: flex; align-items: center; justify-content: space-between;
    gap: var(--s4); padding: var(--s3) var(--s6);
    background: var(--danger-wash); color: var(--love); font-size: var(--t-13);
  }
  .cleanup-error button { flex: none; }

  .cleanup-foot { flex: none; margin-top: 0; padding: var(--s4) var(--s6); border-top: 1px solid var(--line); flex-wrap: wrap; }

  @media (max-width: 600px) {
    .cleanup-dialog { width: calc(100vw - var(--s4)); max-height: calc(100dvh - var(--s4)); }
    .cleanup-head, .cleanup-error, .cleanup-foot { padding-inline: var(--s4); }
    .cleanup-body { padding-inline: var(--s4); }
    .cleanup-field { flex: 1 1 100%; }
    .cleanup-results { max-height: 46dvh; }
  }
</style>
