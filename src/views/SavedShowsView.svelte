<script>
  import { untrack } from "svelte";
  import { navigate, session, focusSearch } from "../lib/state.svelte.js";
  import { personalApi, personalConnected, watchPersonal } from "../lib/personal.svelte.js";
  import Cover from "../components/Cover.svelte";
  let shows = $state([]);
  let offset = $state(0);
  let total = $state(null);
  let loading = $state(false);
  let error = $state("");
  let generation = 0;
  $effect(() => watchPersonal());
  $effect(() => {
    const account = session.username;
    const granted = personalConnected();
    generation++;
    shows = [];
    offset = 0;
    total = null;
    loading = false;
    error = "";
    if (granted) untrack(() => loadMore());
  });
  async function loadMore() {
    if (!personalConnected() || loading) return;
    const current = generation;
    loading = true;
    error = "";
    try {
      const page = await personalApi.savedShows(offset, 50);
      if (current !== generation) return;
      shows = [...shows, ...page.items.map((item) => item.show)];
      offset = page.offset + page.items.length;
      total = page.total;
    } catch (reason) { if (current === generation) error = String(reason); }
    finally { if (current === generation) loading = false; }
  }
</script>
<section class="view page">
  <h1 class="page-title">Podcasts</h1>
  <p>Find audio podcasts and episodes in Search, or paste a Spotify show or episode link.</p>
  <button class="btn-accent" onclick={focusSearch}>Search podcasts</button>
  {#if !personalConnected()}
    <div class="section"><h2 class="section-title">Saved podcasts</h2>
      <p>To view saved shows, authorize your own Spotify developer app. Podcast search and playback do not require one.</p>
      <button class="btn-ghost" onclick={() => navigate("settings")}>Set up in Settings</button>
    </div>
  {:else}
    <div class="section"><h2 class="section-title">Saved podcasts</h2>
    {#if error}<p class="inline-error" role="alert">{error} <button class="link-more" onclick={loadMore}>Try again</button></p>{/if}
    {#if loading && !shows.length}<p role="status">Loading saved podcasts…</p>{/if}
    {#if total === 0}<p>No saved podcasts.</p>{/if}
    <div class="saved-list">
      {#each shows as show (show.id)}
        <button class="saved-show" onclick={() => navigate("show", show.id)}>
          <Cover src={show.images?.[0]?.url} id={show.id} name={show.name} size={64} />
          <span><strong>{show.name}</strong><small>{show.publisher} · {show.total_episodes} episodes</small></span>
        </button>
      {/each}
    </div>
    {#if total !== null && offset < total}
      <button class="btn-ghost" disabled={loading} onclick={loadMore}>{loading ? "Loading…" : "Load more"}</button>
    {/if}
    </div>
  {/if}
</section>
<style>
  .saved-list { display:grid; gap:8px; margin:24px 0; }
  .saved-show { display:flex; align-items:center; gap:16px; padding:12px; width:100%; text-align:left; border:0; background:transparent; color:inherit; cursor:pointer; }
  .saved-show span { display:grid; gap:6px; min-width:0; }
  .saved-show small { color:var(--fg-2); }
</style>
