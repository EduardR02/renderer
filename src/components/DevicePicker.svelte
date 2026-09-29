<script>
  import { personal, personalApi, personalConnected, watchPersonal } from "../lib/personal.svelte.js";
  import { navigate } from "../lib/state.svelte.js";
  let open = $state(false);
  let devices = $state([]);
  let loading = $state(false);
  let transferring = $state("");
  let error = $state("");
  let transferNotice = $state("");
  let generation = 0;
  $effect(() => watchPersonal());
  $effect(() => {
    if (!personal.status?.devices_enabled || !personal.status?.devices_authorized || !personalConnected()) {
      generation++;
      loading = false;
      open = false;
      devices = [];
      transferNotice = "";
    }
  });
  async function refresh() {
    if (!open || loading || !personal.status?.devices_enabled || !personal.status?.devices_authorized || !personalConnected()) return;
    const current = generation;
    loading = true;
    error = "";
    try {
      const result = await personalApi.devices();
      if (current === generation && open) devices = result;
    } catch (reason) { if (current === generation && open) error = String(reason); }
    finally { if (current === generation) loading = false; }
  }
  function toggle() {
    open = !open;
    if (open) refresh();
    else { generation++; devices = []; transferNotice = ""; loading = false; }
  }
  async function transfer(device) {
    if (!personalConnected() || !personal.status?.devices_enabled || !personal.status?.devices_authorized || !device.id || device.is_restricted || transferring) return;
    transferring = device.id;
    error = "";
    try {
      await personalApi.transfer(device.id, false);
      transferNotice = `Remote Spotify session transferred to ${device.name}, without requesting playback. Renderer audio and queue stay on this PC.`;
      await refresh();
    } catch (reason) { error = String(reason); }
    finally { transferring = ""; }
  }
</script>
{#if personal.status?.devices_enabled}
  <div class="device-picker">
    <button class="btn-ghost" onclick={toggle} aria-expanded={open} aria-label="Remote Spotify Connect session">
      Remote Spotify
    </button>
    {#if open}
      <div class="device-menu glass-overlay">
        <strong>Remote Spotify session</strong>
        <p>Transfer Spotify's remote session without requesting playback. This does not send Renderer's track or queue; Renderer audio and controls stay on this PC.</p>
        {#if transferNotice}<p role="status">{transferNotice}</p>{/if}
        {#if !personal.status?.devices_authorized}
          <p>Authorize device access in Settings first.</p><button class="link-more" onclick={() => navigate("settings")}>Open Settings</button>
        {:else}
          <button class="link-more" onclick={refresh} disabled={loading}>{loading ? "Refreshing…" : "Refresh devices"}</button>
          {#each devices as device (device.id ?? device.name)}
            <button class="device-row" disabled={!device.id || device.is_restricted || !!transferring}
              onclick={() => transfer(device)}>Transfer to {device.name} · {device.type}{device.is_active ? " · active" : ""}{device.is_restricted ? " · restricted" : ""}</button>
          {/each}
          {#if !loading && !devices.length && !error}<p>No remote devices found. Open Spotify on another device, then refresh.</p>{/if}
        {/if}
        {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      </div>
    {/if}
  </div>
{/if}
<style>
  .device-picker { position:relative; flex:none; }
  .device-menu { position:absolute; bottom:calc(100% + 12px); right:0; width:min(320px,80vw); padding:16px; z-index:30; display:grid; gap:8px; }
  .device-menu p { font-size:12px; line-height:1.4; margin:0; }
  .device-row { background:transparent; border:0; color:inherit; cursor:pointer; text-align:left; padding:8px; }
  .device-row:hover { background:var(--raise-2); }
</style>
