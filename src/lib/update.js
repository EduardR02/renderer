import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export { getVersion as getCurrentVersion };

// Native release metadata avoids requesting a nonexistent legacy manifest when
// this installation is already current. Newer releases retain the plugin's
// signed download/install resource; failed checks always reject.
export async function checkForUpdate() {
  const metadata = await invoke("check_update");
  return metadata ? new Update(metadata) : null;
}

export async function downloadAndInstall(update, onProgress) {
  await update.downloadAndInstall(onProgress);
}

export { relaunch as restartApp };
