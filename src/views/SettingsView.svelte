<script>
  import { playback, session, api, openAuthUrl, isLoggedOut } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";

  let statusInfo = $state(null);

  $effect(() => {
    api
      .status()
      .then((s) => {
        statusInfo = s ?? null;
      })
      .catch(() => {
        statusInfo = null;
      });
  });

  function fmt(v) {
    if (v === null || v === undefined) return "—";
    if (typeof v === "object") return JSON.stringify(v);
    return String(v);
  }

  const username = $derived(playback.username || session.username);
</script>

<section class="page settings-page">
  <header class="page-head">
    <h1>Settings</h1>
  </header>

  <div class="section-card">
    <h3>Account</h3>
    {#if isLoggedOut()}
      <p class="sub">
        You're logged out. Log in with your Spotify account to start listening.
      </p>
      <div class="card-actions">
        <button class="btn-accent" disabled={!playback.auth_url} onclick={openAuthUrl}>
          <Icon name="login" size={17} />
          Log in with Spotify
        </button>
      </div>
      {#if !playback.auth_url}
        <p class="sub muted">Waiting for a login URL from the core…</p>
      {/if}
    {:else}
      <div class="account-row">
        <span class="avatar"><Icon name="note" size={18} /></span>
        <div class="account-meta">
          <span class="account-name">{username || "Logged in"}</span>
          <span class="sub">Spotify account</span>
        </div>
      </div>
      <div class="card-actions">
        <button class="btn-ghost" onclick={() => api.logout().catch(() => {})}>
          <Icon name="logout" size={16} />
          Log out
        </button>
      </div>
    {/if}
  </div>

  {#if statusInfo}
    <div class="section-card">
      <h3>Status</h3>
      <div class="kv-list">
        {#each Object.entries(statusInfo) as [k, v]}
          <div class="kv">
            <span class="kv-key">{k.replace(/_/g, " ")}</span>
            <span class="kv-val">{fmt(v)}</span>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  <div class="section-card">
    <h3>About</h3>
    <div class="kv-list">
      <div class="kv">
        <span class="kv-key">App</span>
        <span class="kv-val">Spotify Renderer</span>
      </div>
      <div class="kv">
        <span class="kv-key">Version</span>
        <span class="kv-val">0.1.0</span>
      </div>
      <div class="kv">
        <span class="kv-key">Interface</span>
        <span class="kv-val">Svelte 5 · Tauri 2</span>
      </div>
    </div>
  </div>
</section>

<style>
  .settings-page {
    max-width: 720px;
  }
  .section-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-5);
    margin-bottom: var(--space-5);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .section-card h3 {
    font-size: var(--font-lg);
    font-weight: 700;
    letter-spacing: -0.2px;
  }
  .sub {
    color: var(--text-secondary);
    font-size: var(--font-sm);
  }
  .sub.muted {
    color: var(--text-subdued);
  }
  .card-actions {
    display: flex;
    gap: var(--space-3);
    margin-top: var(--space-1);
  }
  .account-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .avatar {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border-radius: var(--radius-full);
    background: var(--bg-hover);
    color: var(--text-secondary);
    flex: none;
  }
  .account-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .account-name {
    font-size: var(--font-md);
    font-weight: 700;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kv-list {
    display: flex;
    flex-direction: column;
  }
  .kv {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-2) 0;
    font-size: var(--font-md);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }
  .kv:last-child {
    border-bottom: none;
  }
  .kv-key {
    color: var(--text-secondary);
    text-transform: capitalize;
  }
  .kv-val {
    text-align: right;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
