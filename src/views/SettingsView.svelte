<script>
  import { tick, untrack } from "svelte";
  import {
    playback,
    session,
    ui,
    cacheStats,
    refreshCacheStats,
    clearCache,
    api,
    appSettings,
    settingsState,
    openAuthUrl,
    isLoggedOut,
  } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
  import Cover from "../components/Cover.svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { avatar, loadAvatar } from "../lib/avatar.svelte.js";
  import { writeClipboard } from "../lib/spotify-link.js";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import Select from "../components/Select.svelte";
  import { formatBytes } from "../lib/time.js";
  import UpdateControl from "../components/UpdateControl.svelte";
  import {
    personal, personalConnected, personalDevicesAuthorized, watchPersonal, configurePersonal,
    authorizePersonal, disconnectPersonal,
  } from "../lib/personal.svelte.js";

  /* The redirect the personal app must list, and the one place a person
     makes that app. Both fixed. */
  const REDIRECT_URI = "http://127.0.0.1:5589/personal-api/callback";
  const DASHBOARD = "https://developer.spotify.com/dashboard";
  let clientId = $state("");
  let clientIdVisible = $state(false);
  const storedClientId = $derived(personal.status?.client_id ?? "");
  const clientIdDirty = $derived(clientId.trim() !== storedClientId);
  let uriCopied = $state(false);
  let uriTimer = 0;
  $effect(() => () => clearTimeout(uriTimer));
  async function copyRedirect() {
    uriCopied = await writeClipboard(REDIRECT_URI);
    clearTimeout(uriTimer);
    if (uriCopied) uriTimer = setTimeout(() => (uriCopied = false), 1400);
  }
  const compact = $derived((ui.paneWidth || 1200) < 560);
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
  $effect(() => { loadAvatar(session.username); });

  let clearTarget = $state(null);
  let clearing = $state(false);
  let clearError = $state("");
  let settingBusy = $state("");
  const requestedSettings = $state({});
  const settingErrors = $state({
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

  async function updateSetting(key, requested, update, fallbackMessage) {
    if (!settingsState.loaded || settingBusy) return;
    settingBusy = key;
    requestedSettings[key] = requested;
    settingErrors[key] = "";
    try {
      // Render the requested value even if the command rejects synchronously.
      // Clearing it then necessarily restores the saved value in the DOM.
      await tick();
      await update();
    } catch (error) {
      settingErrors[key] = String(error || fallbackMessage);
    } finally {
      delete requestedSettings[key];
      if (settingBusy === key) settingBusy = "";
    }
  }

  function updateAudioCacheLimit(mb) {
    if (!Number.isFinite(mb)) return;
    return updateSetting(
      "audioCacheLimit",
      mb,
      () => api.setAudioCacheLimit(mb),
      "Could not save the cache limit.",
    );
  }

  function updateLaunchAtLogin(enabled) {
    return updateSetting(
      "launchAtLogin",
      enabled,
      () => api.setLaunchAtLogin(enabled),
      "Could not update launch at login.",
    );
  }

  function updateStartMinimized(enabled) {
    return updateSetting(
      "startMinimized",
      enabled,
      () => api.setStartMinimized(enabled),
      "Could not update start minimized.",
    );
  }

  function updateAnimatedCanvas(enabled) {
    return updateSetting(
      "animatedCanvas",
      enabled,
      () => api.setAnimatedCanvas(enabled),
      "Could not update animated Canvas.",
    );
  }

  function updateNormalisation(enabled) {
    return updateSetting(
      "normalisation",
      enabled,
      () => api.setNormalisation(enabled),
      "Could not update volume normalization.",
    );
  }


  // The command is cached server-side for a minute and this effect runs once
  // per Settings mount, so reopening the page stays fresh without a disk walk
  // on every render.
  $effect(() => {
    refreshCacheStats().catch(() => {});
  });
</script>

<section class="view page settings-page" class:compact>
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
                : "Waiting for Spotify sign-in to be ready…"}
            </div>
          </div>
          <div class="set-ctl">
            <button
              class="pill accent"
              disabled={!playback.auth_url || session.authPending}
              onclick={openAuthUrl}
            >
              <Icon name="login" size={15} />{session.authPending ? "Opening…" : "Log in"}
            </button>
          </div>
        </div>
      {:else}
        <div class="set-row account-row">
          <div class="account-who">
            <Cover src={avatar.url} id={username || "account"} name={username || "?"} size={40} circle />
            <div>
              <div class="k">{username || "Signed in"}</div>
              <div class="d">Spotify account</div>
            </div>
          </div>
          <div class="set-ctl">
            <button class="pill" onclick={() => api.logout().catch(() => {})}>
              <Icon name="logout" size={14} />Log out
            </button>
          </div>
        </div>
      {/if}
      {#if session.error}
        <div class="set-row">
          <div>
            <div class="k">Last session error</div>
            <div class="inline-error" role="alert">{session.error}</div>
          </div>
          <div class="set-ctl"><span class="dot warn"></span></div>
        </div>
      {/if}
    </div>

    <!-- What the personal app is FOR, in its name. One sentence, the setup as
         three short steps while it is not done, then only its state. -->
    <div class="set-group personal-app">
      <h2>Likes, follows &amp; devices</h2>
      <p class="set-intro">
        Your own free Spotify developer app, for this same account, lets Renderer like songs,
        follow artists and people, list your saved podcasts and move a Spotify Connect session.
      </p>
      {#if !personalConnected()}
        <ol class="setup">
          <li>
            <span class="step">1</span>
            <span class="step-copy">Create an app in the <button class="link-more inline-link" onclick={() => openUrl(DASHBOARD).catch(() => {})}>Spotify developer dashboard</button>.</span>
          </li>
          <li>
            <span class="step">2</span>
            <span class="step-copy">
              Add this redirect URI to it
              <span class="uri-chip">
                <code>{REDIRECT_URI}</code>
                <button class="btn-round uri-copy" class:on={uriCopied} title={uriCopied ? "Copied" : "Copy redirect URI"} aria-label="Copy redirect URI" onclick={copyRedirect}>
                  <Icon name={uriCopied ? "check" : "copy"} size={13} />
                </button>
              </span>
            </span>
          </li>
          <li>
            <span class="step">3</span>
            <span class="step-copy">Paste its Client ID here.</span>
          </li>
        </ol>
      {/if}
      <form class="set-row client-row" onsubmit={(event) => { event.preventDefault(); if (clientIdDirty) personalAction(() => configurePersonal(clientId)); }}>
        <label class="k" for="spotify-client-id">Client ID</label>
        <div class="set-ctl client-ctl">
          <span class="field client-field">
            <input
              id="spotify-client-id"
              type={clientIdVisible ? "text" : "password"}
              bind:value={clientId}
              placeholder="32 characters from your app"
              autocomplete="off"
              spellcheck="false"
            />
            <button type="button" class="field-btn" aria-controls="spotify-client-id" aria-pressed={clientIdVisible}
              title={clientIdVisible ? "Hide Client ID" : "Show Client ID"} aria-label={clientIdVisible ? "Hide Client ID" : "Show Client ID"}
              onclick={() => (clientIdVisible = !clientIdVisible)}>
              <Icon name={clientIdVisible ? "eye-off" : "eye"} size={14} />
            </button>
          </span>
          <button type="submit" class="pill accent" disabled={personalBusy || !clientIdDirty}>Save</button>
        </div>
      </form>
      <div class="set-row">
        <div>
          <div class="v status" class:ok={personalConnected()} class:bad={!!(personalError || personal.error)}>
            <span class="status-dot"></span>
            <span class="k status-text">
              {#if personalConnected()}Connected as {personal.status.account_id}
              {:else if personal.status?.connected}Connected as {personal.status.account_id}, not this account
              {:else if personal.status?.authorization_pending}Waiting for Spotify in your browser…
              {:else if personal.loading && !personal.status}Checking…
              {:else}Not connected{/if}
            </span>
          </div>
          {#if personalError || personal.error}<div class="inline-error" role="alert">{personalError || personal.error}</div>{/if}
        </div>
        <div class="set-ctl">
          {#if personal.status?.connected}
            <button class="pill" disabled={personalBusy} onclick={() => personalAction(disconnectPersonal)}>Disconnect</button>
          {:else}
            <button class="pill accent" disabled={personalBusy || !storedClientId || personal.status?.authorization_pending} onclick={() => personalAction(() => authorizePersonal(false))}>Connect</button>
          {/if}
        </div>
      </div>
      {#if personalConnected()}
        <div class="set-row">
          <div>
            <div class="k">Spotify Connect devices</div>
            <div class="d">Choose where Renderer plays from the player bar: this computer or another Spotify device.</div>
          </div>
          <div class="set-ctl">
            {#if personalDevicesAuthorized()}
              <span class="v status ok"><span class="status-dot"></span>Allowed</span>
            {:else}
              <button class="pill accent" disabled={personalBusy || personal.status?.authorization_pending} onclick={() => personalAction(() => authorizePersonal(true))}>Allow</button>
            {/if}
          </div>
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
            checked={requestedSettings.normalisation ?? appSettings?.normalisation ?? false}
            disabled={!settingsState.loaded || !!settingBusy}
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
            checked={requestedSettings.launchAtLogin ?? appSettings?.launch_at_login ?? false}
            disabled={!settingsState.loaded || !!settingBusy}
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
            checked={requestedSettings.startMinimized ?? appSettings?.start_minimized ?? false}
            disabled={!settingsState.loaded || !!settingBusy}
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
            checked={requestedSettings.animatedCanvas ?? appSettings?.animated_canvas ?? false}
            disabled={!settingsState.loaded || !!settingBusy}
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
          <button class="pill warn" onclick={() => requestClear("audio")}>Clear</button>
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
            value={requestedSettings.audioCacheLimit ?? cacheLimitMb}
            disabled={!settingsState.loaded || !!settingBusy}
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
          <button class="pill warn" onclick={() => requestClear("covers")}>Clear</button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Cache activity</div>
          <div class="d">{cacheStats.error ?? "Artwork is fetched on demand and cached."}</div>
        </div>
        <div class="set-ctl">
          <button class="pill" onclick={() => refreshCacheStats().catch(() => {})} disabled={cacheStats.loading}>
            {cacheStats.loading ? "Measuring…" : "Refresh"}
          </button>
        </div>
      </div>
    </div>

    <div class="set-group"><h2>Updates</h2><UpdateControl /></div>

    {#if playback.error && playback.error !== session.error}
      <div class="set-group">
        <h2>Engine</h2>
        <div class="set-row engine-error-row">
          <div>
            <div class="k">Last engine error</div>
            <div class="inline-error" role="alert">{playback.error}</div>
          </div>
        </div>
      </div>
    {/if}
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
  .account-row .account-who { display: flex; align-items: center; gap: var(--s4); min-width: 0; }
  .account-row .account-who :global(.art) { flex: none; }

  .set-intro { max-width: 60ch; margin: var(--s1) 0 var(--s2); color: var(--fg-2); font-size: var(--t-12); line-height: 1.6; }
  /* The setup, as three short numbered lines rather than a paragraph with a
     URI buried in it. */
  .setup { list-style: none; margin: 0; padding: var(--s2) 0 var(--s1); display: grid; gap: var(--s3); }
  .setup li { display: flex; align-items: center; gap: var(--s3); color: var(--fg-1); font-size: var(--t-12); }
  .step {
    display: grid; place-items: center; width: 20px; height: 20px; flex: none;
    border-radius: var(--rf); box-shadow: inset 0 0 0 1px var(--line-2);
    color: var(--fg-2); font-family: var(--font-number); font-size: var(--t-11); font-weight: var(--w-med);
  }
  .step-copy { min-width: 0; line-height: 1.6; }
  .inline-link { font-size: inherit; color: var(--fg); text-decoration: underline; text-decoration-color: var(--line-2); text-underline-offset: 3px; }
  .inline-link:hover { text-decoration-color: currentColor; }
  /* The URI in the field's own pressed material, with its copy glyph inside. */
  .uri-chip {
    display: inline-flex; align-items: center; gap: var(--s2); min-width: 0; max-width: 100%;
    height: 28px; margin-left: var(--s2); padding: 0 2px 0 var(--s3); vertical-align: middle;
    border-radius: var(--rf); background: rgb(0 0 0 / 0.22); border: 1px solid rgba(255, 255, 255, 0.08);
  }
  .uri-chip code {
    min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    font-family: var(--font-mono); font-size: var(--t-11); color: var(--fg-1); user-select: all;
  }
  .uri-copy { width: 24px; height: 24px; }
  .client-row { align-items: center; }
  .client-ctl { flex: 1; min-width: 0; justify-content: flex-end; }
  .client-field { flex: 0 1 320px; min-width: 0; height: 34px; }
  .client-field input { font-family: var(--font-mono); font-size: var(--t-12); }
  .status-text { color: inherit; }
  .engine-error-row { grid-template-columns: minmax(0, 1fr); }
  .compact .personal-app .set-row { grid-template-columns: minmax(0, 1fr); gap: var(--s3); }
  .compact .personal-app .set-ctl { justify-content: flex-start; flex-wrap: wrap; }
</style>
