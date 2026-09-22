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
    setNowPlayingOpen,
  } from "../lib/state.svelte.js";
  import Cover from "./Cover.svelte";
  import ArtistLinks from "./ArtistLinks.svelte";
  import Icon from "./Icon.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { formatTime } from "../lib/time.js";

  const current = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const next = $derived(playback.queue[playback.upcoming?.[0]] ?? null);
  const playCountFormatter = new Intl.NumberFormat();

  const canvasTrackKey = $derived(current?.id || current?.uri || "");
  let canvasUrl = $state("");
  let canvasReady = $state(false);
  let canvasStageRatio = $state(100);
  let canvasRetiring = $state(false);
  let canvasEl = $state(null);
  let panelEl = $state(null);
  let stageEl = $state(null);
  /* Starts true: before the observer's first callback arrives, a panel that
     just opened must run its Canvas exactly as it did before this gate
     existed. */
  let stageVisible = $state(true);
  let pageVisible = $state(!document.hidden);
  let reducedMotion = $state(false);

  /* ---- Which surface the sleeve shows --------------------------------
     A Canvas is the record in motion; the cover is the record as an
     object. Clicking the sleeve asks for the other one. It is a peek at
     THIS record's artwork rather than a mode the app is switched into, so
     it is a plain local flag: nothing is stored, and the next track
     arrives showing its Canvas exactly as it always has. A track with no
     Canvas shows its cover either way and offers no control at all. */
  let preferCover = $state(false);
  /* The user's handoff, in motion. Deliberately not `canvasRetiring`: that
     one is the authoritative goodbye and ends with the source released,
     while this only lends the frame back to the cover and keeps the Canvas
     loaded, so the trip back costs nothing but the animation. */
  let coverSettling = $state(false);

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
      clearCanvas();
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
        /* A new record is a new question, so it arrives on its Canvas rather
           than inheriting the answer the last one was given. Here, where the
           source actually changes hands, and not the moment the track does:
           until this resolves the previous Canvas is still the decoded thing
           on screen, and going back to "show the Canvas" then would re-open
           THAT one for as long as this request takes. */
        /* Coming back from the cover there is no Canvas on screen to protect,
           so the incoming one may announce itself the ordinary way instead of
           inheriting the last one's readiness: the sleeve then opens on a
           frame that exists, rather than pouring over a source still loading.
           Only when the source genuinely changes — an identical url leaves
           the element holding a picture it has already decoded, and no second
           `canplay` would ever come to re-arm it.
           Tested before the release, which is what lets it read `preferCover`
           directly instead of snapshotting it first. */
        if (preferCover && canvas.url !== canvasUrl) canvasReady = false;
        releaseSurface();
        canvasUrl = canvas.url;
      })
      .catch(() => {
        if (generation !== canvasGeneration) return;
        retireCanvas();
      });
  });

  /** Hand the frame back to the Canvas, with no motion and nothing pending. */
  function releaseSurface() {
    preferCover = false;
    coverSettling = false;
  }

  /** Back to having no Canvas at all: the source, its geometry, its readiness
      and whichever surface the listener had chosen over it.

      This is one function because it used to be three copies, and the second
      concern — the chosen surface — was added by pasting `releaseSurface()`
      beside each of them. Two halves kept in step by hand across three sites
      is how the next one gets forgotten. */
  function clearCanvas() {
    canvasRetiring = false;
    releaseSurface();
    canvasUrl = "";
    canvasReady = false;
    canvasStageRatio = 100;
  }

  /* The stage is the top of a column that scrolls, so the whole 720x1280
     frame can sit outside the panel — above the credits and the up-next block
     the reader is actually on — while every other gate still says yes and the
     decoder runs with nothing on screen to show for it. This watches the
     stage inside the panel's own scrollport: leaving it pauses, coming back
     resumes, and the panel unmounting tears the observer down with it. */
  $effect(() => {
    const stage = stageEl;
    const panel = panelEl;
    if (!stage || !panel) return;
    const observer = new IntersectionObserver(
      ([entry]) => (stageVisible = Boolean(entry?.isIntersecting)),
      { root: panel },
    );
    observer.observe(stage);
    return () => observer.disconnect();
  });

  /**
   * Whether the Canvas video may run its decoder right now.
   *
   * A 720x1280 loop decoding forever is exactly the cost this app exists to
   * avoid, and the element being off screen is not enough to stop it: a
   * `<video autoplay>` in a window that merely lost focus keeps decoding every
   * frame. So playback is driven from here rather than from the `autoplay`
   * attribute, and it stops on every one of the things that mean nobody is
   * watching — the panel closed (this component unmounts), the window
   * backgrounded or minimised, the stage scrolled out of the panel, the
   * music paused, and the listener having asked for the cover art instead.
   * The paused one is not only about cost: a Canvas is the record's motion,
   * and it standing still while the record does is what the official client
   * shows too. The cover one is the plainest case of all — the frame is
   * showing a picture, so there is nothing to decode for.
   */
  const canvasPlaying = $derived(
    Boolean(canvasUrl) &&
      !canvasRetiring &&
      !preferCover &&
      stageVisible &&
      pageVisible &&
      ui.windowFocused &&
      playback.playing,
  );

  /**
   * The stage's two motion states, which are the stylesheet's own.
   *
   * `.play` means a Canvas is in the frame and `.settle` means the frame is
   * being handed back to the cover; between them they already carry the open
   * and the goodbye — the sleeve travelling along its rail, `np-pour` /
   * `np-drain`, `np-recede` / `np-forward`. The click drives exactly these
   * two flags, so the swap IS that crossfade played in one direction or the
   * other, rather than a second animation written beside it.
   *
   * `.play` is held through the user's settle so the contraction has the
   * transition it opened on, and drops when the motion reports done — by
   * which point the animations have already landed on the resting values, so
   * nothing moves when it goes.
   */
  const canvasStaged = $derived(canvasReady && (!preferCover || coverSettling));
  const canvasSettling = $derived(canvasRetiring || coverSettling);
  /**
   * A Canvas the sleeve could show: the only condition that offers a swap.
   *
   * Not during the authoritative goodbye. That motion is already using
   * `.play` and `.settle` to carry the frame home, and a click landing in the
   * middle of it would pull `.play` out from under a transition still
   * running — and it would be offering a Canvas that is on its way out
   * anyway.
   */
  const canvasSwappable = $derived(canvasReady && Boolean(canvasUrl) && !canvasRetiring);

  function swapCanvasSurface() {
    if (!canvasSwappable) return;
    /* Named for the direction it is heading. Not `next`: the component
       already has one, and it is the up-next track — a boolean shadowing a
       reactive track object is a trap laid for the next edit. */
    const toCover = !preferCover;
    /* Only the trip towards the cover has motion of its own to run. The trip
       back simply restores `.play`, and a changed animation-name is what
       re-triggers `np-pour` and `np-recede` — the same open a fresh Canvas
       gets, for free. `canvasSwappable` has already excluded a retiring
       Canvas, so there is nothing further to test for here. */
    coverSettling = toCover;
    preferCover = toCover;
  }

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
       same gates. A redundant play() on a running element resolves quietly.

       `canvasPlaying` itself, rather than its terms written out again: this
       handler used to carry its own copy of the list, and adding the cover
       gate meant editing both — which is precisely the drift a second copy
       invites. `canvasReady` is not one of its inputs, so setting it on the
       line above does not change what this reads. */
    if (canvasPlaying) canvasEl?.play()?.catch(() => {});
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
    /* The user's handoff is already running the very same motion: joining it
       costs nothing on screen and lets its completion carry the source away
       as well, instead of cutting a drain that is halfway down. */
    if (coverSettling) {
      canvasRetiring = true;
      return;
    }
    if (!canvasUrl || !canvasReady || preferCover) {
      /* Nothing was ever revealed, or the cover is already in the frame
         because the listener put it there: there is no motion to preserve,
         and a settle nobody can see would never report completion. */
      clearCanvas();
      return;
    }
    canvasRetiring = true;
  }

  /** Completion, taken from the settle motion's own events — never a timer. */
  function commitRetirement() {
    /* The listener's swap only ever borrowed the motion; the source stays. */
    coverSettling = false;
    if (!canvasRetiring) return;
    clearCanvas();
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
  bind:this={panelEl}
  aria-label="Now playing details"
  style:--tone-wash={tone.wash}
  style:--tone-glow={tone.glow}
>
  <div class="np-head">
    <span class="tag">Now playing</span>
    <button class="btn-icon" title="Close now playing details" onclick={() => setNowPlayingOpen(false)}>
      <Icon name="x" size={13} />
    </button>
  </div>

  {#if current}
    <!-- At rest this is the original inset square sleeve: rounded, raised and
         lit by its own static cover glow. A ready Canvas extends that same
         frame to the video's natural ratio; it never becomes rail chrome. -->
    <div
      class="np-stage"
      bind:this={stageEl}
      class:play={canvasStaged}
      class:settle={canvasSettling}
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
        <!-- The swap target is the whole sleeve, and it exists ONLY when
             there is a second surface to swap to: with no Canvas the picture
             is a picture, with no button over it, no hover state and no
             pointer. The absence is the answer to "can I click this?".

             It is an overlay rather than the sleeve element itself because
             the sleeve must survive this appearing and disappearing — it
             carries the open and settle transitions, and swapping its tag
             between `button` and `div` would remount it mid-motion and take
             the artwork's resolved image with it. -->
        {#if canvasSwappable}
          <button
            class="np-swap"
            type="button"
            aria-label={preferCover ? "Show the Canvas animation" : "Show the cover art"}
            onclick={swapCanvasSurface}
          >
            <!-- `tag` first: this IS the app's tag, on a surface that carries
                 its own colour, so it takes the frosted treatment from the
                 shared rule rather than restating it. Only what is genuinely
                 particular to sitting on the artwork stays local. -->
            <span class="tag np-swap-plate">
              <Icon name="swap" size={11} />
              {preferCover ? "Canvas" : "Cover"}
            </span>
          </button>
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

<style>
  /* The swap surface. It covers the whole sleeve and paints nothing: the
     Canvas plays at full strength underneath it at every moment, and at rest
     the sleeve looks exactly as it did before this existed. */
  .np-swap {
    position: absolute;
    z-index: 4;
    inset: 0;
    display: flex;
    align-items: flex-end;
    justify-content: flex-end;
    padding: var(--s3);
    appearance: none;
    border: 0;
    border-radius: inherit;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  /* Inset, because the sleeve clips: an outline at a positive offset would be
     cut off by the frame it is meant to describe. Drawn just inside the edge
     it reads as a matte around the picture. */
  .np-swap:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -5px;
    border-radius: var(--r4);
  }

  /* Frosted glass, by the app's own rule for a control that lands on a
     surface which already has a colour of its own (`.wash .tag`,
     `.np-head .tag`): it drops any palette fill and becomes white type on a
     plate that picks up whatever is behind it. A Canvas is the strongest
     case of that surface there is — it is moving colour — so the plate
     carries the record's own light, frame by frame, and needs no fill of its
     own to belong. `saturate` matters as much as `blur`: averaging a colour
     field over a 10px radius desaturates it, and without the boost the plate
     reads grey over a picture that is not. */
  /* `.tag` in the markup carries the shape, the type and — via
     `.np-stage .tag` in app.css — the glass. Restating any of it here is what
     let this drift once already: the inset ring was copied at 0.14 against
     the shared rule's 0.09, so the two frosted tags on this one rail stopped
     sharing an edge. What stays local is only what is particular to sitting
     on the artwork. */
  .np-swap-plate {
    gap: 5px;
    height: 21px;
    padding: 0 8px 0 6px;
    /* At rest there is no chrome at all, so the plate has to arrive rather
       than be uncovered: it fades on the colour beat and rises on the
       transform one, which is the app's standard pair. */
    opacity: 0;
    transform: translateY(5px);
    transition:
      opacity var(--d1) var(--ease),
      transform var(--d2) var(--ease),
      background var(--d1) var(--ease);
  }
  .np-swap:hover .np-swap-plate,
  .np-swap:focus-visible .np-swap-plate {
    opacity: 1;
    transform: none;
  }
  /* Reaching the plate itself thickens the glass and leans it towards the
     tone already taken from the record — the same colour lighting the
     sleeve's shadow and the credits rule. Mostly light rather than mostly
     hue, deliberately: a Canvas usually shares its record's palette, so a
     tint alone is invisible against exactly the backdrops this sits on,
     while glass gaining body reads on any of them. */
  .np-swap-plate:hover {
    background: color-mix(in srgb, var(--tone-glow) 22%, rgba(255, 255, 255, 0.26));
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.16);
  }
  .np-swap:active .np-swap-plate {
    transform: scale(0.97);
  }
  /* The glyph sits a shade under its own caps so the word leads. Scoped
     through :global because the mark is an <Icon>, and `.icon` carries its
     own box rules already. */
  .np-swap-plate :global(.icon) { opacity: 0.85; }

  /* Where the effect is unavailable the plate goes back to a plain lighter
     fill — it still has to carry white caps over a picture, and glass is how
     it does that beautifully, not how it does it at all. Kept local rather
     than pushed onto the shared frosted rule: every other tag wearing that
     rule sits on a wash or a portrait, and this is the only one that has to
     stay legible over moving video. `prefers-reduced-transparency` needs no
     block here at all — app.css already answers it for this selector. */
  @supports not (backdrop-filter: blur(1px)) {
    .np-swap-plate { background: rgba(255, 255, 255, 0.16); }
  }
  /* Belt and braces. Asking for reduced motion currently stops the Canvas
     being fetched at all, so there is no second surface and no plate to
     reveal — but the plate's own motion should not be the thing that has to
     be remembered if that gate is ever loosened. */
  @media (prefers-reduced-motion: reduce) {
    .np-swap-plate {
      transform: none;
      transition:
        opacity var(--d1) var(--ease),
        background var(--d1) var(--ease);
    }
    .np-swap:active .np-swap-plate { transform: none; }
  }
</style>
