<script>
  import { untrack } from "svelte";
  import { navigate, session, focusSearch } from "../lib/state.svelte.js";
  import { personalApi, personalConnected, watchPersonal } from "../lib/personal.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
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

<section class="view page saved-shows-page">
  <header class="page-head">
    <h1 class="page-title">Saved podcasts</h1>
    <p class="sub">Shows saved to your Spotify library.</p>
  </header>
  {#if !personalConnected()}
    <div class="saved-state">
      <h2 class="section-title">Connect your Spotify library</h2>
      <p>Authorize your personal Spotify app in Settings to see saved podcasts. You can still find podcasts and episodes in Search.</p>
      <div class="actions"><button class="btn-ghost" onclick={() => navigate("settings")}>Open Settings</button><button class="btn-ghost" onclick={focusSearch}><Icon name="search" size={14} />Search podcasts</button></div>
    </div>
  {:else}
    {#if error}
      <div class="saved-state"><h2 class="section-title">Couldn't load saved podcasts</h2><p class="inline-error" role="alert">{error}</p><button class="btn-ghost" onclick={loadMore}>Try again</button></div>
    {/if}
    {#if loading && !shows.length}
      <div class="grid" role="status" aria-label="Loading saved podcasts">
        {#each [0, 1, 2, 3] as item (item)}<div class="card" aria-hidden="true"><span class="skeleton card-skeleton"></span><span class="skeleton line" style="width:70%;margin-top:14px"></span></div>{/each}
      </div>
    {:else if total === 0}
      <div class="saved-state"><h2 class="section-title">No saved podcasts yet</h2><p>Find a show or episode in Search.</p><button class="btn-ghost" onclick={focusSearch}><Icon name="search" size={14} />Search podcasts</button></div>
    {/if}
    {#if shows.length}
      <div class="grid">
        {#each shows as show (show.id)}
          <div class="card">
            <div class="card-art"><Cover src={show.images?.[0]?.url || ""} id={show.id} name={show.name} fill lg /><button class="card-open" aria-label={`Open ${show.name}`} onclick={() => navigate("show", show.id)}></button></div>
            <button class="card-copy" onclick={() => navigate("show", show.id)}><span class="card-name">{show.name}</span><span class="card-sub">{show.publisher || "Podcast"}</span></button>
          </div>
        {/each}
      </div>
    {/if}
    {#if total !== null && offset < total}
      <div class="load-more"><button class="btn-ghost" disabled={loading} onclick={loadMore}>{loading ? "Loading…" : "Load more"}</button></div>
    {/if}
  {/if}
</section>

<style>
  .saved-state { padding: var(--s5) 0; max-width: 560px; }
  .saved-state .section-title { margin-bottom: var(--s3); }
  .saved-state p { margin: 0 0 var(--s4); color: var(--fg-2); font-size: var(--t-13); line-height: 1.7; overflow-wrap: anywhere; }
  .saved-state .actions { margin-top: var(--s4); }
  .load-more { margin-top: var(--s6); }
  .card-skeleton { display: block; width: 100%; aspect-ratio: 1; border-radius: var(--r3); }
</style>
