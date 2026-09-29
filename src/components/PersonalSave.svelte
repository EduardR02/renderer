<script>
  import { personal, personalConnected, personalApi, watchPersonal } from "../lib/personal.svelte.js";
  import { navigate, session } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  let { uri, label = "Like", savedLabel = "Unlike", unsavedLabel = "Like", compact = false } = $props();
  let saved = $state(null);
  let busy = $state(false);
  let error = $state("");
  let retry = $state(0);
  let generation = 0;
  $effect(() => watchPersonal());
  $effect(() => {
    const current = uri;
    const account = session.username;
    const ready = personalConnected();
    const revision = personal.membershipRevision;
    const attempt = retry;
    generation++;
    busy = false;
    saved = null;
    error = "";
    if (!ready || !current) return;
    let active = true;
    personalApi.contains([current]).then((result) => {
      if (active) saved = result[0] === true;
    }).catch((reason) => {
      if (active) error = String(reason);
    });
    return () => { active = false; };
  });
  async function toggle() {
    if (!personalConnected()) { navigate("settings"); return; }
    if (busy || saved === null) return;
    const current = uri;
    const account = session.username;
    const requestGeneration = generation;
    busy = true;
    error = "";
    try {
      const next = !saved;
      await personalApi.setSaved([current], next);
      if (requestGeneration === generation && current === uri && account === session.username) saved = next;
    } catch (reason) {
      if (requestGeneration === generation) error = String(reason);
    } finally {
      if (requestGeneration === generation) busy = false;
    }
  }
</script>
<span class="personal-save" class:compact>
  <button class={compact ? "btn-icon" : "btn-ghost"} class:saved={saved === true} onclick={toggle}
    disabled={busy || (personalConnected() && saved === null)}
    aria-label={personalConnected() ? (saved ? savedLabel : unsavedLabel) : "Set up your personal Spotify app in Settings"}
    title={personalConnected() ? (saved ? savedLabel : unsavedLabel) : "Set up your personal Spotify app in Settings"}>
    {#if compact}
      <Icon name="heart-f" size={17} />
    {:else}
      {personalConnected() ? (busy ? "Saving…" : saved === null ? (error ? "Unavailable" : "Checking…") : saved ? savedLabel : unsavedLabel) : `Set up to ${label.toLowerCase()}`}
    {/if}
  </button>
  {#if error}<span class="inline-error" role="alert">{error} <button class="link-more" onclick={() => retry++}>Retry</button></span>{/if}
</span>
<style>
  .personal-save { display:inline-flex; align-items:center; gap:8px; flex-wrap:wrap; }
  .compact { flex:none; }
  .compact button { color:var(--fg-3); opacity:.65; }
  .compact button:hover { opacity:1; }
  .compact button.saved { color:var(--rose-ink); opacity:1; }
</style>
