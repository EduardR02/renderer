<script>
  import { playback, session, openAuthUrl, navigate } from "../lib/state.svelte.js";
  import { paletteFor } from "../lib/covertone.svelte.js";
  import Icon from "../components/Icon.svelte";

  /* Every other page in the app takes its colour from a record. This one has
     no record — there is nothing loaded yet, that is the whole point of it —
     so it takes the palette's own cool instead. Without it this is the single
     screen in the app that is a flat grey rectangle, and it is the first one
     anybody sees. `soft`, at a third strength: a sign-in prompt is not a 56px
     album title. */
  const TONE = paletteFor(196, 0.075);
</script>

<!-- Left-aligned under the header with one action, like every other empty
     state. A card centred in the viewport would read as a modal the rest of
     the app never uses. -->
<section
  class="view page wash soft"
  style:--tone-wash={TONE.wash}
  style:--tone-wash-deep={TONE.washDeep}
  style:--tone-glow={TONE.glow}
>
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
      <p class="sub" style="margin-top:var(--s4)">Waiting for Spotify sign-in to be ready…</p>
    {/if}
    {#if session.error}
      <p class="sub" style="margin-top:var(--s2); color:var(--danger)">{session.error}</p>
    {/if}
  </div>
</section>
