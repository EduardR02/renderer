import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { session, personalLibraryChanged } from "./state.svelte.js";

export const personal = $state({ status: null, loading: false, error: "", membershipRevision: 0 });
let request = null;
let initialized = false;
let listening = null;

export function refreshPersonal() {
  if (request) return request;
  personal.loading = true;
  request = invoke("personal_api_status").then((status) => {
    personal.status = status;
    personal.error = status?.error || "";
    return status;
  }).catch((error) => {
    personal.error = String(error);
    throw error;
  }).finally(() => {
    request = null;
    personal.loading = false;
  });
  return request;
}

export function watchPersonal() {
  if (!listening) {
    listening = listen("personal-api-changed", ({ payload }) => {
      personal.status = payload;
      personal.error = payload?.error || "";
    });
  }
  if (!initialized) {
    initialized = true;
    refreshPersonal().catch(() => {});
  }
}

export function personalConnected() {
  return !!personal.status?.connected && !!session.username &&
    personal.status.account_id === session.username;
}

export async function configurePersonal(clientId) {
  personal.status = await invoke("personal_api_configure", { clientId: clientId.trim() });
  personal.error = "";
}

export async function authorizePersonal(enableDevices = false) {
  const { url } = await invoke("personal_api_authorize", { enableDevices });
  await openUrl(url);
  await refreshPersonal();
}

export async function disconnectPersonal() {
  personal.status = await invoke("personal_api_disconnect");
  personal.error = "";
}

export async function setDevicesEnabled(enabled) {
  personal.status = await invoke("personal_api_set_devices_enabled", { enabled });
}

export const personalApi = {
  contains: (uris) => invoke("personal_api_contains", { uris }),
  setSaved: async (uris, saved) => {
    const account = session.username;
    await invoke("personal_api_set_saved", { uris, saved });
    if (account === session.username) {
      personalLibraryChanged(uris);
      personal.membershipRevision++;
    }
  },
  devices: () => invoke("personal_api_devices"),
  transfer: (deviceId, play) => invoke("personal_api_transfer", { deviceId, play }),
  savedShows: (offset = 0, limit = 50) => invoke("personal_api_saved_shows", { offset, limit }),
};
