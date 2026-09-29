import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export { getVersion as getCurrentVersion };

// The updater verifies the minisign signature before installation. A failed
// manifest request rejects; only an actual successful check can return null.
export async function checkForUpdate() {
  return check();
}

export async function downloadAndInstall(update, onProgress) {
  await update.downloadAndInstall(onProgress);
}

export { relaunch as restartApp };
