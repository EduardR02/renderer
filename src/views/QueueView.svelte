<script>
  import { playback, api, navigate, togglePlay } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import ArtistLinks from "../components/ArtistLinks.svelte";
  import { formatTime, formatTotal } from "../lib/time.js";
  const queue = $derived(playback.queue);

  function playAt(i) {
    if (i === playback.current_index) togglePlay();
    else api.playQueueIndex(i).catch(() => {});
  }

  /* ---------------- Windowed rendering ----------------
     Same scheme as TrackList: a fixed-height body preserves the queue's full
     geometry while an absolute-index-keyed window is translated through it.
     Sliding by one row retains every overlapping row/Cover subtree and only
     removes and adds the boundary rows. The scroller is an ancestor (.scroll
     in App.svelte), not this component, so the offset has to be read from it
     rather than from a local scrollTop.
     The queue differs from a playlist table in one way: the engine re-sends
     the whole queue (a fresh array) on every change, so identity changes on
     the queue's own edits too. Those keep the window — a move/remove/play-from
     here must not yank the scroll — while a queue that no longer contains the
     visible tracks is a genuinely new context and resets to the top. */
  const ROW_H = 48; // must track --row-h
  /* Sliding the window one row at a time measured better than batching it into
     blocks of 8 (92 vs 86 fps, worst frame 18ms vs 27ms): the batched version
     does the same total work in rarer, bigger bursts, and it is the burst that
     misses the frame. Keep the updates small and frequent. */
  const OVERSCAN = 6;

  let bodyEl = $state(null);
  let firstRow = $state(0);
  let lastRow = $state(0);
  /* Plain mirrors of the two above: measure() must not *read* reactive state,
     or the wiring effect below would re-subscribe on every scroll frame. */
  let curFirst = 0;
  let curLast = 0;

  /* Tracks identity and length so the effect below fires before the new rows
     are patched in, while an identity change on its own is not enough: the
     engine replaces the array on every mutation, so the visible rows decide
     whether this is an edit of the same queue or a brand-new one. */
  let seenQueue = null;
  let seenLength = -1;

  const visible = $derived(queue.slice(firstRow, lastRow));

  function measure(scroller) {
    if (!bodyEl || !scroller) return;
    const len = queue.length;
    // Layout is clean during scroll, so these reads are cheap and — unlike a
    // cached offset — stay correct when the header above the list changes size.
    const above = scroller.getBoundingClientRect().top - bodyEl.getBoundingClientRect().top;
    const f = Math.max(0, Math.floor(above / ROW_H) - OVERSCAN);
    const l = Math.min(len, Math.ceil((above + scroller.clientHeight) / ROW_H) + OVERSCAN);
    if (f === curFirst && l === curLast) return;
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  function resetWindow(length, scroller) {
    if (scroller) scroller.scrollTop = 0;
    const initialLast = scroller
      ? Math.min(length, Math.ceil(scroller.clientHeight / ROW_H) + OVERSCAN)
      : length;
    curFirst = 0;
    curLast = initialLast;
    firstRow = 0;
    lastRow = initialLast;
  }

  function clampWindow(length) {
    const maxFirst = Math.max(0, length - 1);
    const f = Math.min(curFirst, maxFirst);
    const l = Math.min(Math.max(curLast, f), length);
    curFirst = f;
    curLast = l;
    firstRow = f;
    lastRow = l;
  }

  /** True when the tracks currently in the window still exist in `list` —
   *  i.e. `list` is the same queue edited in place (moved/appended, or a
   *  remove that may have taken one visible row with it), not a whole new
   *  queue. Window-sized, so it is cheap on the rare full states that replace
   *  the queue array. */
  function windowAligned(prev, list) {
    const f = curFirst;
    const l = Math.min(curLast, prev.length);
    let missing = 0;
    for (let k = f; k < l; k++) {
      const uri = prev[k]?.uri;
      if (uri == null) continue;
      let found = false;
      for (let j = 0; j < list.length; j++) {
        if (list[j]?.uri === uri) {
          found = true;
          break;
        }
      }
      if (!found && ++missing > 1) return false;
    }
    return true;
  }

  /*
   * Runs before the new rows are patched into the DOM, so an edit cannot
   * briefly render a stale retained range. Edits of the same queue keep the
   * position (clamped to the new length); a queue that no longer shows the
   * visible tracks resets the shared pane scroll to the top.
   */
  $effect.pre(() => {
    const list = queue;
    const length = list.length;
    const body = bodyEl;
    if (!body) return;

    const identityChanged = list !== seenQueue;
    const lengthChanged = length !== seenLength;
    if (!identityChanged && !lengthChanged) return;

    const prev = seenQueue;
    seenQueue = list;
    seenLength = length;
    const scroller = body.closest(".scroll");
    if (identityChanged && prev && !windowAligned(prev, list)) resetWindow(length, scroller);
    else clampWindow(length);
  });

  $effect(() => {
    // Re-runs when the queue identity, its length, or the mounted body
    // changes; deliberately does not depend on firstRow/lastRow, which change
    // on every scroll frame.
    const list = queue;
    list.length;
    if (!bodyEl) return;
    const scroller = bodyEl.closest(".scroll");
    if (!scroller) {
      // No scroll ancestor (embedded use): render everything, as before.
      curFirst = 0;
      curLast = queue.length;
      firstRow = 0;
      lastRow = queue.length;
      return;
    }
    let queued = false;
    const onScroll = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        measure(scroller);
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(() => measure(scroller));
    ro.observe(scroller);
    measure(scroller);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      ro.disconnect();
    };
  });
