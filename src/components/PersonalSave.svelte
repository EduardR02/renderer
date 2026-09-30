<script>
  import { untrack } from "svelte";
  import { personal, personalConnected, personalApi, personalSaved, watchPersonal } from "../lib/personal.svelte.js";
  import { navigate, session, libraryChanges } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";

  let { uri, label = "Like", savedLabel = "Unlike", unsavedLabel = "Like", compact = false, menu = false, onSaved = null } = $props();
  const saved = $derived(personalSaved(uri));
  const connected = $derived(personalConnected());
  const checking = $derived(connected && saved === undefined);
  let busy = $state(false);
  let error = $state("");
  let retry = $state(0);
  let generation = 0;
  $effect(() => watchPersonal());
  $effect(() => {
    uri;
    session.username;
    connected;
    generation++;
    busy = false;
    error = "";
  });
  $effect(() => {
    const current = uri;
    const ready = connected;
    const revision = personal.membershipRevision;
    const libraryRevision = libraryChanges.savedTracks;
    const attempt = retry;
    if (!ready || !current) return;
    let active = true;
    untrack(() => personalApi.warmMemberships([current])).catch((reason) => {
      if (active) error = String(reason);
    });
    return () => { active = false; };
  });

  async function toggle(event) {
    if (!connected) { onSaved?.(); navigate("settings"); return; }
    if (error && checking) { error = ""; retry++; return; }
    if (busy || checking) return;
    const current = uri;
    const account = session.username;
    const requestGeneration = generation;
    const restoreFocus = document.activeElement === event.currentTarget;
    busy = true;
    error = "";
    try {
      await personalApi.setSaved([current], !saved);
      if (current === uri && account === session.username) onSaved?.({ restoreFocus });
    } catch (reason) {
      if (requestGeneration === generation) error = String(reason);
    } finally {
      if (requestGeneration === generation) busy = false;
    }
  }
  const title = $derived(!connected
    ? "Set up your personal Spotify app in Settings"
    : checking ? (error ? "Retry checking saved status" : "Checking saved status…")
    : saved ? savedLabel : unsavedLabel);
  const text = $derived(!connected ? `Set up to ${label.toLowerCase()}…`
    : busy ? "Saving…" : checking && !menu ? (error ? "Retry" : "Checking…") : title);
</script>

{#snippet control()}
  <button class={menu ? "menu-item personal-save-item" : compact ? "btn-icon" : "btn-ghost"} class:saved={saved === true}
    onclick={toggle} disabled={busy || (checking && !error)}
    aria-label={title} aria-pressed={connected && !menu ? saved === true : undefined}
    aria-busy={busy || (checking && !error)} title={title}>
    {#if compact}
      <Icon name={saved === true ? "check" : "heart"} size={17} />
    {:else}
      {text}
    {/if}
  </button>
{/snippet}

{#if menu}
  {@render control()}
  {#if error}<p class="save-error" role="alert">{error}</p>{/if}
{:else}
  <span class="personal-save" class:compact>
    {@render control()}
    {#if error}<span class="inline-error" role="alert">{error} <button class="link-more" onclick={() => { error = ""; retry++; }}>Retry</button></span>{/if}
  </span>
{/if}

<style>
  .personal-save { display:inline-flex; align-items:center; gap:var(--s2); flex-wrap:wrap; min-width:0; }
  .personal-save .inline-error { overflow-wrap:anywhere; }
  .personal-save:not(.compact) button.btn-ghost {
    justify-content:center; min-width:96px;
    background:linear-gradient(rgba(255,255,255,.035), transparent);
    box-shadow:inset 0 1px 0 rgba(255,255,255,.07);
  }
  .compact { flex:none; }
  /* The player is already glass: a light rim and inner highlight, not another blur layer. */
  .compact button {
    color:var(--fg-1); border:1px solid var(--line-2); background:transparent;
    box-shadow:inset 0 1px 0 rgba(255,255,255,.12);
  }
  .compact button:hover:not(:disabled), .compact button:focus-visible {
    color:var(--rose-ink); border-color:color-mix(in srgb, var(--rose-ink) 48%, var(--line-2));
    background:linear-gradient(rgba(255,255,255,.06), transparent);
  }
  .compact button.saved { color:var(--rose-ink); }
  .save-error { max-width:216px; margin:4px 10px 7px; color:var(--rose-ink); font-size:var(--t-11); line-height:1.35; overflow-wrap:anywhere; }
</style>
