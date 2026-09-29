<script>
  import { api, route, session, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import { formatTime } from "../lib/time.js";
  let data = $state(null);
  let error = $state("");
  let busy = $state(false);
  let playing = $state("");
  let queueing = $state("");
  let playError = $state("");
  let retry = $state(0);
  const publishedDate = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
  function publication(value) {
    if (value == null) return "";
    const date = new Date(value);
    return Number.isFinite(date.getTime()) ? publishedDate.format(date) : "";
  }
  $effect(() => {
    const kind = route.name;
    const id = route.id;
    const account = session.username;
    const attempt = retry;
    data = null;
    error = "";
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
      if (active) data = value;
    })
      .catch((reason) => { if (active) error = String(reason); })
      .finally(() => { if (active) busy = false; });
    return () => { active = false; };
  });
  async function play(episode, episodes = [episode]) {
    if (episode.unavailable || playing || queueing) return;
    playing = episode.id;
    playError = "";
    try {
      const available = episodes.filter((item) => !item.unavailable && item.track);
      await api.playQueue(available.map((item) => item.track), available.findIndex((item) => item.id === episode.id), `show:${episode.show_id}`);
    } catch (reason) { playError = String(reason); }
    finally { playing = ""; }
  }
  async function enqueue(episode) {
    if (episode.unavailable || playing || queueing) return;
    queueing = episode.id;
    playError = "";
    try {
      await api.addQueue(episode.track, `show:${episode.show_id}`);
    } catch (reason) { playError = String(reason); }
    finally { queueing = ""; }
  }
  const episodes = $derived(route.name === "show" ? data?.episodes ?? [] : data ? [data] : []);
</script>
<section class="view page">
  {#if route.name === "show" && data}
    <div class="podcast-head"><Cover src={data.cover_url} id={data.id} name={data.name} size={160} />
      <div><p>Podcast · {data.publisher}</p><h1 class="page-title">{data.name}</h1><p>{data.description}</p></div></div>
  {:else if route.name === "episode" && data}
    <div class="podcast-head"><Cover src={data.cover_url} id={data.id} name={data.name} size={160} />
      <div><p>Episode · <button class="link-more" onclick={() => navigate("show", data.show_id)}>{data.show_name}</button></p>
        <h1 class="page-title">{data.name}</h1></div></div>
  {/if}
  {#if error}<p class="inline-error" role="alert">{error} <button class="link-more" onclick={() => retry++}>Try again</button></p>{/if}
  {#if busy && !data}<p role="status">Loading podcasts…</p>{/if}
  {#if episodes.length}
    <div class="section"><h2 class="section-title">{route.name === "show" ? "Episodes" : "Listen"}</h2>
      {#each episodes as episode (episode.id)}
        <div class="podcast-row"><Cover src={episode.cover_url} id={episode.id} name={episode.name} size={64} />
          <div class="episode-copy"><button class="link-more" onclick={() => navigate("episode", episode.id)}>{episode.name}</button>
            <small>{#if publication(episode.published_at)}{publication(episode.published_at)} · {/if}{formatTime(episode.duration_ms)}</small>
            {#if route.name === "show"}<p class="episode-preview">{episode.description}</p>{/if}
            {#if episode.unavailable}<small>{episode.unavailable_reason || "Audio is unavailable for this episode."}</small>{/if}</div>
          <div class="episode-actions">
            <button class="btn-ghost" disabled={episode.unavailable || !!playing || !!queueing} onclick={() => play(episode, episodes)}>{playing === episode.id ? "Starting…" : "Play audio"}</button>
            <button class="btn-ghost" disabled={episode.unavailable || !!playing || !!queueing} onclick={() => enqueue(episode)}>{queueing === episode.id ? "Adding…" : "Add to queue"}</button>
          </div>
        </div>
      {/each}
    </div>
    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
  {:else if data && route.name === "show"}<p>No audio episodes available.</p>{/if}
  {#if route.name === "episode" && data?.description}
    <div class="section episode-description"><h2 class="section-title">About this episode</h2><p>{data.description}</p></div>
  {/if}
</section>
<style>
  .podcast-head { display:flex; gap:24px; align-items:flex-start; margin:24px 0; }
  .podcast-head > div { min-width:0; }
  .podcast-head p,.episode-copy p,.episode-description p { color:var(--fg-2); line-height:1.5; overflow-wrap:anywhere; }
  .podcast-row { display:grid; grid-template-columns:64px minmax(0,1fr); align-items:start; gap:12px 16px; padding:12px; width:100%; text-align:left; }
  .episode-copy { display:grid; gap:6px; min-width:0; }
  .episode-preview { display:-webkit-box; -webkit-box-orient:vertical; -webkit-line-clamp:3; line-clamp:3; overflow:hidden; margin:0; }
  .episode-copy > button { text-align:left; overflow-wrap:anywhere; }
  .podcast-row small { color:var(--text-muted); }
  .episode-actions { grid-column:2; display:flex; flex-wrap:wrap; justify-content:flex-start; gap:8px; }
  .episode-description p { white-space:pre-wrap; }
</style>
