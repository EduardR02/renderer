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
          <div class="d">Starts Spotify Renderer when you sign in to Windows.</div>
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
          <div class="d">Opens the normal window minimized to the taskbar; closing it still exits the app.</div>
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
        <div class="set-ctl"><span class="v">0.1.0</span></div>
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
  /* A native checkbox ignores the app's control language: at this size its
     checked glyph lands gray-on-gray, outside the accent system every other
     control here speaks. Drawn instead, in the .btn-accent vocabulary: an
     unchecked box is a raised field with a hairline; a checked one is the
     foam fill with the ink glyph. Disabled keeps the dimmed field the Select
     and the filter input use; :focus-visible keeps the global outline. */
  .set-check {
    appearance: none;
    display: grid; place-items: center;
    width: 16px; height: 16px; margin: 0;
    border: 1px solid var(--line-2); border-radius: var(--r1);
    background: var(--bg-2); cursor: pointer;
    transition: background-color var(--d1) var(--ease), border-color var(--d1) var(--ease);
  }
  .set-check:hover:enabled { border-color: var(--fg-3); }
  .set-check:checked {
    background: var(--accent);
    border-color: var(--accent);
  }
  .set-check:checked::before {
    content: "";
    width: 10px; height: 10px;
    background: var(--accent-ink);
    clip-path: polygon(13% 50%, 0 63%, 37% 100%, 100% 16%, 87% 3%, 37% 72%);
  }
  .set-check:disabled {
    cursor: default;
    opacity: 0.45;
  }
</style>
