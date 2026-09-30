import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { session, sessionEpoch, libraryChanges, personalLibraryChanged, playback, lookupSavedIn } from "./state.svelte.js";

export const personal = $state({
  status: null, loading: false, error: "", membershipRevision: 0,
  membershipAccount: null, memberships: {},
});
let request = null;
let watchedSession = null;
let listening = null;
const MEMBERSHIP_TTL_MS = 30_000;
const MEMBERSHIP_CACHE_MAX = 1024;
const membershipReads = new Map();
let membershipEpoch = -1;
let membershipCollectionRevision = -1;
let warmQueue = new Set();
let warmPromise = null;

function syncMembershipAccount() {
  const epoch = sessionEpoch();
  if (membershipEpoch !== epoch || personal.membershipAccount !== session.username ||
      membershipCollectionRevision !== libraryChanges.savedTracks) {
    membershipEpoch = epoch;
    membershipCollectionRevision = libraryChanges.savedTracks;
    membershipReads.clear();
    warmQueue = new Set();
    warmPromise = null;
    personal.membershipAccount = session.username;
    personal.memberships = {};
    personal.membershipRevision++;
  }
  return epoch;
}

function containsMemberships(uris) {
  const epoch = syncMembershipAccount();
  const account = session.username;
  const missing = [...new Set(uris)].filter((uri) => {
    const entry = membershipReads.get(uri);
    return !entry || (!entry.pending && performance.now() - entry.answeredAt >= MEMBERSHIP_TTL_MS);
  });
  for (let offset = 0; offset < missing.length; offset += 40) {
    const batch = missing.slice(offset, offset + 40);
    const read = invoke("personal_api_contains", { uris: batch }).then((values) => {
      if (!Array.isArray(values) || values.length !== batch.length || values.some((value) => typeof value !== "boolean")) {
        throw new Error("Spotify returned incomplete library membership");
      }
      return values;
    });
    batch.forEach((uri, index) => {
      const entry = { pending: null, answeredAt: 0 };
      entry.pending = read.then((values) => {
        if (membershipReads.get(uri) === entry && epoch === sessionEpoch()) {
          const current = account === session.username &&
            membershipCollectionRevision === libraryChanges.savedTracks;
          if (current) personal.memberships[uri] = values[index];
          entry.pending = null;
          entry.answeredAt = current ? performance.now() : -Infinity;
        }
        return personalSaved(uri) ?? values[index];
      }).catch((error) => {
        if (membershipReads.get(uri) === entry) membershipReads.delete(uri);
        throw error;
      });
      membershipReads.delete(uri);
      membershipReads.set(uri, entry);
    });
  }
  const result = Promise.all(uris.map((uri) => membershipReads.get(uri)?.pending ?? personalSaved(uri)));
  for (const [uri, entry] of membershipReads) {
    if (membershipReads.size <= MEMBERSHIP_CACHE_MAX) break;
    if (entry.pending) continue;
    membershipReads.delete(uri);
    delete personal.memberships[uri];
  }
  return result;
}

function warmMemberships(uris) {
  if (!personalConnected()) return Promise.resolve([]);
  syncMembershipAccount();
  for (const uri of uris) if (uri) warmQueue.add(uri);
  if (!warmPromise) {
    const epoch = sessionEpoch();
    const queue = warmQueue;
    const pending = Promise.resolve().then(() => {
      if (warmPromise === pending) warmPromise = null;
      if (epoch !== sessionEpoch() || queue !== warmQueue) return [];
      warmQueue = new Set();
      return containsMemberships([...queue]);
    });
    warmPromise = pending;
  }
  return warmPromise;
}

export function refreshPersonal() {
  const key = `${sessionEpoch()}\u0000${session.username ?? ""}\u0000${playback.auth_state ?? ""}`;
  if (request?.key === key) return request.promise;
  const pending = { key, promise: null };
  personal.loading = true;
  pending.promise = invoke("personal_api_status").then((status) => {
    if (request === pending) {
      personal.status = status;
      personal.error = status?.error || "";
    }
    return status;
  }).catch((error) => {
    if (request === pending) personal.error = String(error);
    throw error;
  }).finally(() => {
    if (request === pending) {
      request = null;
      personal.loading = false;
    }
  });
  request = pending;
  return pending.promise;
}

export function watchPersonal() {
  if (!listening) {
    listening = listen("personal-api-changed", ({ payload }) => {
      request = null;
      personal.loading = false;
      personal.status = payload;
      personal.error = payload?.error || "";
    });
  }
  // watchPersonal is called from mounted effects. Reading identity/readiness
  // here refreshes the initially disconnected status when playback logs in.
  const key = `${sessionEpoch()}\u0000${session.username ?? ""}\u0000${playback.auth_state ?? ""}`;
  if (watchedSession !== key) {
    watchedSession = key;
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


/** Personal API membership wins over the shell's asynchronously reconciled index. */
export function personalSaved(uri) {
  return membershipEpoch === sessionEpoch() && membershipCollectionRevision === libraryChanges.savedTracks &&
    personal.membershipAccount === session.username
    ? personal.memberships[uri] : undefined;
}

function rememberMemberships(account, uris, values) {
  if (account !== session.username) return;
  if (personal.membershipAccount !== account) {
    personal.membershipAccount = account;
    personal.memberships = {};
  }
  uris.forEach((uri, index) => { personal.memberships[uri] = values[index] === true; });
}
export const personalApi = {
  contains: containsMemberships,
  warmMemberships,
  setSaved: async (uris, saved) => {
    const account = session.username;
    const epoch = syncMembershipAccount();
    await invoke("personal_api_set_saved", { uris, saved });
    if (account === session.username && epoch === sessionEpoch()) {
      rememberMemberships(account, uris, uris.map(() => saved));
      for (const uri of uris) {
        membershipReads.set(uri, { pending: null, answeredAt: performance.now() });
      }
      personalLibraryChanged(uris);
      membershipCollectionRevision = libraryChanges.savedTracks;
      personal.membershipRevision++;
      if (uris.includes(playback.current_uri)) lookupSavedIn(playback.current_uri);
    }
  },
  devices: () => invoke("personal_api_devices"),
  transfer: (deviceId, play) => invoke("personal_api_transfer", { deviceId, play }),
  savedShows: (offset = 0, limit = 50) => invoke("personal_api_saved_shows", { offset, limit }),
};
