<script>
  import { api, route, session, navigate, ui } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Cover from "../components/Cover.svelte";
  import PersonalSave from "../components/PersonalSave.svelte";
  let profile = $state(null);
  let error = $state("");
  let loading = $state(false);
  let generation = $state(0);
  const name = $derived(profile?.name?.trim() || profile?.username || "");
  const tone = $derived(coverTone(profile?.image_url, profile?.username || "profile"));
  const compact = $derived((ui.paneWidth || 1200) < 560);
  $effect(() => {
    const username = route.id || session.username;
    const account = session.username;
    const retry = generation;
    profile = null;
    error = "";
    loading = false;
    if (!account || !username) {
      error = "Sign in to Spotify to browse profiles.";
      return;
    }
    let active = true;
    loading = true;
    api.browseProfile(username).then((result) => {
      if (!result?.username || !Array.isArray(result.playlists)) throw new Error("Spotify returned an incomplete profile.");
      if (active) profile = result;
    })
      .catch((reason) => { if (active) error = String(reason); })
      .finally(() => { if (active) loading = false; });
    return () => { active = false; };
  });
</script>

<section class="view page wash soft profile-page" aria-busy={loading} class:compact style:--tone-wash={tone.wash} style:--tone-wash-deep={tone.washDeep} style:--tone-glow={tone.glow}>
  {#if loading}
    <div class="detail-head" role="status" aria-label="Loading profile">
      <span class="skeleton avatar-skeleton"></span>
      <div class="profile-copy"><span class="skeleton line" style="width:64px"></span><span class="skeleton line" style="width:60%;height:36px"></span></div>
    </div>
    <div class="grid" aria-hidden="true">
      {#each [0, 1, 2, 3] as item (item)}
        <div class="card"><span class="skeleton card-skeleton"></span><span class="skeleton line" style="width:70%;margin-top:14px"></span></div>
      {/each}
    </div>
  {:else if error}
    <div class="page-head"><h1 class="page-title">Profile</h1></div>
    <div class="profile-state"><h2 class="section-title">Couldn't load this profile</h2><p class="inline-error" role="alert">{error}</p><button class="btn-ghost" onclick={() => generation++}>Try again</button></div>
  {:else if profile}
    <header class="detail-head">
      <Cover src={profile.image_url || ""} id={profile.username} name={name} size={compact ? 96 : 160} circle raised />
      <div class="profile-copy">
        <span class="tag">Profile</span>
        <h1 class="detail-title">{name}</h1>
        {#if profile.username !== session.username}
          <div class="actions"><PersonalSave uri={`spotify:user:${profile.username}`} label="Follow" savedLabel="Unfollow" unsavedLabel="Follow" /></div>
        {/if}
      </div>
    </header>
    <div class="section profile-playlists">
      <div class="section-head"><h2 class="section-title">Public playlists</h2></div>
      {#if !profile.playlists.length}
        <div class="profile-state"><p>No public playlists to show.</p></div>
      {:else}
        <div class="grid">
          {#each profile.playlists as playlist (playlist.id)}
            <div class="card">
              <div class="card-art">
                <Cover src={playlist.cover_url || ""} srcs={playlist.cover_urls ?? []} id={playlist.id} name={playlist.name} fill lg />
                <button class="card-open" aria-label={`Open ${playlist.name}`} onclick={() => navigate("playlist", playlist.id)}></button>
              </div>
              <button class="card-copy" onclick={() => navigate("playlist", playlist.id)}>
                <span class="card-name">{playlist.name}</span>
                <span class="card-sub">Playlist</span>
              </button>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .profile-copy { min-width: 0; }
  .detail-title { overflow-wrap:anywhere; }
  .profile-playlists { margin-top: var(--s5); }
  .profile-state { padding: var(--s5) 0; color: var(--fg-2); }
  .profile-state p { margin: 0 0 var(--s4); line-height: 1.6; overflow-wrap: anywhere; }
  .profile-state .section-title { margin-bottom: var(--s3); }
  .avatar-skeleton { width: 160px; height: 160px; border-radius: var(--rf); }
  .card-skeleton { display: block; width: 100%; aspect-ratio: 1; border-radius: var(--r3); }
  .compact .detail-head { gap: var(--s4); align-items: center; }
  .compact .detail-title { font-size: 32px; }
  .compact .avatar-skeleton { width: 96px; height: 96px; }
</style>
