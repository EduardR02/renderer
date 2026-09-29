<script>
  import { untrack } from "svelte";
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
  import UpdateControl from "../components/UpdateControl.svelte";
  import {
    personal, personalConnected, watchPersonal, configurePersonal, authorizePersonal,
    disconnectPersonal, setDevicesEnabled,
  } from "../lib/personal.svelte.js";
  let clientId = $state("");
  let personalBusy = $state(false);
  let personalError = $state("");
  $effect(() => watchPersonal());
  $effect(() => {
    const stored = personal.status?.client_id;
    if (stored !== undefined) untrack(() => { clientId = stored; });
  });
  async function personalAction(action) {
    if (personalBusy) return;
    personalBusy = true;
    personalError = "";
    try { await action(); }
    catch (error) { personalError = String(error); }
    finally { personalBusy = false; }
  }

  const username = $derived(playback.username || session.username);


  /* `status` waits for the engine's response (or a timeout). It checks
     reachability now; auth and playback readiness are reported separately. */
  let ping = $state("idle");
  let clearTarget = $state(null);
  let clearing = $state(false);
  let clearError = $state("");
  let appSettings = $state(null);
  let settingBusy = $state("");
  const settingErrors = $state({
    load: "",
    audioCacheLimit: "",
    normalisation: "",
    launchAtLogin: "",
    startMinimized: "",
    animatedCanvas: "",
  });

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

  async function updateSetting(key, update, fallbackMessage) {
    if (!appSettings || settingBusy) return;
    settingBusy = key;
    settingErrors[key] = "";
    try {
      appSettings = await update();
    } catch (error) {
      settingErrors[key] = String(error || fallbackMessage);
    } finally {
      if (settingBusy === key) settingBusy = "";
    }
  }

  function updateAudioCacheLimit(mb) {
    if (!Number.isFinite(mb)) return;
    return updateSetting(
      "audioCacheLimit",
      () => api.setAudioCacheLimit(mb),
      "Could not save the cache limit.",
    );
  }

  function updateLaunchAtLogin(enabled) {
    return updateSetting(
      "launchAtLogin",
      () => api.setLaunchAtLogin(enabled),
      "Could not update launch at login.",
    );
  }

  function updateStartMinimized(enabled) {
    return updateSetting(
      "startMinimized",
      () => api.setStartMinimized(enabled),
      "Could not update start minimized.",
    );
  }

  function updateAnimatedCanvas(enabled) {
    return updateSetting(
      "animatedCanvas",
      () => api.setAnimatedCanvas(enabled),
      "Could not update animated Canvas.",
    );
  }

  function updateNormalisation(enabled) {
    return updateSetting(
      "normalisation",
      () => api.setNormalisation(enabled),
      "Could not update volume normalization.",
    );
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
      .then((value) => {
        appSettings = value;
        settingErrors.load = "";
      })
      .catch((error) => {
        settingErrors.load = String(error || "Could not load app settings.");
      });
  });
</script>

