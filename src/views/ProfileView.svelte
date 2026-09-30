<script>
  import { untrack } from "svelte";
  import { api, route, session, navigate, ui, setPageTitle } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";
  import { rememberAvatar } from "../lib/avatar.svelte.js";
  import Cover from "../components/Cover.svelte";
  import FollowPill from "../components/FollowPill.svelte";
  import HeaderMenu from "../components/HeaderMenu.svelte";
  import CopyLinkItem from "../components/CopyLinkItem.svelte";

  /**
   * A listener's page, in the header every record has: their picture where a
   * cover goes (round, because it is a person), the name, what they share,
   * and — for someone else, with the personal app — the Follow pill.
   */
  let profile = $state(null);
  let error = $state("");
  let loading = $state(false);
  let retry = $state(0);
  const username = $derived(route.id || session.username || "");
  const mine = $derived(!!profile && profile.username === session.username);
  const name = $derived(profile?.name?.trim() || profile?.username || "");
  const tone = $derived(coverTone(profile?.image_url, profile?.username || "profile"));
  const artSize = $derived(detailArtSize(ui.paneWidth));

  $effect(() => {
    const user = username;
    const account = session.username;
    const attempt = retry;
    profile = null;
    error = "";
    loading = false;
    if (!account || !user) {
      error = "Sign in to Spotify to browse profiles.";
      return;
    }
    let active = true;
    loading = true;
    api.browseProfile(user).then((result) => {
      if (!result?.username || !Array.isArray(result.playlists)) throw new Error("Spotify returned an incomplete profile.");
      if (active) profile = result;
    })
      .catch((reason) => { if (active) error = String(reason); })
      .finally(() => { if (active) loading = false; });
    return () => { active = false; };
  });

  $effect(() => {
    if (!profile) return;
    untrack(() => {
      setPageTitle(name);
      /* Your own picture is what the rail's account row draws; it learns it
         here, the one place it is on screen anyway. */
      if (profile.username === session.username) rememberAvatar(session.username, profile.image_url || "");
    });
  });
</script>

<section
  class="view page wash profile-page"
  aria-busy={loading}
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if loading}
    <header class="detail-head" role="status" aria-label="Loading profile">
      <span class="skeleton" style:width="{artSize}px" style:height="{artSize}px" style="border-radius:var(--rf)"></span>
      <div>
        <span class="skeleton line sm" style="width:64px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(360px,64%)"></span>
        <span class="skeleton line sm" style="width:120px"></span>
      </div>
    </header>
    <div class="grid section" aria-hidden="true">
      {#each Array.from({ length: 5 }) as _, i (i)}
        <div class="card">
          <span class="skeleton tile"></span>
          <span class="card-copy"><span class="skeleton line" style="width:70%;height:12px;margin:0"></span></span>
        </div>
      {/each}
    </div>
  {:else if error}
    <header class="detail-head">
      <span class="skeleton" style:width="{artSize}px" style:height="{artSize}px" style="border-radius:var(--rf)"></span>
      <div>
        <span class="tag">Profile</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This profile could not be loaded.</p>
      <p class="why">{error}</p>
      <div class="actions">
        <button class="pill" onclick={() => retry++}>Try again</button>
        <button class="link-more" onclick={() => navigate("library")}>Back to your library</button>
      </div>
    </div>
  {:else if profile}
    <header class="detail-head">
      <Cover src={profile.image_url || ""} id={profile.username} name={name} size={artSize} circle raised />
      <div>
        <span class="tag">Profile</span>
        <h1 class="detail-title" class:long={name.length > 44}>{name}</h1>
        <p class="detail-meta">
          <span class="num">{profile.playlists.length} public {profile.playlists.length === 1 ? "playlist" : "playlists"}</span>
        </p>
        <div class="actions">
          {#if !mine}<FollowPill username={profile.username} />{/if}
          <HeaderMenu label="Profile actions">
            {#snippet children(close)}
              <CopyLinkItem link={`https://open.spotify.com/user/${encodeURIComponent(profile.username)}`} {close} />
            {/snippet}
          </HeaderMenu>
        </div>
      </div>
    </header>

    <section class="section">
      <div class="section-head"><h2 class="section-title">Public playlists</h2></div>
      {#if !profile.playlists.length}
        <div class="empty">
          <p class="h">No public playlists.</p>
          <p class="sub">{mine ? "Playlists you make public on Spotify appear here." : "Nothing shared here yet."}</p>
        </div>
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
                <span class="card-sub">{playlist.tracks_total ? `${playlist.tracks_total} songs` : "Playlist"}</span>
              </button>
            </div>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</section>
