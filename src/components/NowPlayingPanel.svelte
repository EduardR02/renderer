<script>
  import { untrack } from "svelte";
  import {
    playback,
    ui,
    navigate,
    openCredits,
    trackCredits,
    loadTrackCredits,
    api,
    appSettings,
  } from "../lib/state.svelte.js";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Icon from "./Icon.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { formatTime } from "../lib/time.js";

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const next = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index + 1] ?? null) : null,
  );
  const playCountFormatter = new Intl.NumberFormat();

  const canvasTrackKey = $derived(current?.id || current?.uri || "");
  let canvasUrl = $state("");
  let canvasReady = $state(false);
  let canvasStageRatio = $state(100);
  let canvasRetiring = $state(false);
  let canvasEl = $state(null);
  let pageVisible = $state(!document.hidden);
  let reducedMotion = $state(false);

  /* Canvas is an opt-in media decoder: do not even ask the engine while the
     panel is hidden, the document is backgrounded, or the user asks for
     reduced motion. The effect's generation also makes an older Tauri reply
     harmless when the queue advances before it arrives. */
  $effect(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const updateMotion = () => (reducedMotion = media.matches);
    const updateVisibility = () => (pageVisible = !document.hidden);
    updateMotion();
    updateVisibility();
    media.addEventListener?.("change", updateMotion);
    document.addEventListener("visibilitychange", updateVisibility);
    return () => {
      media.removeEventListener?.("change", updateMotion);
      document.removeEventListener("visibilitychange", updateVisibility);
    };
  });

  /* Settings is loaded on panel mount, not polled. `api` mirrors the result
     into the shared preference bit so a Settings toggle closes an existing
     Canvas immediately as well. */
  $effect(() => {
    api.getAppSettings().catch(() => {});
  });

  let canvasGeneration = 0;
  $effect(() => {
    const key = canvasTrackKey;
    const shouldFetch = Boolean(key && appSettings.animated_canvas && pageVisible && !reducedMotion);
    const generation = ++canvasGeneration;
    /* Resource-policy changes are authoritative and clear immediately —
       nothing is watching, so there is no handoff worth staging. A track
       change is different: keep the current Canvas and its geometry while
       the next request is in flight, then replace it, or retire it
       gracefully, exactly once when that request answers. */
    if (!shouldFetch) {
      canvasRetiring = false;
      canvasUrl = "";
      canvasReady = false;
      canvasStageRatio = 100;
      return;
    }
    api.browseCanvas(key)
      .then((canvas) => {
        if (generation !== canvasGeneration) return;
        if (!canvas?.url) {
          retireCanvas();
          return;
        }
        /* A newer positive supersedes a handoff already in motion: drop the
           flag first, so the retiring motion's completion event finds a
           source it is no longer allowed to clear. */
        canvasRetiring = false;
        canvasUrl = canvas.url;
      })
      .catch(() => {
        if (generation !== canvasGeneration) return;
        retireCanvas();
      });
  });

  /**
   * Whether the Canvas video may run its decoder right now.
   *
   * A 720x1280 loop decoding forever is exactly the cost this app exists to
   * avoid, and the element being off screen is not enough to stop it: a
   * `<video autoplay>` in a window that merely lost focus keeps decoding every
   * frame. So playback is driven from here rather than from the `autoplay`
   * attribute, and it stops on all three of the things that mean nobody is
   * watching — the panel closed (this component unmounts), the window
   * backgrounded or minimised, and the music paused. The last one is not only
   * about cost: a Canvas is the record's motion, and it standing still while
   * the record does is what the official client shows too.
   */
  const canvasPlaying = $derived(
    Boolean(canvasUrl) && !canvasRetiring && pageVisible && ui.windowFocused && playback.playing,
  );

  $effect(() => {
    /* Keyed on the displayed source itself, not just the gates: replacing
       the src while playback never stopped reloads the element and parks
       it, so the replacement has to be started again explicitly. */
    const url = canvasUrl;
    const video = canvasEl;
    if (!video || !url) return;
    if (canvasPlaying) video.play().catch(() => {});
    else video.pause();
  });

  function handleCanvasReady() {
    /* Readiness carries the frame's true dimensions: write the natural ratio
       into --stage-open as a padding-top percentage against the rail width.
       The box adopts the ratio and the square-to-ratio growth is what pushes
       the blocks below down through normal flow.
       The ratio is used raw. An earlier pass snapped this target to whole
       device pixels to chase the sideways shimmy; the shimmy was the video's
       `object-fit: contain` width tracking the animating height, which the
       stylesheet now removes, and snapping actively hurts here — the video is
       width-led, so its natural height is what the sleeve must end on, and a
       rounded target would leave a sliver of backing showing under it. */
    const w = canvasEl?.videoWidth ?? 0;
    const h = canvasEl?.videoHeight ?? 0;
    if (w > 0 && h > 0) canvasStageRatio = (h / w) * 100;
    canvasReady = true;
    /* Source replacement lands here too, with playback still running: the
       effect above already restarted the swapped element, and this is the
       moment a start can actually succeed, so re-arm it under exactly the
       same gates. A redundant play() on a running element resolves quietly. */
    if (!canvasRetiring && pageVisible && ui.windowFocused && playback.playing) {
      canvasEl?.play()?.catch(() => {});
    }
  }

  function handleCanvasError(event) {
    /* Swapping the src aborts the previous load, and that abort surfaces
       here as MEDIA_ERR_ABORTED — noise about a source already gone, not a
       verdict on whatever is displayed now. */
    if (event?.target?.error?.code === MediaError.MEDIA_ERR_ABORTED) return;
    retireCanvas();
  }

  /**
   * An authoritative null/error sends the displayed Canvas home without a
   * snap: the frame stays mounted and paused while CSS carries it back —
   * the sleeve contracts along the rail it opened on, the cover steps
   * forward out of its recede, the video drains away — and the URL and the
   * element are released only when that motion reports completion.
   * Interrupting it (a newer positive, another track) drops the flag, so a
   * stale completion event finds nothing it may clear.
   */
  function retireCanvas() {
    if (canvasRetiring) return;
    if (!canvasUrl || !canvasReady) {
      /* Nothing was ever revealed: there is no motion to preserve. */
      canvasUrl = "";
      canvasReady = false;
      canvasStageRatio = 100;
      return;
    }
    canvasRetiring = true;
  }

  /** Completion, taken from the settle motion's own events — never a timer. */
  function commitRetirement() {
    if (!canvasRetiring) return;
    canvasRetiring = false;
    canvasUrl = "";
    canvasReady = false;
    canvasStageRatio = 100;
  }

  /** The sleeve finishing its contraction is the end of the handoff. */
  function handleStageSettled(event) {
    if (event.propertyName === "padding-top" && event.pseudoElement === "::before") {
      commitRetirement();
    }
  }

  /** Fallback completion signal alongside the padding transition. */
  function handleCanvasDrained(event) {
    if (event.animationName === "np-drain") commitRetirement();
  }

  /* Credits are content in this panel, not a destination, so they load with
     the track. Only ever while the panel is mounted — it is opt-in — and the
     store caches per track id, so scrubbing back and forth costs one request. */
  $effect(() => {
    const track = current;
    if (track) untrack(() => loadTrackCredits(track));
  });

  /** Groups worth showing inline; the rest live behind "all credits". */
  const PANEL_GROUPS = 3;
  const groups = $derived(trackCredits.data?.groups ?? []);
  const shownGroups = $derived(groups.slice(0, PANEL_GROUPS));
  const contributorTotal = $derived(
    groups.reduce((sum, g) => sum + (g.contributors?.length ?? 0), 0),
  );
  /** How many names each group shows before it starts counting the remainder. */
  const PANEL_NAMES = 4;

  /**
   * The panel's colour, taken from the record that is playing.
   *
   * This rail was the flattest thing in the app: chrome grey top to bottom,
   * with the artwork the only thing in it that was not a shade of the same
   * dark. It is also the one surface that always has a picture in it, so it is
   * the surface with the least excuse for being grey. The tone drives the head
   * band, the artwork's cast shadow and the credits rule, so the whole column
   * shifts hue every time the track does — which is the reason it exists.
   */
  const tone = $derived(coverTone(current?.cover_url ?? "", current?.album_id || current?.uri || ""));
