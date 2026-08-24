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
    <div>
      <span class="tag">Local</span>
      <h1 class="page-title">Listening history</h1>
      <p>Your latest plays, stored only on this computer.</p>
    </div>
    <button class="btn-ghost danger" disabled={!loaded || loading || !history.length} onclick={() => (confirmClear = true)}>
      Clear history
    </button>
  </header>

  <div class="history-tools-sentinel" bind:this={toolsSentinel} aria-hidden="true"></div>
  <div class="history-tools" class:stuck={toolsStuck}>
    <div class="history-filter">
      <Icon name="search" size={13} />
      <input
        bind:value={query}
        aria-label="Filter listening history"
        placeholder="Filter by song or artist"
        spellcheck="false"
        disabled={!loaded || !!error}
        onkeydown={(event) => event.key === "Escape" && (query = "")}
      />
      {#if query}
        <button class="history-filter-clear" title="Clear filter" onclick={() => (query = "")}>
          <Icon name="x" size={11} />
        </button>
      {/if}
    </div>
    <span class="history-count">
      {#if loaded && !error}
        <span class="tnum">{countFormatter.format(filtered.length)}</span>
        {filtered.length === 1 ? "play" : "plays"}
        {#if filtering}<span> of {countFormatter.format(history.length)}</span>{/if}
      {/if}
    </span>
    <Select
      label="Sort listening history"
      options={SORTS}
      value={sort}
      disabled={!loaded || !!error}
      onchange={(value) => (sort = value)}
    />
  </div>

  {#if error}
    <div class="empty failed">
      <p class="h">History unavailable</p>
      <p class="why">{error}</p>
      <div class="actions"><button class="btn-ghost" disabled={loading} onclick={loadHistory}>{loading ? "Loading…" : "Try again"}</button></div>
    </div>
  {:else if loaded && !filtered.length}
    <div class="empty">
      <p class="h">{filtering ? "No plays match that" : "Nothing played yet"}</p>
      <p>
        {filtering
          ? "Try another song title or artist."
          : "Your first completed or skipped play will appear here."}
      </p>
    </div>
  {:else}
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
  .history-page { max-width: 1020px; margin: 0 auto; }
  .history-head {
    display: flex; align-items: flex-end; justify-content: space-between; gap: var(--s6);
    margin-bottom: var(--s5);
  }
  .history-head p { margin: 0; color: var(--fg-2); font-size: var(--t-13); }
  .danger { color: var(--rose-ink); }

  /* Sticky under the topbar, and — like every other sticky bar here — type on
     the page until rows start passing beneath it. */
  .history-tools-sentinel { height: 0; pointer-events: none; }
  .history-tools {
    position: sticky; top: var(--topbar-h); z-index: 20;
    display: flex; align-items: center; gap: var(--s3);
    height: 46px; margin-bottom: var(--s1);
    background: transparent;
    box-shadow: inset 0 -1px 0 var(--line);
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

  .history-filter {
    display: flex; align-items: center; gap: var(--s2);
    flex: 0 1 300px; min-width: 0; height: 30px; padding: 0 var(--s2);
    border: 1px solid var(--line); border-radius: var(--r2);
    color: var(--fg-3);
  }
  .history-filter:focus-within { border-color: color-mix(in srgb, var(--accent) 58%, transparent); }
  .history-filter input {
    flex: 1; min-width: 0; color: var(--fg); font-size: var(--t-12);
  }
  .history-filter input::placeholder { color: var(--fg-3); }
  .history-filter-clear {
    flex: none; display: grid; place-items: center;
    width: 18px; height: 18px; border-radius: 50%; color: var(--fg-3);
  }
  .history-filter-clear:hover { color: var(--fg); background: rgba(255, 255, 255, 0.08); }
  .history-count {
    margin-right: auto; color: var(--fg-2);
    font-family: var(--font-small); font-size: var(--t-11);
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
    .history-head { flex-direction: column; align-items: flex-start; gap: var(--s3); }
    .hi-row { grid-template-columns: minmax(0, 1fr) 92px; }
    .hi-context, .hi-played { display: none; }
  }
</style>
