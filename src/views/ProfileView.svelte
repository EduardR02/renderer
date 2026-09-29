<script>
  import { api, route, session, navigate } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import PersonalSave from "../components/PersonalSave.svelte";
  let profile = $state(null);
  let error = $state("");
  let loading = $state(false);
  let generation = $state(0);
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
<section class="view page">
  {#if loading}<p role="status">Loading profile…</p>{/if}
  {#if error}<p class="inline-error" role="alert">{error} <button class="link-more" onclick={() => generation++}>Try again</button></p>{/if}
  {#if profile}
    <div class="profile-head"><Cover src={profile.image_url} id={profile.username} name={profile.name || profile.username} size={120} circle />
      <div><p>Profile</p><h1 class="page-title">{profile.name || profile.username}</h1>
        <p>{profile.username}</p>
        {#if profile.username !== session.username}
          <PersonalSave uri={`spotify:user:${profile.username}`} label="Follow" savedLabel="Unfollow" unsavedLabel="Follow" />
        {/if}
      </div></div>
    <div class="section"><h2 class="section-title">Public playlists</h2>
      {#if !profile.playlists.length}<p>No public playlists available.</p>{/if}
      <div class="profile-list">{#each profile.playlists as playlist (playlist.id)}
        <button class="profile-row" onclick={() => navigate("playlist", playlist.id)}>
          <Cover src={playlist.cover_url} id={playlist.id} name={playlist.name} size={48} />
          <span>{playlist.name}</span>
        </button>
      {/each}</div>
    </div>
  {/if}
</section>
<style>
  .profile-head { display:flex; align-items:center; gap:24px; margin:24px 0; }
  .profile-list { display:grid; gap:8px; }
  .profile-row { display:flex; align-items:center; gap:12px; padding:8px; background:transparent; border:0; color:inherit; cursor:pointer; text-align:left; }
</style>
