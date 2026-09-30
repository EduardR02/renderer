<script>
  import { onMount } from "svelte";
  import { checkForUpdate, getCurrentVersion, downloadAndInstall, restartApp } from "../lib/update.js";

  let currentVersion = $state("");
  let update = $state(null);
  let phase = $state("idle");
  let errorMessage = $state("");
  let downloaded = $state(0);
  let total = $state(0);
  let active = true;

  onMount(() => {
    getCurrentVersion()
      .then((version) => {
        if (active) currentVersion = version;
      })
      .catch((error) => {
        if (active) errorMessage = `Could not read app version: ${String(error)}`;
      });
    return () => {
      active = false;
      if (update && phase !== "downloading" && phase !== "installing") {
        void update.close();
      }
    };
  });

  async function checkUpdates() {
    if (phase === "checking" || phase === "downloading" || phase === "installing") return;
    phase = "checking";
    errorMessage = "";
    if (update) {
      const previous = update;
      update = null;
      try {
        await previous.close();
      } catch (error) {
        phase = "error";
        errorMessage = `Could not release previous update: ${String(error)}`;
        return;
      }
    }
    try {
      const result = await checkForUpdate();
      if (!active) {
        if (result) await result.close();
        return;
      }
      update = result;
      phase = result ? "available" : "current";
    } catch (error) {
      if (active) {
        phase = "error";
        errorMessage = `Could not check for updates: ${String(error)}`;
      }
    }
  }

  async function restart() {
    try {
      await restartApp();
    } catch (error) {
      if (active) errorMessage = `The update is installed, but restart failed: ${String(error)}`;
    }
  }

  async function install() {
    if (!update || phase !== "available") return;
    phase = "downloading";
    errorMessage = "";
    downloaded = 0;
    total = 0;
    const selected = update;
    try {
      await downloadAndInstall(selected, (event) => {
        if (!active) return;
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
        } else if (event.event === "Finished") {
          phase = "installing";
        }
      });
      if (active) phase = "installed";
      await restart();
    } catch (error) {
      if (active) {
        phase = "error";
        errorMessage = `Could not install the update: ${String(error)}`;
      }
    } finally {
      update = null;
      await selected.close().catch(() => {});
    }
  }
</script>

<div class="set-row update-row">
  <div>
    <div class="k">App updates</div>
    <div class="d">Version {currentVersion || "unavailable"}</div>
    {#if phase === "current"}<div class="d" role="status">No newer release is available.</div>{/if}
    {#if phase === "available" && update}
      <div class="d" role="status">Version {update.version} is available.</div>
    {/if}
    {#if phase === "downloading"}<div class="d" role="status">Downloading update{total ? `: ${Math.min(100, Math.floor(downloaded * 100 / total))}%` : "…"}</div>{/if}
    {#if phase === "installing"}<div class="d" role="status">Installing update…</div>{/if}
    {#if phase === "installed"}<div class="d" role="status">Update installed. Restart the app to use it.</div>{/if}
    {#if errorMessage}<div class="inline-error" role="alert">{errorMessage}</div>{/if}
  </div>
  <div class="set-ctl">
    {#if phase === "available" && update}
      <button class="btn-accent" onclick={install}>Install and restart</button>
    {/if}
    {#if phase === "installed"}
      <button class="btn-accent" onclick={restart}>Restart now</button>
    {/if}
    <button class="btn-ghost" onclick={checkUpdates} disabled={phase === "checking" || phase === "downloading" || phase === "installing"}>
      {phase === "checking" ? "Checking…" : "Check for updates"}
    </button>
  </div>
</div>

