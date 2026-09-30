<script>
  import { personalApi, personalDevicesAuthorized, watchPersonal } from "../lib/personal.svelte.js";
  import { api, playback } from "../lib/state.svelte.js";
  import Icon from "./Icon.svelte";
  import Menu from "./Menu.svelte";

  // Discovery is demand-only; selected-output sync belongs to the native
  // playback router and remains active when this menu closes.
  let open = $state(false);
  let button = $state(null);
  let devices = $state([]);
  let loading = $state(false);
  let error = $state("");
  let transferring = $state("");
  let generation = 0;

  $effect(() => watchPersonal());
  const authorized = $derived(personalDevicesAuthorized());
  const remote = $derived(playback.output_device_id);
  const remoteName = $derived(playback.output_device_name);
  $effect(() => {
    if (authorized) return;
    generation++;
    open = false;
    devices = [];
  });

  async function refresh() {
    if (!open || loading || !personalDevicesAuthorized()) return;
    const current = generation;
    loading = true;
    error = "";
    try {
      const list = await personalApi.devices();
      if (current !== generation) return;
      devices = Array.isArray(list) ? list : [];
    } catch (reason) {
      if (current === generation) error = String(reason);
    } finally {
      if (current === generation) loading = false;
    }
  }

  function toggle() {
    open = !open;
    if (open) refresh();
  }

  function dismiss() {
    open = false;
    generation++;
    loading = false;
    transferring = "";
  }

  async function select(device = null) {
    if ((device && (!device.id || device.is_restricted)) || transferring) return;
    const current = generation;
    transferring = device?.id ?? "local";
    error = "";
    try {
      await api.selectOutput(device?.id ?? null);
      if (current === generation && device) await refresh();
    } catch (reason) {
      if (current === generation) error = String(reason);
    } finally {
      if (current === generation) transferring = "";
    }
  }

  function glyph(type) {
    const kind = String(type ?? "").toLowerCase();
    if (kind === "smartphone" || kind === "tablet") return "phone";
    if (kind === "computer" || kind === "tv" || kind === "game_console" || kind === "castvideo") return "computer";
    return "speaker";
  }

  function typeLabel(type) {
    const kind = String(type ?? "").replace(/_/g, " ").toLowerCase();
    return kind ? kind[0].toUpperCase() + kind.slice(1) : "";
  }
</script>

{#if authorized || remote}
  <button
    class="btn-round device-btn"
    class:on={!!remote}
    bind:this={button}
    title={remote ? `Playback output · ${remoteName}` : "Playback output · This computer"}
    aria-label="Spotify Connect devices"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    <Icon name="devices" size={18} />
  </button>
  {#if open}
    <Menu anchor={button} side="above" align="end" label="Spotify Connect devices" class="device-menu" onclose={dismiss}>
      {#snippet children()}
        <div class="device-head">
          <span class="device-title">Spotify Connect</span>
          <button class="btn-round device-refresh" title="Refresh devices" aria-label="Refresh devices" disabled={loading} onclick={refresh}>
            <Icon name="refresh" size={15} />
          </button>
        </div>
        <p class="device-note">Play Renderer audio here or on a Spotify device. Returning to this computer pauses playback.</p>
        <button
          class="menu-item device-row"
          role="menuitemradio"
          aria-checked={!remote}
          aria-busy={transferring === "local"}
          disabled={!!transferring}
          title="Return playback to this computer, paused"
          onclick={() => select()}
        >
          <Icon name="computer" size={16} />
          <span class="device-name">This computer</span>
          {#if !remote}<span class="mark live"><Icon name="check" size={14} /></span>{/if}
        </button>
        {#each devices as device (device.id ?? device.name)}
          <button
            class="menu-item device-row"
            role="menuitemradio"
            aria-checked={remote === device.id}
            aria-busy={transferring === device.id}
            disabled={!authorized || !device.id || device.is_restricted || !!transferring}
            title={device.is_restricted ? `${device.name} cannot be controlled` : remote === device.id ? `Renderer output: ${device.name}` : `Play Renderer on ${device.name}`}
            onclick={() => select(device)}
          >
            <Icon name={glyph(device.type)} size={16} />
            <span class="device-name">{device.name}</span>
            <span class="device-type">{typeLabel(device.type)}</span>
            {#if remote === device.id}<span class="mark live"><Icon name="check" size={14} /></span>{/if}
          </button>
        {/each}
        {#if loading && !devices.length}
          <p class="device-note">Looking for devices…</p>
        {:else if !loading && !devices.length && !error}
          <p class="device-note">No devices found. Open Spotify on another device, then refresh.</p>
        {/if}
        {#if error}<p class="device-error" role="alert">{error}</p>{/if}
        {#if !error && remote && playback.error}<p class="device-error" role="alert">{playback.error}</p>{/if}
      {/snippet}
    </Menu>
  {/if}
{/if}

<style>
  /* Playing elsewhere is a state the bar keeps showing: the glyph in foam and
     the dot every other "on" control in the bar wears. */
  .device-btn { position: relative; }
  .device-btn.on::after {
    content: ""; position: absolute; bottom: 1px; left: 50%; margin-left: -1.5px;
    width: 3px; height: 3px; border-radius: 50%; background: var(--accent);
  }
  :global(.menu.device-menu) { width: 288px; padding-bottom: var(--s2); }
  .device-head {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s1) var(--s1) 0 var(--s3);
  }
  .device-title {
    font-family: var(--font-small); font-size: var(--t-caps); font-weight: var(--w-semi);
    letter-spacing: var(--track-caps); text-transform: uppercase; color: var(--label-hi);
  }
  .device-refresh { width: 28px; height: 28px; }
  .device-note {
    margin: 0 0 var(--s2); padding: 0 var(--s3);
    color: var(--fg-3); font-family: var(--font-small); font-size: var(--t-11); line-height: 1.45;
    white-space: normal;
  }
  .device-row { height: 38px; }
  .device-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .device-type {
    flex: none; margin-left: auto; color: var(--fg-3);
    font-family: var(--font-small); font-size: var(--t-11);
  }
  .device-row[aria-checked="true"] { color: var(--fg); }
  .device-row[aria-checked="true"] > :global(.icon:first-child) { color: var(--accent); }
  .mark.live { color: var(--accent); margin-left: auto; }
  .device-type + .mark.live { margin-left: var(--s2); }
  .device-row:disabled:not([aria-busy="true"]) { opacity: 0.5; }
  .device-row[aria-busy="true"] { opacity: 0.7; }
  .device-error {
    margin: var(--s2) 0 0; padding: 0 var(--s3);
    color: var(--love); font-size: var(--t-11); line-height: 1.4; overflow-wrap: anywhere;
  }
</style>
