<script>
  import { playback, openAuthUrl, navigate } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
</script>

<div class="login">
  <div class="login-card">
    <span class="login-logo"><Icon name="note" size={36} /></span>
    <h1>Spotify Renderer</h1>
    <p class="sub">Log in with your Spotify account to listen to your music.</p>
    <button class="btn-accent" disabled={!playback.auth_url} onclick={openAuthUrl}>
      <Icon name="login" size={17} />
      Log in with Spotify
    </button>
    {#if !playback.auth_url}
      <p class="sub muted">Waiting for a login URL from the core…</p>
    {/if}
    <button class="link" onclick={() => navigate("settings")}>More options</button>
  </div>
</div>

<style>
  .login {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100%;
    padding: var(--space-6);
  }
  .login-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-7) var(--space-6);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5);
    text-align: center;
    max-width: 420px;
    width: 100%;
  }
  .login-logo {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 72px;
    height: 72px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: #000;
    margin-bottom: var(--space-2);
  }
  .login-card h1 {
    font-size: var(--font-2xl);
    font-weight: 700;
    letter-spacing: -0.5px;
  }
  .sub {
    color: var(--text-secondary);
    font-size: var(--font-sm);
    margin-bottom: var(--space-2);
  }
  .sub.muted {
    color: var(--text-subdued);
  }
  .link {
    margin-top: var(--space-2);
    font-size: var(--font-sm);
    font-weight: 700;
    color: var(--text-secondary);
    transition: color var(--transition-fast);
  }
  .link:hover {
    color: var(--text-primary);
  }
</style>
