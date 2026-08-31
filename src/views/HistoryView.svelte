<script>
  import { untrack } from "svelte";
  import { api } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import Select from "../components/Select.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { formatTime } from "../lib/time.js";
  import { observeStuck } from "../lib/sticky.js";
  import { rowWindow } from "../lib/virtual.js";

  const ROW_H = 56;
  const OVERSCAN = 6;
  const SORTS = [
    { value: "recent", label: "Newest first" },
    { value: "oldest", label: "Oldest first" },
    { value: "title", label: "Song title" },
    { value: "artist", label: "Artist" },
  ];

  let history = $state([]);
  let query = $state("");
  let sort = $state("recent");
  let loading = $state(false);
  let loaded = $state(false);
  let error = $state("");
  let confirmClear = $state(false);
  let clearing = $state(false);
  let clearError = $state("");

  let bodyEl = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  let curFirst = 0;
  let curLast = 0;
  let resetFrame = 0;

  const dateFormatter = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" });
  const yearFormatter = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });
  const timeFormatter = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
  const countFormatter = new Intl.NumberFormat();
  const collator = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

  function startOfDay(date) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  }

  /* The when-cell is a two-line stack: the day on top, the clock under it.
     A bare "3:49" next to the Played column's "1:12" read as two durations;
     a day word cannot be mistaken for one, and the column of days is what
     makes the journal scannable. */
  function whenDay(timestamp) {
    const date = new Date(timestamp);
    const today = startOfDay(new Date());
    const day = startOfDay(date);
    if (day === today) return "Today";
    if (day === today - 86_400_000) return "Yest.";
    const now = new Date();
    if (date.getFullYear() === now.getFullYear()) return dateFormatter.format(date);
    return yearFormatter.format(date);
  }

  function whenClock(timestamp) {
    return timeFormatter.format(new Date(timestamp));
  }

  function contextLabel(context) {
    if (!context) return "Unknown source";
    if (context === "liked") return "Liked Songs";
    if (context === "search") return "Search";
    if (context === "history") return "History";
    const [kind] = context.split(":", 1);
    return ({ playlist: "Playlist", album: "Album", artist: "Artist", radio: "Radio" })[kind] || "Queue";
  }

  /* A quiet glyph per kind of source, so the column scans by shape before
     its text is read. Liked is the one warm mark — it is the "yours" hue. */
  function contextIcon(context) {
    if (!context) return null;
    if (context === "liked") return "heart-f";
    if (context === "search") return "search";
    const [kind] = context.split(":", 1);
    return ({ playlist: "queue", album: "note", artist: "note", radio: "shuffle" })[kind] || null;
  }

  /* How much of the song a skipped play actually got through — drawn as a
     micro seek-rail so a partial reads as a shape, not as a bare duration. */
  function playedPct(entry) {
    const total = entry.track.duration_ms || 0;
    if (total <= 0) return 0;
    return Math.min(100, Math.max(3, Math.round((entry.ms_played / total) * 100)));
  }

  function titleOf(entry) {
    return entry.track.name;
  }

  function artistOf(entry) {
    return (entry.track.artist_names ?? []).join(", ");
  }

  const filtered = $derived.by(() => {
    const needle = query.trim().toLocaleLowerCase();
    const rows = needle
      ? history.filter((entry) =>
          `${titleOf(entry)}\n${artistOf(entry)}`.toLocaleLowerCase().includes(needle),
        )
      : [...history];

    if (sort === "oldest") {
      rows.sort((a, b) => Number(a.started_at) - Number(b.started_at));
    } else if (sort === "title") {
      rows.sort((a, b) =>
        collator.compare(titleOf(a), titleOf(b)) ||
        Number(b.started_at) - Number(a.started_at),
      );
    } else if (sort === "artist") {
      rows.sort((a, b) =>
        collator.compare(artistOf(a), artistOf(b)) ||
        collator.compare(titleOf(a), titleOf(b)) ||
        Number(b.started_at) - Number(a.started_at),
      );
    }
    return rows;
  });

  const visible = $derived(filtered.slice(firstRow, lastRow));
  const filtering = $derived(query.trim().length > 0);

  async function loadHistory() {
    if (loading) return;
    loading = true;
    loaded = false;
    error = "";
    try {
      history = (await api.getHistory()) ?? [];
      loaded = true;
    } catch (reason) {
      error = String(reason || "Could not load listening history.");
      loaded = true;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    untrack(() => loadHistory());
  });

  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    const { first, last } = rowWindow(bodyEl, scroller, ROW_H, OVERSCAN, filtered.length);
    if (first === curFirst && last === curLast) return;
    curFirst = first;
    curLast = last;
    firstRow = first;
    lastRow = last;
  }
  function resetWindow() {
    curFirst = 0;
    curLast = 0;
    firstRow = 0;
    lastRow = 0;
    const scroller = bodyEl?.closest(".scroll");
    if (!scroller) return;
    scroller.scrollTop = 0;
    if (resetFrame) cancelAnimationFrame(resetFrame);
    resetFrame = requestAnimationFrame(() => {
      resetFrame = 0;
      measure(scroller);
    });
  }

  $effect(() => {
    query;
    sort;
    untrack(resetWindow);
    return () => {
      if (resetFrame) cancelAnimationFrame(resetFrame);
      resetFrame = 0;
    };
  });

  $effect(() => {
    filtered.length;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) return;
    let scrollFrame = 0;
    const onScroll = () => {
      if (scrollFrame) return;
      scrollFrame = requestAnimationFrame(() => {
        scrollFrame = 0;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const observer = new ResizeObserver(() => measure(scroller));
    observer.observe(scroller);
    untrack(() => measure(scroller));
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      observer.disconnect();
      if (scrollFrame) cancelAnimationFrame(scrollFrame);
    };
  });
  let toolsSentinel = $state(null);
  let toolsStuck = $state(false);
  $effect(() => observeStuck(toolsSentinel, (stuck) => (toolsStuck = stuck)));

  async function clearAll() {
    if (clearing) return;
    clearing = true;
    clearError = "";
    try {
      await api.clearHistory();
      history = [];
      query = "";
      confirmClear = false;
    } catch (reason) {
      clearError = String(reason || "Could not clear listening history.");
    } finally {
      clearing = false;
    }
  }

  function replay(entry) {
    api.playQueue([entry.track], 0, "history").catch(() => {});
  }
</script>

<section class="view page history-page">
  <header class="history-head">
    <div class="history-head-copy">
      <span class="tag">Local</span>
      <h1 class="page-title">Listening history</h1>
      <p>Your latest plays, stored only on this computer.</p>
    </div>
  </header>

  <div class="history-tools-sentinel" bind:this={toolsSentinel} aria-hidden="true"></div>
  <!-- One quiet instrument row: what is listed, the filter, the order, and the
       one destructive action. The controls say what they are; the bar carries
       no labels of its own and no chrome until it sticks. -->
  <div class="history-tools" class:stuck={toolsStuck}>
    <div class="history-summary" aria-live="polite">
      <Icon name="clock" size={15} />
      {#if loaded && !error}
        <span class="caps">{filtering ? "Showing" : "Recorded"}</span>
        <span class="history-count">
          <strong class="tnum">{countFormatter.format(filtered.length)}</strong>
          {filtered.length === 1 ? "play" : "plays"}
          {#if filtering}<span class="history-total">of <span class="tnum">{countFormatter.format(history.length)}</span></span>{/if}
        </span>
      {:else if error}
        <span class="caps">History</span>
        <span class="history-count">Unavailable</span>
      {:else}
        <span class="caps">History</span>
        <span class="history-count">Loading…</span>
      {/if}
    </div>
    <div class="history-control history-filter-control">
      <span class="history-filter">
        <Icon name="search" size={13} />
        <input
          bind:value={query}
          aria-label="Filter listening history"
          placeholder="Song or artist"
          spellcheck="false"
          disabled={!loaded || !!error}
          onkeydown={(event) => event.key === "Escape" && (query = "")}
        />
        {#if query}
          <button class="history-filter-clear" type="button" aria-label="Clear filter" title="Clear filter" onclick={() => (query = "")}>
            <Icon name="x" size={11} />
          </button>
        {/if}
      </span>
    </div>
    <div class="history-control history-sort-control">
      <Select
        label="Sort listening history"
        options={SORTS}
        value={sort}
        disabled={!loaded || !!error}
        onchange={(value) => (sort = value)}
      />
    </div>
    <button
      class="btn-ghost history-clear"
      type="button"
      aria-label="Clear listening history"
      disabled={!loaded || loading || !history.length}
      onclick={() => (confirmClear = true)}
    >
      <Icon name="x" size={13} /> Clear history
    </button>
  </div>

  {#if error}
    <div class="empty history-empty failed">
      <p class="h">History unavailable</p>
      <p class="why">{error}</p>
      <div class="actions"><button class="btn-ghost" disabled={loading} onclick={loadHistory}>{loading ? "Loading…" : "Try again"}</button></div>
    </div>
  {:else if loaded && !filtered.length}
    <div class="empty history-empty">
      <p class="h">{filtering ? "No plays match that" : "Nothing played yet"}</p>
      <p>
        {filtering
          ? "Try another song title or artist."
          : "Your first completed or skipped play will appear here."}
      </p>
      {#if filtering}
        <div class="actions">
          <button class="btn-ghost" type="button" onclick={() => (query = "")}>
            <Icon name="x" size={13} /> Clear filter
          </button>
        </div>
      {/if}
    </div>
  {:else}
    <div class="history-list-head" aria-hidden="true">
      <span class="caps">Track</span>
      <span class="caps">Source</span>
      <span class="caps">Played</span>
      <span class="caps">When</span>
    </div>
    <div
      class="history-list"
      bind:this={bodyEl}
      style="position: relative; overflow-anchor: none"
      style:height="{Math.max(filtered.length, loaded ? 0 : 8) * ROW_H}px"
    >
      <div style="position: absolute; inset: 0 0 auto" style:transform="translateY({firstRow * ROW_H}px)">
        {#if !loaded}
          {#each Array.from({ length: 8 }) as _, i (i)}
            <div class="hi-row" aria-hidden="true">
              <span class="hi-main">
                <span class="skeleton" style="width:40px;height:40px;border-radius:var(--r1)"></span>
                <span class="hi-copy">
                  <span class="skeleton line" style="width:{58 - ((i * 11) % 26)}%;height:11px;margin:0"></span>
                  <span class="skeleton line" style="width:{34 - ((i * 7) % 14)}%;height:9px;margin:0"></span>
                </span>
              </span>
            </div>
          {/each}
        {:else}
          {#each visible as entry, index (firstRow + index)}
            <div class="hi-row">
              <!-- The whole row is the button: one tab stop, replay from
                   anywhere on it, and the accessible name is the play itself. -->
              <button class="hi-main" title={`Play ${entry.track.name}`} onclick={() => replay(entry)}>
                <span class="hi-art">
                  <Cover
                    src={entry.track.cover_url}
                    id={entry.track.album_id || entry.track.uri}
                    name={entry.track.name}
                    size={40}
                  />
                  <span class="hi-go" aria-hidden="true"><Icon name="play" size={13} /></span>
                </span>
                <span class="hi-copy">
                  <strong>{entry.track.name}</strong>
                  <span>{artistOf(entry)}</span>
                </span>
                <span class="hi-context" class:liked={entry.context === "liked"}>
                  {#if contextIcon(entry.context)}<Icon name={contextIcon(entry.context)} size={11} />{/if}
                  <span class="hi-context-text">{contextLabel(entry.context)}</span>
                </span>
                <span class="hi-played">
                  {#if entry.completed}
                    <!-- Foam, the app's "this is done" hue, and the only colour
                         in the row besides a liked heart. -->
                    <span class="hi-complete"><Icon name="check" size={12} />Played</span>
                  {:else}
                    <span
                      class="hi-partial"
                      title={`Skipped — ${formatTime(entry.ms_played)} of ${formatTime(entry.track.duration_ms)} heard`}
                    >
                      <span class="sr-only">Partial play, </span>
                      <span class="tnum">{formatTime(entry.ms_played)}</span>
                      <span class="hi-progress" aria-hidden="true"><i style:width="{playedPct(entry)}%"></i></span>
                    </span>
                  {/if}
                </span>
                <time class="hi-when tnum" datetime={new Date(entry.started_at).toISOString()}>
                  <span class="hi-when-day">{whenDay(entry.started_at)}</span>
                  <span class="hi-when-clock">{whenClock(entry.started_at)}</span>
                </time>
              </button>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</section>

<ConfirmDialog
  open={confirmClear}
  title="Clear listening history?"
  message="This permanently removes every locally recorded play."
  confirmLabel="Clear history"
  busyLabel="Clearing…"
  busy={clearing}
  error={clearError}
  onConfirm={clearAll}
  onCancel={() => (confirmClear = false)}
/>


<style>
  .history-head { padding: var(--s2) 0 var(--s6); }
  .history-head-copy { min-width: 0; }
  .history-head .page-title { margin-top: var(--s2); }
  .history-head p {
    max-width: 520px; margin-top: var(--s2);
    color: var(--fg-2); font-size: var(--t-13); line-height: 1.45;
  }
  .history-clear { height: 32px; margin-left: auto; padding: 0 var(--s3); color: var(--fg-2); }
  .history-clear:hover:not(:disabled) {
    color: var(--love); background: var(--danger-wash);
    border-color: color-mix(in srgb, var(--love) 45%, transparent);
  }
  .history-page {
    max-width: 1020px; margin: 0 auto; padding-top: var(--s5);
    /* The one column template, shared by the head and every row so a label
       always sits over its column: art, track, source, played, when. The
       windowing only asks that a row is 56px tall; the columns may do this. */
    --hi-cols: 40px minmax(220px, 1fr) 116px 92px 96px;
  }
  /* The instrument row sticks under the topbar. At rest it is nothing at
     all — no fill, no rules; the controls float on the page like the
     track-list head's do. When it sticks, only the surface changes: the
     same glass the topbar and tl-head wear, and one hairline underneath to
     hold the rows out of it. Sizes and alignment never move. */
  .history-tools-sentinel { height: 0; pointer-events: none; }
  .history-tools {
    position: sticky; top: var(--topbar-h); z-index: 20;
    display: flex; align-items: center; gap: var(--s4);
    min-height: 48px; padding: var(--s2) 0;
    background: transparent;
    transition: background-color var(--d2) var(--ease), box-shadow var(--d2) var(--ease);
  }
  .history-tools.stuck {
    background: color-mix(in srgb, var(--bg-1) 68%, transparent);
    -webkit-backdrop-filter: blur(14px) saturate(1.7);
            backdrop-filter: blur(14px) saturate(1.7);
    box-shadow: inset 0 -1px 0 var(--line-2);
  }
  @supports not (backdrop-filter: blur(1px)) {
    .history-tools.stuck { background: var(--bg-1); }
  }

  .history-summary {
    display: flex; align-items: center; gap: var(--s2);
    flex: none; min-width: 0; color: var(--fg-3);
  }
  .history-count {
    display: inline-flex; align-items: baseline; gap: 5px; min-width: 0;
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-12);
    white-space: nowrap;
  }
  .history-count strong { color: var(--fg); font-weight: var(--w-semi); font-size: var(--t-13); }
  .history-total { color: var(--fg-3); }

  .history-control { display: flex; align-items: center; min-width: 0; }
  .history-filter-control { flex: 1 1 310px; max-width: 420px; }
  .history-filter {
    display: flex; align-items: center; gap: var(--s2);
    min-width: 0; width: 100%; height: 32px; padding: 0 var(--s3);
    border: 1px solid var(--line-2); border-radius: var(--r2);
    background: var(--bg-2); color: var(--fg-3);
    transition: border-color var(--d1) var(--ease), background-color var(--d1) var(--ease);
  }
  .history-filter:focus-within { border-color: var(--foam); background: var(--bg-3); }
  .history-filter input {
    flex: 1; min-width: 0; height: 100%; padding: 0; border: 0; outline: 0;
    background: transparent; color: var(--fg); font-size: var(--t-12);
    text-overflow: ellipsis;
  }
  .history-filter input::placeholder { color: var(--fg-2); }
  .history-filter input:disabled { opacity: 0.45; }
  .history-filter-clear {
    flex: none; display: grid; place-items: center;
    width: 20px; height: 20px; border-radius: 50%; color: var(--fg-3);
  }
  .history-filter-clear:hover { color: var(--fg); background: var(--hover-3); }
  .history-sort-control { flex: none; }
  .history-sort-control :global(.sel-btn) { height: 32px; min-width: 138px; }

  .history-list-head {
    display: grid; grid-template-columns: var(--hi-cols);
    align-items: center; gap: var(--s4);
    height: 30px; padding: 0 var(--s3);
    color: var(--fg-3);
  }
  /* "Track" covers the art column too; the rest sit over their own. */
  .history-list-head span:first-child { grid-column: 1 / 3; }
  .history-list-head span:last-child { text-align: right; }
  .history-empty {
    max-width: none; min-height: 132px; margin: 0; padding: var(--s6) var(--s3);
  }

  /* One template for every row, because every row is the same height — that
     is what the windowing above depends on. The rows speak the track-list's
     language: borderless rounded hover surfaces, no separators. The sticky
     bar's hairline is the only structural line the list answers to. */
  .hi-row { height: 56px; }
  .hi-main {
    display: grid; grid-template-columns: var(--hi-cols);
    align-items: center; gap: var(--s4);
    width: 100%; height: 56px; padding: 0 var(--s3);
    border-radius: var(--r2); text-align: left;
    transition: background-color var(--d1) var(--ease);
  }
  .hi-main:hover { background: var(--hover); }

  /* The replay affordance lives on the art: on hover (or keyboard focus) the
     sleeve dims under a scrim and the play glyph surfaces. Opacity only —
     nothing moves, so reduced motion has nothing to undo. */
  .hi-art { position: relative; display: block; flex: none; width: 40px; height: 40px; }
  .hi-go {
    position: absolute; inset: 0; z-index: 1;
    display: grid; place-items: center;
    border-radius: var(--r1);
    background: color-mix(in srgb, var(--bg-0) 55%, transparent);
    color: var(--fg);
    opacity: 0;
    transition: opacity var(--d1) var(--ease);
  }
  .hi-main:hover .hi-go, .hi-main:focus-visible .hi-go { opacity: 1; }

  .hi-copy { display: grid; gap: 3px; min-width: 0; }
  .hi-copy strong {
    font-size: var(--t-13); font-weight: var(--w-med); color: var(--fg);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-copy > span {
    font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-main:hover .hi-copy > span { color: var(--fg-1); }

  /* The source of a play, as the app's quiet chip: a hairline pill that
     scans by its glyph before its word is read. Liked is the one warm mark. */
  .hi-context {
    display: inline-flex; align-items: center; gap: 5px;
    justify-self: start; min-width: 0; max-width: 100%;
    height: 20px; padding: 0 var(--s2);
    border: 1px solid var(--line); border-radius: var(--rf);
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
    transition: color var(--d1) var(--ease), border-color var(--d1) var(--ease);
  }
  .hi-context-text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hi-context :global(.icon) { color: var(--fg-3); }
  .hi-context.liked :global(.icon) { color: var(--rose-ink); }
  .hi-main:hover .hi-context { color: var(--fg-1); border-color: var(--line-2); }

  .hi-played { display: flex; align-items: center; min-width: 0; font-size: var(--t-11); color: var(--fg-2); }
  .hi-complete { display: inline-flex; align-items: center; gap: 4px; color: var(--accent); font-weight: var(--w-med); }
  /* A skipped play shows how much of the song it got through as a micro
     seek-rail — the cache-meter's object at row scale — so completed and
     partial separate by shape before either is read. */
  .hi-partial { display: grid; gap: 5px; justify-items: start; }
  .hi-progress {
    width: 44px; height: 3px; border-radius: var(--rf);
    background: var(--bg-4); overflow: hidden;
  }
  .hi-progress i { display: block; height: 100%; border-radius: inherit; background: var(--accent-grad); }

  /* Day over clock, right-aligned: the column reads as a run of days with
     times hanging under them, which is how a journal is scanned. */
  .hi-when { display: grid; gap: 1px; justify-items: end; min-width: 0; text-align: right; }
  .hi-when-day { font-family: var(--font-small); font-size: var(--t-12); font-weight: var(--w-med); color: var(--fg-1); white-space: nowrap; }
  .hi-when-clock { font-size: var(--t-11); color: var(--fg-3); white-space: nowrap; }
  @media (max-width: 760px) {
    .history-page { --hi-cols: 40px minmax(0, 1fr) 92px; }
    .history-tools { flex-wrap: wrap; gap: var(--s3); padding: var(--s3) 0; }
    .history-summary { flex: 1 0 calc(100% - var(--s4)); }
    .history-filter-control { flex: 1 1 220px; max-width: none; }
    .history-list-head span:nth-child(2), .history-list-head span:nth-child(3),
    .hi-context, .hi-played { display: none; }
  }

  @media (max-width: 520px) {
    .history-filter-control { flex-basis: 100%; }
    .history-sort-control, .history-sort-control :global(.sel), .history-sort-control :global(.sel-btn) { width: 100%; }
  }
</style>
