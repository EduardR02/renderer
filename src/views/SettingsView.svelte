<script>
  import {
    playback,
    session,
    stats,
    cacheStats,
    refreshCacheStats,
    api,
    openAuthUrl,
    isLoggedOut,
  } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
  import { formatBytes } from "../lib/time.js";

  const username = $derived(playback.username || session.username);


  /* `status` is a fire-and-forget ping: the engine answers on the event
     channel, not in the response, so all this can report is reachability. */
  let ping = $state("idle");

  function checkEngine() {
    ping = "checking";
    api
      .status()
      .then(() => (ping = "ok"))
      .catch(() => (ping = "failed"));
  }

  const pingLabel = $derived(
    ping === "ok" ? "Reachable" : ping === "failed" ? "No answer" : ping === "checking" ? "Checking…" : "Not checked"
  );

  // The command is cached server-side for a minute and this effect runs once
  // per Settings mount, so reopening the page stays fresh without a disk walk
  // on every render.
  $effect(() => {
    refreshCacheStats().catch(() => {});
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
              {cacheStats.audio.files} files · {formatBytes(cacheStats.audio.bytes)}
            {:else if cacheStats.loading}
              Measuring…
            {:else}
              Unavailable
            {/if}
          </span>
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
              {cacheStats.covers.files} files · {formatBytes(cacheStats.covers.bytes)}
            {:else if cacheStats.loading}
              Measuring…
            {:else}
              Unavailable
            {/if}
          </span>
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
          <span class="v">{pingLabel}</span>
          <button class="btn-ghost" onclick={checkEngine} disabled={ping === "checking"}>Check</button>
        </div>
      </div>
      <div class="set-row">
        <div class="k">Auth state</div>
        <div class="set-ctl"><span class="v">{playback.auth_state ?? "unknown"}</span></div>
      </div>
      <div class="set-row">
        <div class="k">Queue length</div>
        <div class="set-ctl"><span class="v">{playback.queue.length}</span></div>
      </div>
      <div class="set-row">
        <div class="k">Version</div>
        <div class="set-ctl"><span class="v">0.1.0 / Svelte 5 / Tauri 2</span></div>
      </div>
    </div>
  </div>
</section>
