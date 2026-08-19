<script>
  import {
    playback,
    session,
    stats,
    api,
    openAuthUrl,
    isLoggedOut,
  } from "../lib/state.svelte.js";
  import Icon from "../components/Icon.svelte";
  import Slider from "../components/Slider.svelte";

  const username = $derived(playback.username || session.username);

  const repeatLabel = $derived(
    playback.repeat === "track" ? "One song" : playback.repeat === "context" ? "Whole queue" : "Off"
  );

  function cycleRepeat() {
    const order = ["off", "context", "track"];
    const next = order[(order.indexOf(playback.repeat) + 1) % order.length];
    api.setRepeat(next).catch(() => {});
  }

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
</script>

<section class="view page">
  <div style="padding:var(--s4) 0 var(--s2)">
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

    <div class="set-group">
      <h2>Playback</h2>
      <div class="set-row">
        <div>
          <div class="k">Shuffle</div>
          <div class="d">Plays the current queue in a random order.</div>
        </div>
        <div class="set-ctl">
          <button
            class="switch"
            class:on={playback.shuffle}
            role="switch"
            aria-checked={playback.shuffle}
            aria-label="Shuffle"
            onclick={() => api.setShuffle(!playback.shuffle).catch(() => {})}
          ></button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Repeat</div>
          <div class="d">Off, the whole queue, or the current song.</div>
        </div>
        <div class="set-ctl">
          <button class="btn-ghost" onclick={cycleRepeat}>
            <Icon name={playback.repeat === "track" ? "repeat-one" : "repeat"} size={14} />
            {repeatLabel}
          </button>
        </div>
      </div>
      <div class="set-row">
        <div>
          <div class="k">Volume</div>
          <div class="d">Applies to the engine's own output, not the system mixer.</div>
        </div>
        <div class="set-ctl">
          <span class="v">{Math.round(playback.volume)}%</span>
          <Slider
            min={0}
            max={100}
            value={playback.volume}
            label="Volume"
            step={5}
            kind="vol"
            onCommit={(v) => api.setVolume(v).catch(() => {})}
          />
        </div>
      </div>
    </div>

    <div class="set-group">
      <h2>Storage</h2>
      <div class="set-row">
        <div>
          <div class="k">Cover cache</div>
          <div class="d">
            Covers are fetched once by the core and served from disk afterwards.
          </div>
        </div>
        <div class="set-ctl"><span class="v">{stats.coversResolved} this session</span></div>
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