</script>

<aside
  class="np-panel"
  aria-label="Now playing details"
  style:--tone-wash={tone.wash}
  style:--tone-glow={tone.glow}
>
  <div class="np-head">
    <span class="tag">Now playing</span>
    <button class="btn-icon" title="Close now playing details" onclick={() => (ui.nowPlayingOpen = false)}>
      <Icon name="x" size={13} />
    </button>
  </div>

  {#if current}
    <!-- At rest this is the original inset square sleeve: rounded, raised and
         lit by its own static cover glow. A ready Canvas extends that same
         frame to the video's natural ratio; it never becomes rail chrome. -->
    <div
      class="np-stage"
      class:play={canvasReady}
      class:settle={canvasRetiring}
      style:--stage-open={`${canvasStageRatio.toFixed(4)}%`}
    >
      <span class="np-glow" aria-hidden="true">
        <Cover src={current.cover_url} id={current.album_id || current.uri} name="" fill />
      </span>
      <div class="np-art" ontransitionend={handleStageSettled}>
        <Cover
          src={current.cover_url}
          id={current.album_id || current.uri}
          name={current.album_name || current.name}
          fill
          lg
        />
        {#if canvasUrl}
          <video
            class="np-canvas"
            bind:this={canvasEl}
            src={canvasUrl}
            muted
            loop
            playsinline
            preload="auto"
            aria-label={`Canvas animation for ${current.name}`}
            oncanplay={handleCanvasReady}
            onerror={handleCanvasError}
            onanimationend={handleCanvasDrained}
          ></video>
        {/if}
      </div>
    </div>
  {#key current?.id}
    <div class="np-block np-identity">
      <h2>{current.name}</h2>
      <ArtistLinks
        class="np-artists"
        names={current.artist_names}
        ids={current.artist_ids ?? []}
        id={current.artist_id}
      />
      <p class="np-meta">
        {#if current.duration_ms}<span class="tnum">{formatTime(current.duration_ms)}</span>{/if}
        {#if current.duration_ms && current.play_count}<span class="np-dot" aria-hidden="true"></span>{/if}
        {#if current.play_count}<span class="tnum">{playCountFormatter.format(current.play_count)}</span> plays{/if}
      </p>
    </div>

    {#if current.album_id || current.album_name}
      <button
        class="np-album"
        disabled={!current.album_id}
        onclick={() => current.album_id && navigate("album", current.album_id)}
      >
        <Cover src={current.cover_url} id={current.album_id || current.uri} name={current.album_name || ""} size={34} />
        <span class="np-album-copy">
          <span class="np-label">Album</span>
          <span class="np-album-name">{current.album_name || "Unknown album"}</span>
        </span>
        {#if current.album_id}<Icon name="fwd" size={13} />{/if}
      </button>
    {/if}

    <!-- Credits, in the panel. The full contributor list can run to a hundred
         names, so this shows the shape of it — every group, the first few
         names in each — and hands the rest to the dialog. -->
    <!-- The credits block is GOLD, which is the whole answer to this panel
         reading grey. Gold is the app's "who made it" hue and it has real
         chroma, so it survives being set as 11px tracked caps — which is
         exactly what rose could not do, and why every micro-label in the app
         ended up neutral in the first place. -->
    <section class="np-block np-credits">
      <div class="np-section-head">
        <span class="tag credit">Credits</span>
        {#if contributorTotal}<span class="np-count tnum">{contributorTotal}</span>{/if}
      </div>

      {#if trackCredits.loading}
        <div class="np-credit-line" aria-label="Loading credits">
          <span class="skeleton line sm"></span>
          <span class="skeleton line"></span>
        </div>
      {:else if trackCredits.error}
        <p class="np-muted">Credits unavailable.</p>
      {:else if shownGroups.length}
        {#each shownGroups as group, groupIndex (`${group.title}-${groupIndex}`)}
          {@const people = group.contributors ?? []}
          {@const shown = people.slice(0, PANEL_NAMES)}
          <div class="np-credit-line">
            <span class="np-role">{group.title}</span>
            <p>
              {shown.map((c) => c.name).join(", ")}{#if people.length > shown.length}<span class="np-more-inline"
                >&nbsp;+{people.length - shown.length}</span
              >{/if}
            </p>
          </div>
        {/each}
        {#if groups.length > PANEL_GROUPS || contributorTotal > shownGroups.reduce((n, g) => n + Math.min(PANEL_NAMES, g.contributors?.length ?? 0), 0)}
          <button class="np-link credit" onclick={() => openCredits(current)}>
            All {contributorTotal} credits<Icon name="fwd" size={12} />
          </button>
        {:else}
          <button class="np-link credit" onclick={() => openCredits(current)}>
            Full credits<Icon name="fwd" size={12} />
          </button>
        {/if}
      {:else}
        <p class="np-muted">No contributors listed for this track.</p>
      {/if}
    </section>

    {#if next}
      <section class="np-block np-upnext">
        <div class="np-section-head">
          <!-- A plain field label, not a tag. Two coloured tags in a 336px
               column is a rhythm; three is a stripe. -->
          <h3 class="caps">Up next</h3>
          <button class="np-link" onclick={() => navigate("queue")}>Queue<Icon name="fwd" size={12} /></button>
        </div>
        <button class="np-next" onclick={() => navigate("queue")}>
          <Cover src={next.cover_url} id={next.album_id || next.uri} name={next.name} size={38} />
          <span class="np-next-copy">
            <strong>{next.name}</strong>
            <span>{(next.artist_names ?? []).join(", ")}</span>
          </span>
        </button>
      </section>
    {/if}
  {/key}
  {:else}
    <div class="np-empty">
      <p>Nothing playing</p>
      <span>Start a song to see its artwork, artists, album, and credits.</span>
    </div>
  {/if}
</aside>
