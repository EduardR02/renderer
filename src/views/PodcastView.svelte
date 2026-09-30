<script>
  import { untrack } from "svelte";
  import { api, route, session, navigate, ui } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTime } from "../lib/time.js";
  import { rowWindow } from "../lib/virtual.js";
  let response = $state(null);
  const data = $derived(response?.kind === route.name && response.id === route.id ? response.value : null);
  let error = $state("");
  let busy = $state(false);
  let playing = $state("");
  let queueing = $state("");
  let playError = $state("");
  let retry = $state(0);
  let expanded = $state(false);
  let descriptionBody = $state(null);
  let clipped = $state(false);
  let generation = 0;
  const publishedDate = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
  const tone = $derived(coverTone(data?.cover_url, data?.id || "podcast"));
  const compact = $derived((ui.paneWidth || 1200) < 700);
  const rowHeight = $derived(compact ? 208 : 160);
  const episodes = $derived(route.name === "show" ? data?.episodes ?? [] : []);
  let pageElement = $state(null);
  let listElement = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(12);
  let focusedRow = $state(-1);
  let currentFirst = 0;
  let currentLast = 12;
  const renderedRows = $derived.by(() => {
    const indices = [];
    const focusFirst = Math.max(0, focusedRow - 1);
    const focusLast = Math.min(episodes.length, focusedRow + 2);
    if (focusedRow >= 0) {
      for (let i = focusFirst; i < Math.min(firstRow, focusLast); i++) indices.push(i);
    }
    for (let i = firstRow; i < Math.min(lastRow, episodes.length); i++) indices.push(i);
    if (focusedRow >= 0) {
      for (let i = Math.max(lastRow, focusFirst); i < focusLast; i++) indices.push(i);
    }
    return indices;
  });
  function measureRows(scroller) {
    if (!listElement) return;
    const { first, last } = rowWindow(listElement, scroller, rowHeight, 6, episodes.length);
    if (first === currentFirst && last === currentLast) return;
    currentFirst = first;
    currentLast = last;
    firstRow = first;
    lastRow = last;
  }
  function retainFocus(event) {
    const row = event.target.closest("[data-episode-index]");
    if (row) focusedRow = Number(row.dataset.episodeIndex);
  }
  function releaseFocus(event) {
    if (!listElement?.contains(event.relatedTarget)) focusedRow = -1;
  }
  $effect(() => {
    const list = listElement;
    const page = pageElement;
    if (!list || !page) return;
    const scroller = list.closest(".scroll");
    if (!scroller) return;
    let frame = 0;
    const schedule = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        measureRows(scroller);
      });
    };
    scroller.addEventListener("scroll", schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(scroller);
    observer.observe(page);
    untrack(() => measureRows(scroller));
    return () => {
      scroller.removeEventListener("scroll", schedule);
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  });
  $effect(() => {
    episodes;
    rowHeight;
    const list = listElement;
    const scroller = list?.closest(".scroll");
    if (scroller) untrack(() => measureRows(scroller));
  });
  function publication(value) {
    if (value == null) return "";
    const date = new Date(value);
    return Number.isFinite(date.getTime()) ? publishedDate.format(date) : "";
  }
  function unavailable(episode) {
    return episode.unavailable || !episode.track || episode.track.unavailable;
  }
  $effect(() => {
    const body = descriptionBody;
    data?.description;
    if (!body) return;
    const measure = () => { clipped = expanded || body.scrollHeight - body.clientHeight > 2; };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(body);
    return () => observer.disconnect();
  });
  $effect(() => {
    const kind = route.name;
    const id = route.id;
    const account = session.username;
    const attempt = retry;
    generation++;
    focusedRow = -1;
    response = null;
    error = "";
    playError = "";
    playing = "";
    queueing = "";
    expanded = false;
    busy = false;
    if (!account) {
      error = "Sign in to Spotify to browse podcasts.";
      return;
    }
    let active = true;
    busy = true;
    const load = kind === "show" ? api.browseShow(id) : api.browseEpisode(id);
    load.then((value) => {
      if (!value?.id || (kind === "show" && !Array.isArray(value.episodes))) throw new Error("Spotify returned incomplete podcast details.");
      if (active) response = { kind, id, value };
    })
      .catch((reason) => { if (active) error = String(reason); })
      .finally(() => { if (active) busy = false; });
    return () => { active = false; };
  });
  async function play(episode, episodes = [episode]) {
    if (unavailable(episode) || playing || queueing) return;
    const requestGeneration = generation;
    playing = episode.id;
    playError = "";
    try {
      const available = episodes.filter((item) => !unavailable(item));
      await api.playQueue(available.map((item) => item.track), available.findIndex((item) => item.id === episode.id), `show:${episode.show_id}`);
    } catch (reason) { if (requestGeneration === generation) playError = String(reason); }
    finally { if (requestGeneration === generation) playing = ""; }
  }
  async function enqueue(episode) {
    if (unavailable(episode) || playing || queueing) return;
    const requestGeneration = generation;
    queueing = episode.id;
    playError = "";
    try {
      await api.addQueue(episode.track, `show:${episode.show_id}`);
    } catch (reason) { if (requestGeneration === generation) playError = String(reason); }
    finally { if (requestGeneration === generation) queueing = ""; }
  }
