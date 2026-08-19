/** m:ss / h:mm:ss from milliseconds. */
export function formatTime(ms) {
  if (!Number.isFinite(ms) || ms < 0) ms = 0;
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** "1 hr 12 min" style duration for collection footers. */
export function formatDuration(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "0 min";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.round((total % 3600) / 60);
  if (h > 0) return m > 0 ? `${h} hr ${m} min` : `${h} hr`;
  return `${Math.max(1, m)} min`;
}

/** Summed duration of a track list, as "8 hr 12 min". */
export function formatTotal(tracks) {
  return formatDuration(tracks.reduce((sum, t) => sum + (t.duration_ms || 0), 0));
}
