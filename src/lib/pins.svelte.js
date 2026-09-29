import { session } from "./state.svelte.js";

export const pins = $state({ account: null, ids: [] });
const LIKED = "liked";

export function loadPins(account = session.username) {
  if (pins.account === account) return;
  pins.account = account || null;
  pins.ids = [];
  if (!account) return;
  try {
    const stored = JSON.parse(localStorage.getItem(`sr.library-pins:${account}`) || "[]");
    if (Array.isArray(stored)) pins.ids = stored.filter((id) => typeof id === "string" && (id === LIKED || /^[A-Za-z0-9]{22}$/.test(id)));
  } catch { /* Storage may be disabled; pins remain session-local. */ }
}

export function isPinned(id) {
  return !!pins.account && pins.ids.includes(id);
}

export function togglePin(id) {
  loadPins();
  if (!pins.account || !id) return;
  pins.ids = isPinned(id) ? pins.ids.filter((item) => item !== id) : [...pins.ids, id];
  try { localStorage.setItem(`sr.library-pins:${pins.account}`, JSON.stringify(pins.ids)); }
  catch { /* Storage may be disabled. */ }
}