<section class="view page">
  <div class="settings-intro">
    <h1 class="page-title">Settings</h1>
    {#if settingErrors.load}<p class="inline-error" role="alert">{settingErrors.load}</p>{/if}
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
                : "Waiting for Spotify sign-in to be ready…"}
            </div>
            {#if session.error}<div class="inline-error" role="alert">{session.error}</div>{/if}
          </div>
          <div class="set-ctl">
            <button
              class="btn-accent"
              disabled={!playback.auth_url || session.authPending}
              onclick={openAuthUrl}
            >
              <Icon name="login" size={15} />{session.authPending ? "Opening…" : "Log in"}
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

    <div class="set-group">
      <h2>Personal Spotify app</h2>
      <div class="set-row">
        <div>
          <div class="k">Developer Client ID</div>
          <div class="d">Optional. Create your own app in Spotify's developer dashboard and register <code>http://127.0.0.1:5589/personal-api/callback</code> as its redirect URI. Your Client ID stays on this computer; no shared app or token is used in the webview. Without a grant, likes and follows remain read-only.</div>
          {#if personalError || personal.error}<div class="inline-error" role="alert">{personalError || personal.error}</div>{/if}
        </div>
        <div class="set-ctl">
          <input class="client-id-input" aria-label="Spotify developer Client ID" bind:value={clientId} placeholder="Client ID" autocomplete="off" spellcheck="false" />
          <button class="btn-ghost" disabled={personalBusy || !clientId.trim()} onclick={() => personalAction(() => configurePersonal(clientId))}>Save</button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Personal authorization</div>
          <div class="d">{personal.loading ? "Checking…" : personal.status?.connected && !personalConnected() ? `Personal grant belongs to ${personal.status.account_id}. Disconnect and authorize this account.` : personal.status?.connected ? `Connected as ${personal.status.account_id}` : personal.status?.authorization_pending ? "Waiting for authorization in your browser…" : personal.status?.client_id ? "Not authorized. Your Spotify playback remains available without this grant." : "Enter your own Client ID to enable library changes."}</div>
        </div>
        <div class="set-ctl">
          {#if personal.status?.connected}
            <button class="btn-ghost" disabled={personalBusy} onclick={() => personalAction(disconnectPersonal)}>Disconnect</button>
          {:else}
            <button class="btn-accent" disabled={personalBusy || !personal.status?.client_id || personal.status?.authorization_pending} onclick={() => personalAction(() => authorizePersonal(false))}>Authorize</button>
          {/if}
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Spotify Connect devices</div>
          <div class="d">Optional remote Spotify Connect playback. Renderer keeps playing through this PC's OS default output; transferring a separate Spotify Connect session does not move Renderer audio. Off makes no device requests. Device access needs separate authorization.</div>
        </div>
        <div class="set-ctl">
          <input class="set-check" type="checkbox" aria-label="Enable Spotify Connect" checked={personal.status?.devices_enabled ?? false} disabled={personalBusy || !personalConnected()} onchange={(event) => personalAction(() => setDevicesEnabled(event.currentTarget.checked))} />
          {#if personal.status?.devices_enabled && !personal.status?.devices_authorized}
            <button class="btn-ghost" disabled={personalBusy || personal.status?.authorization_pending} onclick={() => personalAction(() => authorizePersonal(true))}>{personal.status?.authorization_pending ? "Waiting for authorization…" : "Authorize devices"}</button>
          {/if}
        </div>
      </div>
    </div>

    <div class="set-group">
      <h2>Playback &amp; startup</h2>
      <div class="set-row">
        <div>
          <div class="k">Volume normalization</div>
          <div class="d">
            Levels tracks using Spotify's loudness tags, so quiet and loud masters play at a
            similar volume. Constant per-track gain only — it can turn tracks down but never
            compresses or limits dynamics. Switching briefly reconnects playback.
          </div>
          {#if settingErrors.normalisation}<div class="inline-error" role="alert">{settingErrors.normalisation}</div>{/if}
        </div>
        <div class="set-ctl">
          <input
            class="set-check"
            type="checkbox"
            aria-label="Volume normalization"
            checked={appSettings?.normalisation ?? false}
            disabled={!appSettings || !!settingBusy}
            onchange={(event) => updateNormalisation(event.currentTarget.checked)}
          />
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Launch at login</div>
          <div class="d">Starts Renderer when you sign in.</div>
          {#if settingErrors.launchAtLogin}<div class="inline-error" role="alert">{settingErrors.launchAtLogin}</div>{/if}
        </div>
        <div class="set-ctl">
          <input
            class="set-check"
            type="checkbox"
            aria-label="Launch at login"
            checked={appSettings?.launch_at_login ?? false}
            disabled={!appSettings || !!settingBusy}
            onchange={(event) => updateLaunchAtLogin(event.currentTarget.checked)}
          />
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Start minimized</div>
          <div class="d">Opens the window minimized; closing it still exits the app.</div>
          {#if settingErrors.startMinimized}<div class="inline-error" role="alert">{settingErrors.startMinimized}</div>{/if}
        </div>
        <div class="set-ctl">
          <input
            class="set-check"
            type="checkbox"
            aria-label="Start minimized"
            checked={appSettings?.start_minimized ?? false}
            disabled={!appSettings || !!settingBusy}
            onchange={(event) => updateStartMinimized(event.currentTarget.checked)}
          />
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Animated Canvas</div>
          <div class="d">
            Show Spotify's looping Canvas videos in the Now playing panel when they are available.
            Spotify can withhold Canvas per account, so this switch can only turn it off; enable
            video in Spotify's own settings first.
          </div>
          {#if settingErrors.animatedCanvas}<div class="inline-error" role="alert">{settingErrors.animatedCanvas}</div>{/if}
        </div>
        <div class="set-ctl">
          <input
            class="set-check"
            type="checkbox"
            aria-label="Animated Canvas"
            checked={appSettings?.animated_canvas ?? false}
            disabled={!appSettings || !!settingBusy}
            onchange={(event) => updateAnimatedCanvas(event.currentTarget.checked)}
          />
        </div>
      </div>
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
          {#if settingErrors.audioCacheLimit}<div class="inline-error" role="alert">{settingErrors.audioCacheLimit}</div>{/if}
        </div>
        <div class="set-ctl">
          <Select
            label="Audio cache limit"
            options={CACHE_LIMITS}
            value={cacheLimitMb}
            disabled={!appSettings || !!settingBusy}
            onchange={updateAudioCacheLimit}
          />
        </div>
      </div>
      <div class="set-row" data-cache-stat="covers">
        <div>
          <div class="k">Cover cache</div>
          <div class="d">Artwork downloaded once, then served from disk.</div>
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
          <div class="k">Cache activity</div>
          <div class="d">{cacheStats.error ?? `${stats.coversResolved} cover requests this session`}</div>
        </div>
        <div class="set-ctl">
          <button class="btn-ghost" onclick={() => refreshCacheStats().catch(() => {})} disabled={cacheStats.loading}>
            {cacheStats.loading ? "Measuring…" : "Refresh"}
          </button>
        </div>
      </div>
    </div>

    <div class="set-group"><h2>Updates</h2><UpdateControl /></div>

    <div class="set-group">
      <h2>Diagnostics</h2>
      <div class="set-row">
        <div>
          <div class="k">Engine</div>
          <div class="d">Check whether the engine answers now. Reachability does not confirm Spotify sign-in or audio output.</div>
        </div>
        <div class="set-ctl">
          <span class="v status" class:ok={ping === "ok"} class:bad={ping === "failed"}>
            <span class="status-dot" aria-hidden="true"></span>{pingLabel}
          </span>
          <button class="btn-ghost" onclick={checkEngine} disabled={ping === "checking"}>Check</button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Playback session</div>
          <div class="d">{playback.ready && playback.auth_state === "ready" ? "Spotify session reports ready. Audio output has not been tested here." : "Spotify session is not ready for playback."}</div>
        </div>
        <div class="set-ctl"><span class="v">{playback.ready && playback.auth_state === "ready" ? "Ready" : "Not ready"}</span></div>
      </div>
      <div class="set-row">
        <div class="k">Auth state</div>
        <div class="set-ctl">
          <span class="v status" class:ok={playback.auth_state === "ready"}>
            <span class="status-dot" aria-hidden="true"></span>{playback.auth_state ?? "unknown"}
          </span>
        </div>
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

<style>
  .client-id-input {
    width: 180px; min-width: 0; height: 34px; padding: 0 var(--s2);
    border: 1px solid var(--line-2); border-radius: var(--r2);
    background: var(--raise-1); color: var(--fg);
    font-size: var(--t-12);
  }
</style>
