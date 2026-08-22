<script>
  import { untrack } from "svelte";
  import { api } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import { formatTime } from "../lib/time.js";

  let entries = $state([]);
  let nextOffset = $state(null);
  let loading = $state(true);
  let loadingMore = $state(false);
  let error = $state("");
  let confirmClear = $state(false);
  let clearing = $state(false);
  let clearError = $state("");

  const dateFormatter = new Intl.DateTimeFormat(undefined, { weekday: "long", month: "long", day: "numeric" });
  const timeFormatter = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });

  function dayKey(timestamp) {
    const date = new Date(timestamp);
    return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
  }

  const groups = $derived.by(() => {
    const result = [];
    for (const entry of entries) {
      const key = dayKey(entry.started_at);
      let group = result[result.length - 1];
      if (!group || group.key !== key) {
        group = { key, label: dateFormatter.format(new Date(entry.started_at)), entries: [] };
        result.push(group);
      }
      group.entries.push(entry);
    }
    return result;
  });

  function contextLabel(context) {
    if (!context) return "Unknown source";
    if (context === "liked") return "Liked Songs";
    if (context === "search") return "Search";
    const [kind] = context.split(":", 1);
    return ({ playlist: "Playlist", album: "Album", artist: "Artist", radio: "Radio" })[kind] || "Queue";
  }

  async function load(reset = false) {
    if (reset) {
      loading = true;
      error = "";
    } else {
      loadingMore = true;
    }
    try {
      const offset = reset ? 0 : nextOffset ?? entries.length;
      const page = await api.getHistory(offset, 40);
      entries = reset ? (page?.entries ?? []) : [...entries, ...(page?.entries ?? [])];
      nextOffset = page?.next_offset ?? null;
    } catch (reason) {
      error = String(reason || "Could not load listening history.");
    } finally {
      loading = false;
      loadingMore = false;
    }
  }

  async function clearAll() {
    if (clearing) return;
    clearing = true;
    clearError = "";
    try {
      await api.clearHistory();
      entries = [];
      nextOffset = null;
      confirmClear = false;
    } catch (reason) {
      clearError = String(reason || "Could not clear listening history.");
    } finally {
      clearing = false;
    }
  }

  function replay(entry) {
    if (entry.track) api.playQueue([entry.track], 0, "history").catch(() => {});
  }

  $effect(() => untrack(() => load(true)));
</script>

<section class="view page history-page">
  <header class="history-head">
    <div><span class="tag">Local</span><h1 class="page-title">Listening history</h1><p>One entry per play, stored only on this computer.</p></div>
    {#if entries.length}<button class="btn-ghost danger" onclick={() => (confirmClear = true)}>Clear history</button>{/if}
  </header>

  {#if loading}
    <div class="history-loading" aria-label="Loading history">
      {#each Array(8) as _}<span class="skeleton history-skeleton"></span>{/each}
    </div>
  {:else if error}
    <div class="empty-state"><h2>History unavailable</h2><p>{error}</p><button class="btn-accent" onclick={() => load(true)}>Try again</button></div>
  {:else if entries.length === 0}
    <div class="empty-state"><Icon name="clock" size={28} /><h2>Nothing played yet</h2><p>Your first completed or skipped play will appear here.</p></div>
  {:else}
    <div class="history-groups">
      {#each groups as group (group.key)}
        <section class="history-group">
          <h2>{group.label}</h2>
          <div class="history-list">
            {#each group.entries as entry (`${entry.started_at}-${entry.track_id}`)}
              <article class="history-row">
                <button class="history-play" disabled={!entry.track} title={entry.track ? `Play ${entry.track.name}` : "Track metadata unavailable"} onclick={() => replay(entry)}>
                  {#if entry.track}<Cover src={entry.track.cover_url} id={entry.track.album_id || entry.track.uri} name={entry.track.name} size={44} />{:else}<span class="history-missing"><Icon name="note" size={15} /></span>{/if}
                  <span class="history-copy"><strong>{entry.track?.name || entry.track_id}</strong><span>{entry.track ? (entry.track.artist_names ?? []).join(", ") : "Metadata expired"}</span></span>
                </button>
                <span class="history-context">{contextLabel(entry.context)}</span>
                <span class="history-progress"><strong>{entry.completed ? "Completed" : formatTime(entry.ms_played)}</strong><small>{entry.completed ? formatTime(entry.track?.duration_ms ?? entry.ms_played) : "played"}</small></span>
                <time class="tnum" datetime={new Date(entry.started_at).toISOString()}>{timeFormatter.format(new Date(entry.started_at))}</time>
              </article>
            {/each}
          </div>
        </section>
      {/each}
    </div>
    {#if nextOffset !== null}<button class="history-more btn-ghost" disabled={loadingMore} onclick={() => load(false)}>{loadingMore ? "Loading…" : "Load older plays"}</button>{/if}
  {/if}
</section>

<ConfirmDialog open={confirmClear} title="Clear listening history?" message="This permanently removes every locally recorded play." confirmLabel="Clear history" busyLabel="Clearing…" busy={clearing} error={clearError} onConfirm={clearAll} onCancel={() => (confirmClear = false)} />

<style>
  .history-page{max-width:1020px;margin:0 auto}.history-head{display:flex;align-items:flex-end;justify-content:space-between;gap:24px;margin-bottom:34px}.history-head h1{margin:8px 0}.history-head p{color:var(--fg-2);margin:0}.danger{color:var(--rose-ink)}.history-groups{display:grid;gap:30px}.history-group h2{font-size:var(--t-12);text-transform:uppercase;letter-spacing:.09em;color:var(--fg-2);margin:0 0 10px}.history-list{border-top:1px solid var(--line-2)}.history-row{display:grid;grid-template-columns:minmax(260px,1fr) 110px 100px 82px;gap:18px;align-items:center;min-height:62px;border-bottom:1px solid var(--line-2)}.history-play{display:flex;align-items:center;min-width:0;gap:12px;text-align:left}.history-play:disabled{opacity:1}.history-copy{display:grid;min-width:0;gap:3px}.history-copy strong,.history-copy span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.history-copy span,.history-context,.history-progress small,.history-row time{color:var(--fg-2);font-size:var(--t-12)}.history-progress{display:grid;gap:3px}.history-progress strong{font-size:var(--t-12);font-weight:var(--w-med)}.history-row time{text-align:right}.history-missing{display:grid;place-items:center;width:44px;height:44px;border-radius:var(--r1);background:var(--bg-3);color:var(--fg-3);flex:none}.history-more{display:block;margin:28px auto 0}.history-loading{display:grid;gap:8px}.history-skeleton{display:block;height:62px;border-radius:var(--r1)}@media(max-width:720px){.history-row{grid-template-columns:minmax(190px,1fr) 84px}.history-context,.history-progress{display:none}.history-head{align-items:flex-start;flex-direction:column}}
</style>
