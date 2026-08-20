<script>
  import {
    playback,
    session,
    stats,
    cacheStats,
    refreshCacheStats,
    clearCache,
    api,
    openAuthUrl,
    isLoggedOut,
  } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import Select from "../components/Select.svelte";
  import { formatBytes } from "../lib/time.js";

  const username = $derived(playback.username || session.username);


  /* `status` is a fire-and-forget ping: the engine answers on the event
     channel, not in the response, so all this can report is reachability. */
  let ping = $state("idle");
  let clearTarget = $state(null);
  let clearing = $state(false);
  let clearError = $state("");
  let appSettings = $state(null);
  let settingsBusy = $state(false);
  let settingsError = $state("");

  const CACHE_LIMITS = [
    { value: 1024, label: "1 GiB" },
    { value: 2048, label: "2 GiB" },
    { value: 4096, label: "4 GiB" },
    { value: 8192, label: "8 GiB" },
    { value: 0, label: "Unlimited" },
  ];

  const clearCopy = $derived(
    clearTarget === "audio"
      ? {
          title: "Clear audio cache?",
          message: "Playback will stop and the current queue will be cleared. Downloaded audio will be fetched again when needed.",
        }
      : {
          title: "Clear cover cache?",
          message: "Cached artwork will be removed and downloaded again as pages are opened.",
        },
  );

  function requestClear(kind) {
    clearTarget = kind;
    clearError = "";
  }

  async function confirmClear() {
    if (!clearTarget || clearing) return;
    clearing = true;
    clearError = "";
    try {
      await clearCache(clearTarget);
      clearTarget = null;
    } catch (error) {
      clearError = String(error || "Could not clear the cache.");
    } finally {
      clearing = false;
    }
  }

  function checkEngine() {
    ping = "checking";
    api
      .status()
      .then(() => (ping = "ok"))
      .catch(() => (ping = "failed"));
  }

  /* The cache limit is the one number in Settings with a live counterpart on
     disk, so the row shows the two against each other rather than as two
     unrelated facts three rows apart. Unlimited has nothing to fill, so it
     draws no meter at all. */
  const cacheLimitMb = $derived(appSettings?.audio_cache_limit_mb ?? 0);
  const cacheFill = $derived(
    cacheLimitMb > 0 && cacheStats.audio
      ? Math.min(1, cacheStats.audio.bytes / (cacheLimitMb * 1024 * 1024))
      : null,
  );

  async function updateAudioCacheLimit(mb) {
    if (!Number.isFinite(mb) || settingsBusy) return;
    settingsBusy = true;
    settingsError = "";
    try {
      appSettings = await api.setAudioCacheLimit(mb);
    } catch (error) {
      settingsError = String(error || "Could not save the cache limit.");
    } finally {
      settingsBusy = false;
    }
  }

  const pingLabel = $derived(
    ping === "ok" ? "Reachable" : ping === "failed" ? "No answer" : ping === "checking" ? "Checking…" : "Not checked"
  );

  // The command is cached server-side for a minute and this effect runs once
  // per Settings mount, so reopening the page stays fresh without a disk walk
  // on every render.
  $effect(() => {
    refreshCacheStats().catch(() => {});
    api.getAppSettings()
      .then((value) => (appSettings = value))
      .catch((error) => (settingsError = String(error || "Could not load app settings.")));
  });
</script>

