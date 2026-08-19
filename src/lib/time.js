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

/**
 * "1 hr 12 min" style duration for collection footers.
 *
 * Under an hour it keeps seconds ("38 min 12 sec"): a five-track playlist
 * rounded to whole minutes threw away most of what the number was telling you.
 */
export function formatDuration(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "0 sec";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  if (h > 0) {
    const m = Math.round((total % 3600) / 60);
    return m > 0 ? `${h} hr ${m} min` : `${h} hr`;
  }
  const m = Math.floor(total / 60);
  const s = total % 60;
  if (m === 0) return `${s} sec`;
  return s > 0 ? `${m} min ${s} sec` : `${m} min`;
}

/** Summed duration of a track list, as "8 hr 12 min". */
export function formatTotal(tracks) {
  return formatDuration(tracks.reduce((sum, t) => sum + (t.duration_ms || 0), 0));
}