</script>

<section class="view page">
  <div style="padding:var(--s4) 0 var(--s6)">
    <span class="tag">Up next</span>
    <h1 class="page-title">Queue</h1>
    {#if queue.length}
      <p class="detail-meta" style="margin-top:var(--s3)">
        <span class="num">{queue.length} {queue.length === 1 ? "song" : "songs"}</span>
        <span class="sep">/</span><span class="num">{formatTotal(queue)}</span>
        {#if playback.current_index >= 0}
          <span class="sep">/</span><span class="num">now playing #{playback.current_index + 1}</span>
        {/if}
      </p>
    {/if}
  </div>

  {#if queue.length}
    <!-- Same row system as every other track table; only the trailing cell
         differs, so a queue row and a playlist row line up exactly. -->
    <div class="tl queue" style="overflow-anchor: none">
      <div class="tl-head">
        <span style="text-align:right">#</span>
        <span></span>
        <span>Title</span>
        <span style="display:grid;justify-items:end"><Icon name="clock" size={13} /></span>
        <span></span>
      </div>

      <div
        bind:this={bodyEl}
        style="overflow-anchor: none; position: relative"
        style:height="{queue.length * ROW_H}px"
      >
        <div
          style="position: absolute; inset: 0 0 auto"
          style:transform="translateY({firstRow * ROW_H}px)"
        >
          <!-- Absolute-index keys retain all overlapping rows when the window
               slides. Queue action indexes remain the original queue indexes. -->
          {#each visible as track, k (firstRow + k)}
            {@const i = firstRow + k}
            <div
              class="tl-row"
              class:current={i === playback.current_index}
              role="button"
              tabindex="-1"
              ondblclick={() => playAt(i)}
            >
              <span class="c-idx">
                <span class="n">{i + 1}</span>
                <span class="eq"><i></i><i></i><i></i><i></i></span>
                <button class="go" title="Play from here" onclick={() => playAt(i)}>
                  <Icon name={i === playback.current_index && playback.playing ? "pause" : "play"} size={12} />
                </button>
              </span>

              <Cover
                src={track.cover_url}
                id={track.album_id || track.uri}
                name={track.album_name || track.name}
                size={36}
                class="c-art"
              />

              <span class="c-title">
                <span class="t-name">{track.name}</span>
                <ArtistLinks
                  class="t-artists"
                  names={track.artist_names}
                  ids={track.artist_ids ?? []}
                  id={track.artist_id}
                />
              </span>

              <span class="c-time">{formatTime(track.duration_ms)}</span>

              <span class="q-actions">
                <button
                  title="Move up"
                  disabled={i === 0}
                  onclick={() => api.moveQueue(i, i - 1).catch(() => {})}
                >
                  <Icon name="chevron-up" size={15} />
                </button>
                <button
                  title="Move down"
                  disabled={i === queue.length - 1}
                  onclick={() => api.moveQueue(i, i + 1).catch(() => {})}
                >
                  <Icon name="chevron-down" size={15} />
                </button>
                <button class="danger" title="Remove from queue" onclick={() => api.removeQueue(i).catch(() => {})}>
                  <Icon name="x" size={14} />
                </button>
              </span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  {:else}
    <div class="empty">
      <p class="h">The queue is empty.</p>
      <p class="sub">Play a playlist or add single tracks from any track menu.</p>
      <div class="actions">
        <button class="btn-ghost" onclick={() => navigate("library")}>
          <Icon name="library" size={14} />Go to your library
        </button>
      </div>
    </div>
  {/if}
</section>
