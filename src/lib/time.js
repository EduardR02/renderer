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

/** Human-readable binary byte size for cache and diagnostics surfaces. */
export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = -1;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const decimals = value >= 10 || Number.isInteger(value) ? 0 : 1;
  return `${value.toFixed(decimals)} ${units[unit]}`;
}

/** Summed duration of a track list, as "8 hr 12 min". */
export function formatTotal(tracks) {
  return formatDuration(tracks.reduce((sum, t) => sum + (t.duration_ms || 0), 0));
}
