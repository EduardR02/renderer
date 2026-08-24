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

  const dateFormatter = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" });
  const yearFormatter = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });
  const timeFormatter = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
  const countFormatter = new Intl.NumberFormat();
  const collator = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

  function startOfDay(date) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  }

  function formatWhen(timestamp) {
    const date = new Date(timestamp);
    const today = startOfDay(new Date());
    const day = startOfDay(date);
    const time = timeFormatter.format(date);
    if (day === today) return time;
    if (day === today - 86_400_000) return `Yest. ${time}`;
    const now = new Date();
    if (date.getFullYear() === now.getFullYear()) return `${dateFormatter.format(date)} ${time}`;
    return `${yearFormatter.format(date)} ${time}`;
  }

  function contextLabel(context) {
    if (!context) return "Unknown source";
    if (context === "liked") return "Liked Songs";
    if (context === "search") return "Search";
    if (context === "history") return "History";
    const [kind] = context.split(":", 1);
    return ({ playlist: "Playlist", album: "Album", artist: "Artist", radio: "Radio" })[kind] || "Queue";
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
    requestAnimationFrame(() => measure(scroller));
  }

  $effect(() => {
    query;
    sort;
    untrack(resetWindow);
  });

  $effect(() => {
    filtered.length;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) return;
    let queued = false;
    const onScroll = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const observer = new ResizeObserver(() => measure(scroller));
    observer.observe(scroller);
    measure(scroller);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      observer.disconnect();
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
  <div class="history-tools" class:stuck={toolsStuck}>
    <div class="history-summary" aria-live="polite">
      <span class="history-summary-icon"><Icon name="clock" size={13} /></span>
      <span class="history-summary-copy">
        {#if loaded && !error}
          <span class="history-summary-kicker">{filtering ? "Showing" : "Recorded"}</span>
          <span class="history-summary-value">
            <strong class="tnum">{countFormatter.format(filtered.length)}</strong>
            {filtered.length === 1 ? "play" : "plays"}
            {#if filtering}<span class="history-summary-total">of <span class="tnum">{countFormatter.format(history.length)}</span></span>{/if}
          </span>
        {:else if error}
          <span class="history-summary-kicker">History</span>
          <span class="history-summary-value">Unavailable</span>
        {:else}
          <span class="history-summary-kicker">History</span>
          <span class="history-summary-value">Loading…</span>
        {/if}
      </span>
    </div>
    <span class="history-tools-divider" aria-hidden="true"></span>
    <div class="history-control history-filter-control">
      <span class="history-control-label"><Icon name="search" size={12} />Filter</span>
      <span class="history-filter">
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
      <span class="history-control-label"><Icon name="clock" size={12} />Sort</span>
      <Select
        label="Sort listening history"
        options={SORTS}
        value={sort}
        disabled={!loaded || !!error}
        onchange={(value) => (sort = value)}
      />
    </div>
    <span class="history-tools-divider" aria-hidden="true"></span>
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
      <span>Track</span>
      <span>Source</span>
      <span>Played</span>
      <span>When</span>
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
              <button
                class="hi-main"
                title={`Play ${entry.track.name}`}
                onclick={() => replay(entry)}
              >
                <Cover
                  src={entry.track.cover_url}
                  id={entry.track.album_id || entry.track.uri}
                  name={entry.track.name}
                  size={40}
                />
                <span class="hi-copy">
                  <strong>{entry.track.name}</strong>
                  <span>{artistOf(entry)}</span>
                </span>
              </button>
              <span class="hi-context">{contextLabel(entry.context)}</span>
              <span class="hi-played">
                {#if entry.completed}
                  <span class="hi-complete"><Icon name="check" size={12} />Played</span>
                {:else}
                  <span class="tnum">{formatTime(entry.ms_played)}</span>
                {/if}
              </span>
              <time class="hi-when tnum" datetime={new Date(entry.started_at).toISOString()}>
                {formatWhen(entry.started_at)}
              </time>
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
  .history-page { max-width: 1020px; margin: 0 auto; padding-top: var(--s5); }
  .history-head { padding: var(--s2) 0 var(--s6); }
  .history-head-copy { min-width: 0; }
  .history-head .page-title { margin-top: var(--s2); }
  .history-head p {
    max-width: 520px; margin-top: var(--s2);
    color: var(--fg-2); font-size: var(--t-13); line-height: 1.45;
  }
  .history-clear { height: 34px; margin-left: auto; padding: 0 var(--s3); color: var(--fg-2); }
  .history-clear:hover:not(:disabled) {
    color: var(--love); background: var(--danger-wash);
    border-color: color-mix(in srgb, var(--love) 45%, transparent);
  }

  /* The metadata and both controls are one instrument panel. When it sticks,
     only the surface changes; sizes and alignment stay fixed. */
  .history-tools-sentinel { height: 0; pointer-events: none; }
  .history-tools {
    position: sticky; top: var(--topbar-h); z-index: 20;
    display: flex; align-items: center; gap: var(--s4);
    min-height: 58px; padding: var(--s2) 0;
    background: transparent;
    box-shadow: inset 0 1px 0 var(--line), inset 0 -1px 0 var(--line);
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
    display: flex; align-items: center; gap: var(--s3);
    flex: 0 0 146px; min-width: 0;
  }
  .history-summary-icon {
    display: grid; place-items: center; flex: none;
    width: 28px; height: 28px; border-radius: var(--rf);
    background: var(--accent-wash); color: var(--accent);
  }
  .history-summary-copy { display: grid; gap: 1px; min-width: 0; }
  .history-summary-kicker, .history-control-label {
    display: inline-flex; align-items: center; gap: 5px;
    color: var(--fg-3); font-family: var(--font-small);
    font-size: 9px; font-weight: var(--w-semi); letter-spacing: 0.11em; text-transform: uppercase;
  }
  .history-summary-value {
    color: var(--fg-2); font-family: var(--font-small); font-size: var(--t-11);
    white-space: nowrap;
  }
  .history-summary-value strong { color: var(--fg); font-weight: var(--w-semi); }
  .history-summary-total { color: var(--fg-3); }
  .history-tools-divider { align-self: stretch; width: 1px; margin: var(--s1) 0; background: var(--line); }

  .history-control { display: grid; align-content: center; gap: 4px; min-width: 0; }
  .history-control-label { height: 12px; }
  .history-filter-control { flex: 1 1 310px; max-width: 420px; }
  .history-filter {
    display: flex; align-items: center;
    min-width: 0; height: 34px; padding: 0 var(--s2) 0 var(--s3);
    border: 1px solid var(--line-2); border-radius: var(--r2);
    background: var(--bg-2); color: var(--fg-3);
    transition: border-color var(--d1) var(--ease), background-color var(--d1) var(--ease);
  }
  .history-filter:focus-within {
    border-color: color-mix(in srgb, var(--accent) 58%, transparent);
    background: var(--bg-3);
  }
  .history-filter input {
    flex: 1; min-width: 0; height: 100%; padding: 0; border: 0; outline: 0;
    background: transparent; color: var(--fg); font-size: var(--t-12);
  }
  .history-filter input::placeholder { color: var(--fg-3); }
  .history-filter input:disabled { opacity: 0.45; }
  .history-filter-clear {
    flex: none; display: grid; place-items: center;
    width: 20px; height: 20px; border-radius: 50%; color: var(--fg-3);
  }
  .history-filter-clear:hover { color: var(--fg); background: rgba(255, 255, 255, 0.08); }
  .history-sort-control { flex: none; }
  .history-sort-control :global(.sel-btn) { height: 34px; min-width: 146px; background: var(--bg-2); }

  .history-list-head {
    display: grid; grid-template-columns: minmax(220px, 1fr) 104px 92px 96px;
    align-items: center; gap: var(--s4);
    height: 30px; padding: 0 var(--s2);
    border-bottom: 1px solid var(--line);
    color: var(--fg-3); font-family: var(--font-small);
    font-size: 9px; font-weight: var(--w-semi); letter-spacing: 0.11em; text-transform: uppercase;
  }
  .history-list-head span:last-child { text-align: right; }
  .history-empty {
    max-width: none; min-height: 132px; margin: 0; padding: var(--s6) var(--s2);
  }

  /* One template for every row, because every row is the same height — that
     is what the windowing above depends on. */
  .hi-row {
    display: grid;
    grid-template-columns: minmax(220px, 1fr) 104px 92px 96px;
    align-items: center; gap: var(--s4);
    height: 56px; padding: 0 var(--s2);
    border-radius: var(--r1);
    box-shadow: inset 0 -1px 0 var(--line-2);
  }
  .hi-row:hover { background: rgba(255, 255, 255, 0.035); }
  .hi-main {
    display: flex; align-items: center; gap: var(--s3);
    min-width: 0; height: 100%; text-align: left;
  }
  .hi-main:disabled { opacity: 1; }
  .hi-copy { display: grid; gap: 3px; min-width: 0; }
  .hi-copy strong {
    font-size: var(--t-13); font-weight: var(--w-med); color: var(--fg);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-copy > span {
    font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-row:hover .hi-copy strong { color: var(--fg); }
  .hi-context, .hi-when {
    font-size: var(--t-11); color: var(--fg-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .hi-played { font-size: var(--t-11); color: var(--fg-2); }
  /* Foam, the app's "this is done" hue, and the only colour in the row. */
  .hi-complete { display: inline-flex; align-items: center; gap: 4px; color: var(--accent); }
  .hi-when { text-align: right; }

  @media (max-width: 760px) {
    .history-tools { flex-wrap: wrap; gap: var(--s3); padding: var(--s3) 0; }
    .history-summary { flex: 1 0 calc(100% - var(--s4)); }
    .history-tools-divider { display: none; }
    .history-filter-control { flex: 1 1 220px; max-width: none; }
    .history-list-head, .hi-row { grid-template-columns: minmax(0, 1fr) 92px; }
    .history-list-head span:nth-child(2), .history-list-head span:nth-child(3),
    .hi-context, .hi-played { display: none; }
  }

  @media (max-width: 520px) {
    .history-filter-control { flex-basis: 100%; }
    .history-sort-control, .history-sort-control :global(.sel), .history-sort-control :global(.sel-btn) { width: 100%; }
  }
</style>