<section class="view page">
  <div class="settings-intro">
    <h1 class="page-title">Settings</h1>
  </div>

  <!-- Hairline-separated rows rather than boxed cards: [label + helper] on the
       left, the control on the right, one 660px column. -->
  <div class="set">
    <div class="set-group">
      <h2>Account</h2>
      {#if isLoggedOut()}
        <div class="set-row">
          <div>
            <div class="k">Not signed in</div>
            <div class="d">
              {playback.auth_url
                ? "Opens Spotify in your browser to authorise this device."
                : "Waiting for a login URL from the core…"}
            </div>
          </div>
          <div class="set-ctl">
            <button class="btn-accent" disabled={!playback.auth_url} onclick={openAuthUrl}>
              <Icon name="login" size={15} />Log in
            </button>
          </div>
        </div>
      {:else}
        <div class="set-row">
          <div>
            <div class="k">{username || "Signed in"}</div>
            <div class="d">Spotify account</div>
          </div>
          <div class="set-ctl">
            <button class="btn-ghost" onclick={() => api.logout().catch(() => {})}>
              <Icon name="logout" size={14} />Log out
            </button>
          </div>
        </div>
      {/if}
      {#if session.error}
        <div class="set-row">
          <div>
            <div class="k">Last session error</div>
            <div class="d">{session.error}</div>
          </div>
          <div class="set-ctl"><span class="dot warn"></span></div>
        </div>
      {/if}
    </div>


    <div class="set-group" data-cache-stats>
      <h2>Storage</h2>
      <div class="set-row" data-cache-stat="audio">
        <div>
          <div class="k">Audio cache</div>
          <div class="d">Downloaded audio retained for offline replay.</div>
        </div>
        <div class="set-ctl">
          <span class="v" aria-live="polite">
            {#if cacheStats.audio}
              <span class="tnum">{cacheStats.audio.files}</span> files
              <span class="v-sep" aria-hidden="true"></span>
              <strong class="tnum">{formatBytes(cacheStats.audio.bytes)}</strong>
            {:else if cacheStats.loading}
              Measuring…
            {:else}
              Unavailable
            {/if}
          </span>
          <button class="btn-ghost danger" onclick={() => requestClear("audio")}>Clear</button>
        </div>
        {#if cacheFill !== null}
          <div class="set-meter" style:--p={cacheFill}>
            <span class="set-meter-rail"><span class="set-meter-fill"></span></span>
            <span class="set-meter-note">
              <span class="tnum">{Math.round(cacheFill * 100)}%</span> of the
              {CACHE_LIMITS.find((o) => o.value === cacheLimitMb)?.label ?? ""} limit
            </span>
          </div>
        {/if}
      </div>
      <div class="set-row" data-cache-limit>
        <div>
          <div class="k">Audio cache limit</div>
          <div class="d">Maximum downloaded audio kept on disk. Applies after the next restart.</div>
          {#if settingsError}<div class="inline-error">{settingsError}</div>{/if}
        </div>
        <div class="set-ctl">
          <Select
            label="Audio cache limit"
            options={CACHE_LIMITS}
            value={cacheLimitMb}
            disabled={!appSettings || settingsBusy}
            onchange={updateAudioCacheLimit}
          />
        </div>
      </div>
      <div class="set-row" data-cache-stat="covers">
        <div>
          <div class="k">Cover cache</div>
          <div class="d">Artwork fetched once by the core and served from disk afterwards.</div>
        </div>
        <div class="set-ctl">
          <span class="v" aria-live="polite">
            {#if cacheStats.covers}
              <span class="tnum">{cacheStats.covers.files}</span> files
              <span class="v-sep" aria-hidden="true"></span>
              <strong class="tnum">{formatBytes(cacheStats.covers.bytes)}</strong>
            {:else if cacheStats.loading}
              Measuring…
            {:else}
              Unavailable
            {/if}
          </span>
          <button class="btn-ghost danger" onclick={() => requestClear("covers")}>Clear</button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Refresh cache stats</div>
          <div class="d">{cacheStats.error ?? `${stats.coversResolved} cover requests this session`}</div>
        </div>
        <div class="set-ctl">
          <button class="btn-ghost" onclick={() => refreshCacheStats().catch(() => {})} disabled={cacheStats.loading}>
            {cacheStats.loading ? "Measuring…" : "Refresh"}
          </button>
        </div>
      </div>
    </div>

    <div class="set-group">
      <h2>Diagnostics</h2>
      <div class="set-row">
        <div>
          <div class="k">Engine</div>
          <div class="d">{playback.ready ? "Connected and ready." : "Not reported ready yet."}</div>
        </div>
        <div class="set-ctl">
          <span class="v status" class:ok={ping === "ok"} class:bad={ping === "failed"}>
            <span class="status-dot" aria-hidden="true"></span>{pingLabel}
          </span>
          <button class="btn-ghost" onclick={checkEngine} disabled={ping === "checking"}>Check</button>
        </div>
      </div>
      <div class="set-row">
        <div class="k">Auth state</div>
        <div class="set-ctl">
          <span class="v status" class:ok={playback.auth_state === "authenticated"}>
            <span class="status-dot" aria-hidden="true"></span>{playback.auth_state ?? "unknown"}
          </span>
        </div>
      </div>
      <div class="set-row">
        <div class="k">Version</div>
        <div class="set-ctl"><span class="v">0.1.0 / Svelte 5 / Tauri 2</span></div>
      </div>
    </div>
  </div>
</section>

{#if clearTarget}
  <ConfirmDialog
    open
    title={clearCopy.title}
    message={clearCopy.message}
    confirmLabel="Clear cache"
    busyLabel="Clearing…"
    busy={clearing}
    error={clearError}
    onConfirm={confirmClear}
    onCancel={() => {
      clearTarget = null;
      clearError = "";
    }}
  />
{/if}
