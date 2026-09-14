/**
 * The bounded bookkeeping shared by everything that resolves or samples cover
 * art.
 *
 * Both halves exist because one cover arrives from several places at once: a
 * playlist's mosaic cells ARE the covers its album cards draw, and the rail
 * draws the same playlist the page does. Without this, one image cost one IPC
 * round trip and one decode per tile that showed it. Neither cache holds a
 * whole library — a session walks thousands of covers — so the oldest entry
 * leaves first and the memory stays flat.
 */

/**
 * A memo and a concurrency-limited queue in front of an async reader.
 *
 * `read` is what one value costs to work on — decoding a cover to read its
 * colour, here. Two of those run per image otherwise: `coverTone` keys its own
 * cache by the whole pool of urls, which is right, because four cells are one
 * answer — but it means one file answers to several keys. The memo is keyed
 * per url, so the second ask shares the first one's promise instead of
 * decoding the same picture again.
 *
 * The queue is the other half. A library page mounts two hundred cards and
 * asks for every one of their colours at once; started together, the covers
 * nobody is looking at take the browser's decode queue ahead of the ones on
 * screen. `limit` reads run at a time and the rest wait their turn in the
 * order they were asked for, so the top of a list is served first.
 *
 * @param read  `(value) => Promise` — the work for one value
 * @param max   results the memo keeps, oldest evicted first
 * @param limit reads allowed to run at once
 * @returns `(key, value) => Promise`
 */
export function boundedReads(read, { max, limit }) {
  /** key → the promise, so a caller arriving mid-read shares it. */
  const memo = new Map();
  const queue = [];
  let reading = 0;

  const pump = () => {
    while (reading < limit && queue.length) {
      const next = queue.shift();
      reading += 1;
      /* Wrapped in an async function so a reader that throws before it
         returns a promise rejects its own caller instead of escaping into
         this loop and stranding everything queued behind it. */
      (async () => read(next.value))()
        .then(next.resolve, next.reject)
        .finally(() => {
          reading -= 1;
          pump();
        });
    }
  };

  return (key, value) => {
    const hit = memo.get(key);
    if (hit) return hit;
    const pending = new Promise((resolve, reject) => {
      queue.push({ value, resolve, reject });
      pump();
    });
    memo.set(key, pending);
    if (memo.size > max) memo.delete(memo.keys().next().value);
    return pending;
  };
}

/**
 * Empty answers that have to agree before a url is given up on.
 *
 * One is the network; two in a row are the url.
 */
const STRIKES = 2;

/**
 * How long two empty answers keep describing the same url.
 *
 * They only describe it while they are close together: a minute of no network
 * answers empty on a url whose cover exists, and that is the same two answers
 * a delisted record gives. So the memory is a statement about the recent past
 * and it expires — after which the url is asked for again, and a cover hidden
 * by an outage this long comes back on its own.
 */
const WINDOW_MS = 30_000;

/**
 * The urls whose resolution came back empty, and the rule that keeps them from
 * hiding a cover.
 *
 * Used for urls whose resolution has already failed: one url is every tile
 * that shows it, so without this a cover that cannot be fetched at all — a
 * delisted record, an engine that is not logged in — is asked for again by
 * every new tile that scrolls past, for the rest of the session. Bounded,
 * because a session walks thousands of covers: the url whose answer came
 * oldest leaves first, and an answer that is older than [`WINDOW_MS`] has
 * stopped counting.
 *
 * The answer itself arrives flattened, though. `resolveCoverUrl` turns a
 * dropped connection and a record with no artwork into the same empty result —
 * the engine's own error, the one thing that could tell them apart, is
 * discarded on the way — so a single empty answer is a measurement, not a
 * verdict. Measured twice in a row, it is a property of the url and the url is
 * left alone; measured once, the next tile that shows it asks again, and a
 * cover that one flicker of the network hid is drawn the moment the engine can
 * answer for it.
 *
 * @param max     urls remembered, oldest evicted first
 * @param options `within` — how long an empty answer stands, in ms
 * @returns `miss(url)` one empty answer, `resolved(url)` an answer that
 *          carried a url, `dead(url)` whether the url has answered empty often
 *          and recently enough to be left alone
 */
export function boundedMisses(max, { within = WINDOW_MS } = {}) {
  /** url → how many empty answers it has given, and when the last one came. */
  const misses = new Map();
  return {
    miss(url) {
      const previous = misses.get(url);
      misses.set(url, { count: (previous?.count ?? 0) + 1, at: Date.now() });
      if (misses.size > max) misses.delete(misses.keys().next().value);
    },
    resolved(url) {
      misses.delete(url);
    },
    dead(url) {
      const entry = misses.get(url);
      return !!entry && entry.count >= STRIKES && Date.now() - entry.at < within;
    },
  };
}
