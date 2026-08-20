<script>
  import { playback, session, openAuthUrl, navigate } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
</script>

<!-- Left-aligned under the header with one action, like every other empty
     state. A card centred in the viewport would read as a modal the rest of
     the app never uses. -->
<section class="view page">
  <div style="padding:var(--s7) 0 var(--s2)">
    <span class="login-mark"><Icon name="note" size={20} /></span>
    <span class="tag">Spotify Renderer</span>
    <h1 class="page-title">Sign in to start listening</h1>
  </div>

  <div class="empty">
    <p class="sub">
      Authorisation opens in your browser. Nothing is stored here but the
      session the core hands back.
    </p>

    <div class="actions">
      <button class="btn-accent" disabled={!playback.auth_url} onclick={openAuthUrl}>
        <Icon name="login" size={15} />Log in with Spotify
      </button>
      <button class="link-more" onclick={() => navigate("settings")}>Settings</button>
    </div>

    {#if !playback.auth_url}
      <p class="sub" style="margin-top:var(--s4)">Waiting for a login URL from the core…</p>
    {/if}
    {#if session.error}
      <p class="sub" style="margin-top:var(--s2); color:var(--danger)">{session.error}</p>
    {/if}
  </div>
</section>
