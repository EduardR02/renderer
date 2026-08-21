<script>
  import {
    detail,
    navigate,
    loadCataloguePage,
  } from "../lib/state.svelte.js";
  import { playAlbumById } from "../lib/play.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";

  const artist = $derived(detail.artist);
  const counts = $derived(artist?.release_counts ?? {});
  const appearsOnCount = $derived(counts.appears_on ?? 0);
  const RELEASE_TYPES = ["appears_on"];
  const PAGE_SIZE = 7;

  let releases = $state([]);
  let nextOffset = $state(0);
  let total = $state(0);
  let loading = $state(false);
  let error = $state("");
  let sentinel = $state(null);
  let loadedArtist = "";
  let armed = false;
  let busy = $state("");
  let playError = $state("");

  async function loadNext(key = artist?.id ?? "") {
    const id = artist?.id;
    if (!id || id !== key || loading || nextOffset == null || !appearsOnCount) return;
    const offset = nextOffset;
    loading = true;
    error = "";
    try {
      const page = await loadCataloguePage(id, RELEASE_TYPES, offset, PAGE_SIZE);
      if (artist?.id !== key) return;
      const known = new Set(releases.map((release) => release.id));
      for (const release of page?.releases ?? []) {
        if (!release?.id || known.has(release.id)) continue;
        releases.push(release);
        known.add(release.id);
      }
      total = page?.total ?? appearsOnCount;
      nextOffset = page?.next_offset ?? null;
      armed = false;
    } catch (reason) {
      error = String(reason || "Could not load Appears On.");
    } finally {
      if (artist?.id === key) loading = false;
    }
  }

  $effect(() => {
    const id = artist?.id ?? "";
    if (!id || id === loadedArtist) return;
    loadedArtist = id;
    releases = [];
    nextOffset = 0;
    total = appearsOnCount;
    error = "";
    armed = false;
    queueMicrotask(() => loadNext(id));
  });

  /* One bounded page opens the view. Further pages wait for a real scroll so a
     short viewport never chains through the complete catalogue on mount. */
  $effect(() => {
    const node = sentinel;
    if (!node) return;
    const scroller = node.closest(".scroll");
    if (!scroller) return;
    const onScroll = () => (armed = true);
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting && armed) loadNext();
      },
      { root: scroller, rootMargin: "0px 0px 400px 0px" },
    );
    scroller.addEventListener("scroll", onScroll, { passive: true });
    observer.observe(node);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  });

  async function playRelease(id) {
    if (busy) return;
    busy = id;
    playError = "";
    try {
      await playAlbumById(id);
    } catch (reason) {
      playError = String(reason || "Could not play this release.");
    } finally {
      busy = "";
    }
  }
</script>

<section class="view page aux-page appears-page">
  {#if artist}
    <header class="aux-head">
      <button class="aux-back" onclick={() => navigate("artist", artist.id)}>
        <Icon name="back" size={13} />{artist.name}
      </button>
      <div class="aux-title-row">
        <h1 class="page-title">Appears On</h1>
        <span class="section-count tnum">{appearsOnCount}</span>
      </div>
    </header>

    {#if releases.length}
      <div class="grid appears-grid">
        {#each releases as release (release.id)}
          {@const tone = coverTone(release.cover_url, release.id)}
          <div class="card" style:--tone-glow={tone.glow}>
            <div class="card-art">
              <Cover src={release.cover_url} id={release.id} name={release.name} fill lg />
              <button
                class="card-open"
                aria-label={`Open ${release.name}`}
                onclick={() => navigate("album", release.id)}
              ></button>
              <button
                class="card-play"
                aria-label={`Play ${release.name}`}
                title={`Play ${release.name}`}
                disabled={!!busy}
                onclick={() => playRelease(release.id)}
              >
                <Icon name={busy === release.id ? "more" : "play"} size={15} />
              </button>
            </div>
            <button class="card-copy" onclick={() => navigate("album", release.id)}>
              <span class="card-name">{release.name}</span>
              <span class="card-sub">{[release.year || null, "Appears on"].filter(Boolean).join(" · ")}</span>
            </button>
          </div>
        {/each}
      </div>
    {:else if loading}
      <div class="grid appears-grid" aria-hidden="true">
        {#each Array.from({ length: 3 }) as _, i (i)}
          <div class="card">
            <span class="skeleton" style="display:block;aspect-ratio:1;width:100%;border-radius:var(--r3)"></span>
            <span class="card-copy">
              <span class="skeleton line" style="width:72%;height:12px;margin:0"></span>
              <span class="skeleton line sm" style="height:10px;margin:6px 0 0"></span>
            </span>
          </div>
        {/each}
      </div>
    {:else if !error}
      <div class="empty">
        <p class="h">No Appears On releases are available.</p>
        <p class="sub">Spotify did not return release summaries for this artist.</p>
      </div>
    {/if}

    <div class="aux-foot" bind:this={sentinel}>
      {#if loading && releases.length}
        <span class="aux-status">Loading releases…</span>
      {:else if nextOffset != null && releases.length}
        <button class="btn-ghost" onclick={() => loadNext()}>Load more<Icon name="chevron-down" size={14} /></button>
      {:else if releases.length}
        <span class="aux-status"><span class="tnum">{releases.length}</span>{total && total !== releases.length ? ` of ${total}` : ""} releases</span>
      {/if}
    </div>
    {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
    {#if playError}<p class="inline-error" role="alert">{playError}</p>{/if}
  {:else if detail.error}
    <header class="aux-head">
      <div class="aux-title-row"><h1 class="page-title">Appears On</h1></div>
    </header>
    <div class="empty failed">
      <p class="h">This artist could not be loaded.</p>
      <p class="why">{detail.error}</p>
    </div>
  {:else}
    <header class="aux-head" aria-hidden="true">
      <span class="skeleton line sm"></span>
      <span class="skeleton line lg" style="height:40px;width:min(340px,60%)"></span>
    </header>
  {/if}
</section>

<style>
  .aux-page { padding-top: var(--s5); }
  .aux-head { padding: var(--s2) 0 var(--s6); }
  .aux-back {
    display: inline-flex; align-items: center; gap: var(--s2);
    color: var(--fg-2); font-size: var(--t-12);
  }
  .aux-back:hover { color: var(--fg); }
  .aux-title-row { display: flex; align-items: baseline; gap: var(--s3); margin-top: var(--s3); }
  .appears-grid { grid-template-columns: repeat(auto-fill, minmax(158px, 1fr)); }
  .aux-foot { display: flex; justify-content: center; min-height: 56px; padding-top: var(--s6); }
  .aux-status { color: var(--fg-2); font-size: var(--t-12); }
</style>