</script>

{#snippet episodeActions(episode, episodes = [episode])}
  <div class="episode-actions">
    <button class="btn-accent" disabled={unavailable(episode) || !!playing || !!queueing} aria-busy={playing === episode.id} onclick={() => play(episode, episodes)} aria-label={`Play audio: ${episode.name}`}>
      <Icon name={playing === episode.id ? "more" : "play"} size={14} />{playing === episode.id ? "Starting…" : "Play audio"}
    </button>
    <button class="btn-ghost" disabled={unavailable(episode) || !!playing || !!queueing} aria-busy={queueing === episode.id} onclick={() => enqueue(episode)} aria-label={`Add to queue: ${episode.name}`}>
      <Icon name="queue" size={14} />{queueing === episode.id ? "Adding…" : "Add to queue"}
    </button>
  </div>
{/snippet}

<section bind:this={pageElement} aria-busy={busy} class="view page wash podcast-page" class:compact style:--episode-row-height={`${rowHeight}px`} style:--tone-wash={tone.wash} style:--tone-wash-deep={tone.washDeep} style:--tone-glow={tone.glow}>
  {#if busy}
    <div class="detail-head" role="status" aria-label="Loading podcast details">
      <span class="skeleton cover-skeleton"></span><div class="podcast-copy"><span class="skeleton line" style="width:64px"></span><span class="skeleton line" style="width:80%;height:36px"></span><span class="skeleton line" style="width:60%"></span></div>
    </div>
  {:else if error}
    <div class="page-head"><h1 class="page-title">{route.name === "show" ? "Podcast" : "Episode"}</h1></div>
    <div class="podcast-state"><h2 class="section-title">Couldn't load {route.name === "show" ? "this podcast" : "this episode"}</h2><p class="inline-error" role="alert">{error}</p><button class="btn-ghost" onclick={() => retry++}>Try again</button></div>
  {:else if data}
    {#if route.name === "episode" && data.show_id}
      <button class="page-back" onclick={() => navigate("show", data.show_id)}><Icon name="back" size={14} />{data.show_name || "Back to podcast"}</button>
    {/if}
    <header class="detail-head">
      <Cover src={data.cover_url || ""} id={data.id} name={data.name} size={compact ? 120 : 184} lg raised />
      <div class="podcast-copy">
        <span class="tag">{route.name === "show" ? "Podcast" : "Episode"}</span>
        <h1 class="detail-title">{data.name}</h1>
        <div class="detail-meta">
          {#if route.name === "show"}
            {#if data.publisher}<span class="who">{data.publisher}</span>{/if}
          {:else}
            {#if publication(data.published_at)}<span>{publication(data.published_at)}</span><span class="sep">·</span>{/if}<span class="num">{formatTime(data.duration_ms)}</span>
          {/if}
        </div>
        {#if route.name === "episode"}
          <div class="actions">{@render episodeActions(data)}</div>
          {#if unavailable(data)}<p class="start-blocked">{data.unavailable_reason || "Audio is unavailable for this episode."}</p>{/if}
        {/if}
      </div>
    </header>
    {#if route.name === "show" && data.description}
      <div class="show-description">
        <p bind:this={descriptionBody} class:expanded>{data.description}</p>
        {#if clipped}<button class="link-more" aria-expanded={expanded} onclick={() => expanded = !expanded}>{expanded ? "Show less" : "Read more"}</button>{/if}
      </div>
    {/if}
    {#if playError}<p class="inline-error playback-error" role="alert">{playError}</p>{/if}
    {#if route.name === "show"}
      <div class="section episode-section">
        <div class="section-head"><h2 class="section-title">Episodes</h2></div>
        {#if !data.episodes.length}
          <div class="podcast-state"><p>No episodes available.</p></div>
        {:else}
          <div class="episode-list" bind:this={listElement} style:height={`${episodes.length * rowHeight}px`} onfocusin={retainFocus} onfocusout={releaseFocus}>
            {#each renderedRows as index (index)}
              {@const episode = episodes[index]}
              {@const date = publication(episode.published_at)}
              <article class="podcast-row" data-episode-index={index} class:unavailable={unavailable(episode)} style:top={`${index * rowHeight}px`}>
                <button class="episode-art" aria-label={`Open ${episode.name}`} onclick={() => navigate("episode", episode.id)}><Cover src={episode.cover_url || data.cover_url || ""} id={episode.id} name={episode.name} size={64} /></button>
                <div class="episode-copy">
                  <button class="episode-title" title={episode.name} onclick={() => navigate("episode", episode.id)}>{episode.name}</button>
                  <div class="episode-meta">{#if date}<span>{date}</span><span class="sep">·</span>{/if}<span>{formatTime(episode.duration_ms)}</span></div>
                  {#if episode.description}<p class="episode-preview">{episode.description}</p>{/if}
                  {#if unavailable(episode)}<p class="episode-unavailable" title={episode.unavailable_reason || "Audio is unavailable for this episode."}>{episode.unavailable_reason || "Audio is unavailable for this episode."}</p>{/if}
                </div>
                {@render episodeActions(episode, data.episodes)}
              </article>
            {/each}
          </div>
        {/if}
      </div>
    {:else if data.description}
      <div class="section episode-description"><div class="section-head"><h2 class="section-title">About this episode</h2></div><p>{data.description}</p></div>
    {/if}
  {/if}
</section>

<style>
  .podcast-copy { min-width: 0; }
  .podcast-page .detail-title { font-size: clamp(30px, 3.8vw, 48px); line-height: 1.08; overflow-wrap:anywhere; }
  .cover-skeleton { width: 184px; height: 184px; border-radius: var(--r3); }
  .show-description { max-width: 760px; margin: var(--s3) 0 var(--s6); }
  .show-description p, .episode-description p { color: var(--fg-2); font-size: var(--t-13); line-height: 1.7; overflow-wrap: anywhere; }
  .show-description p { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; margin: 0 0 var(--s2); }
  .show-description p.expanded { display: block; white-space: pre-wrap; }
  .episode-section { margin-top: var(--s5); }
  .episode-list { position: relative; }
  .podcast-row { position: absolute; left: 0; right: 0; height: var(--episode-row-height); box-sizing: border-box; display: grid; grid-template-columns: 64px minmax(0, 1fr) auto; align-items: center; gap: var(--s4); padding: var(--s4) var(--s3); border-top: 1px solid var(--line); border-radius: var(--r2); transition: background var(--d1) var(--ease); }
  .podcast-row:hover, .podcast-row:focus-within { background: var(--hover); }
  .episode-art { border-radius: var(--r1); align-self: start; }
  .episode-copy { min-width: 0; align-self: start; }
  .episode-title { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow: hidden; text-align: left; color: var(--fg); font-size: var(--t-13); font-weight: var(--w-semi); line-height: 1.45; overflow-wrap: anywhere; }
  .episode-title:hover { color: var(--accent); }
  .episode-meta { display: flex; gap: var(--s2); white-space: nowrap; margin-top: var(--s2); color: var(--fg-3); font-family: var(--font-small); font-size: var(--t-11); font-variant-numeric: tabular-nums; }
  .episode-preview { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow: hidden; color: var(--fg-2); font-size: var(--t-12); line-height: 1.65; overflow-wrap: anywhere; margin: var(--s2) 0 0; }
  .episode-actions { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s2); }
  .episode-unavailable { margin: var(--s2) 0 0; color: var(--fg-2); font-size: var(--t-11); line-height: 1.5; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .unavailable .episode-preview { -webkit-line-clamp: 1; line-clamp: 1; }
  .unavailable .episode-art { opacity: .65; }
  .episode-description { max-width: 760px; }
  .episode-description p { white-space: pre-wrap; }
  .podcast-state { padding: var(--s5) 0; color: var(--fg-2); }
  .podcast-state p { margin: 0 0 var(--s4); line-height: 1.6; overflow-wrap: anywhere; }
  .podcast-state .section-title { margin-bottom: var(--s3); }
  .playback-error { margin: var(--s4) 0; overflow-wrap: anywhere; }
  .compact .detail-head { gap: var(--s4); align-items: center; }
  .compact .detail-title { font-size: 30px; }
  .compact .cover-skeleton { width: 120px; height: 120px; }
  .compact .podcast-row { grid-template-columns: 64px minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) 34px; gap: var(--s3) var(--s4); align-items: start; }
  .compact .podcast-row .episode-actions { grid-column: 1 / -1; flex-wrap: nowrap; }
  .compact .episode-actions button { padding-inline:var(--s3); }
</style>
