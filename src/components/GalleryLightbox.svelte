<script>
  import { resolveCoverUrl } from "../lib/state.svelte.js";
  import { coverTone } from "../lib/covertone.svelte.js";
  import Icon from "./Icon.svelte";

  /**
   * The About gallery at the size the photographs were made at.
   *
   * The figure on the artist page is a FIXED frame — that is what stops
   * stepping through eighteen pictures from resizing the card and shoving
   * every section below it — so it is never the whole of any one picture at
   * its own scale. This is: one photograph on a dimmed ground, scaled down to
   * fit the window and never up past its own pixels, with the two steppers the
   * figcaption already carries so there is no second control to learn.
   *
   * It takes `index` from the page rather than keeping its own, so the picture
   * you were looking at in here is the one still in the frame when it closes.
   *
   * A native modal <dialog>, like every other overlay in this app: Escape,
   * focus containment and the inert backdrop are the platform's job and it
   * does them better than a div with a keydown handler.
   */
  let { open = false, images = [], index = 0, name = "", onStep, onClose } = $props();

  let dialog = $state(null);
  /** Resolved `cover://` urls, keyed by source. `""` records a failure, which
      is what keeps the effect below from retrying a dead url forever. */
  let resolved = $state({});
  const captionId = "gallery-lightbox-caption";

  const total = $derived(images.length);
  const position = $derived(total ? Math.min(index, total - 1) : 0);
  const shot = $derived(images[position] ?? null);
  const source = $derived(shot?.url ?? "");
  const src = $derived(resolved[source] || "");
  const width = $derived(Number(shot?.width) > 0 ? Number(shot.width) : null);
  const height = $derived(Number(shot?.height) > 0 ? Number(shot.height) : null);
  /* Something has to hold the middle of the screen while a cold picture
     resolves, and it may as well hold the right shape when the shape is
     known. A gallery with no measurements gets a square, which is the same
     neutral the inline frame falls back to. */
  const pendingAspect = $derived(width && height ? width / height : 1);
  /* The one light in the room is the picture's own. `coverTone` has already
     measured this url for the mat behind the inline frame — same key, same
     cache — so the overlay costs nothing to light and cannot disagree with the
     frame it was opened from. */
  const tone = $derived(coverTone(source, name));

  $effect(() => {
    if (!open || !total) return;
    /* The neighbours as well as the picture on screen. A step that waits on a
       round trip reads as a stall, and by the time anyone has stepped twice
       these are already on disk. */
    const wanted = [position, (position + 1) % total, (position - 1 + total) % total]
      .map((i) => images[i]?.url)
      .filter(Boolean);
    for (const url of wanted) {
      if (url in resolved) continue;
      resolveCoverUrl(url).then((local) => (resolved[url] = local || ""));
    }
  });

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      dialog.showModal();
      /* The DIALOG takes focus, not the close button: landing a focus ring on
         the one control that dismisses what you just opened is a strange first
         suggestion. Same reasoning as the credits sheet. */
      queueMicrotask(() => dialog?.focus());
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  function step(delta) {
    if (total > 1) onStep?.(delta);
  }

  function onNativeCancel(event) {
    event.preventDefault();
    onClose?.();
  }

  /* Everything that is not the photograph or a control is backdrop. The
     dialog fills the viewport and centres its one child, so "the click landed
     on the dialog itself" is exactly "the click landed beside the picture". */
  function onBackdropClick(event) {
    if (event.target === dialog) onClose?.();
  }

  function onKeydown(event) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    step(event.key === "ArrowLeft" ? -1 : 1);
  }
</script>

<dialog
  class="lightbox"
  bind:this={dialog}
  tabindex="-1"
  aria-label={`${name} — picture ${position + 1} of ${total}`}
  aria-describedby={captionId}
  oncancel={onNativeCancel}
  onclick={onBackdropClick}
  onkeydown={onKeydown}
  style:--tone-glow={tone.glow}
>
  <figure class="shot">
    {#if src}
      <!-- width/height as ATTRIBUTES, so the box exists at the right shape
           before the bytes do and the overlay does not resize under the
           reader. `width: auto` in the sheet then lets the picture take its
           own pixel size, which the max-* pair may shrink and can never
           grow. -->
      <img src={src} alt={name} {width} {height} draggable="false" decoding="async" />
    {:else}
      <span class="shot-pending" style:aspect-ratio={pendingAspect}></span>
    {/if}
    <figcaption class="shot-caption" id={captionId}>
      <span class="shot-name">{name}</span>
      {#if total > 1}
        <span class="shot-n tnum">{position + 1} / {total}</span>
      {/if}
    </figcaption>
  </figure>

  {#if total > 1}
    <button class="shot-step prev glass-plate" title="Previous picture" aria-label="Previous picture" onclick={() => step(-1)}>
      <Icon name="back" size={18} />
    </button>
    <button class="shot-step next glass-plate" title="Next picture" aria-label="Next picture" onclick={() => step(1)}>
      <Icon name="fwd" size={18} />
    </button>
  {/if}
  <button class="shot-close glass-plate" title="Close" aria-label="Close picture" onclick={() => onClose?.()}>
    <Icon name="x" size={16} />
  </button>
</dialog>

<style>
  /* A <dialog> is a centred box by default, and this one is the whole window:
     the picture is what gets centred, and every pixel around it is a target
     that closes. */
  .lightbox {
    position: fixed; inset: 0;
    place-items: center;
    width: 100vw; height: 100vh; max-width: none; max-height: none;
    margin: 0; padding: var(--s7);
    border: 0; background: none; overflow: hidden;
  }
  /* `display` belongs on the OPEN state and nowhere else. The UA hides a
     closed dialog with `dialog:not([open]) { display: none }`, and an author
     rule beats the UA sheet whatever its specificity — so declaring the grid
     on the bare class kept a CLOSED dialog painting, and this one is fixed at
     the full viewport, so it covered the page it belongs to. The rest of the
     box is harmless while hidden; only this line is load-bearing.
     The app's other two modals never set `display` at all, which is why they
     never had to learn this. */
  .lightbox[open] { display: grid; }
  /* Deeper than the credits sheet's backdrop on purpose. That one dims a page
     you are still reading around; this one is the room the light is off in. */
  .lightbox::backdrop { background: color-mix(in srgb, var(--bg-0) 92%, transparent); }
  .lightbox:focus-visible { outline: none; }

  .shot { display: flex; flex-direction: column; align-items: center; gap: var(--s3); margin: 0; }
  /* `auto` both ways is the whole rule: the picture is drawn at its own size,
     and the two maxima only ever take away. Upscaling an editorial photograph
     past its own pixels does not make it bigger, it makes it soft. */
  .shot img {
    width: auto; height: auto;
    /* 84vw, not 100%: the steppers are pinned to the window edges and a
       picture that runs under them is a picture with two buttons stamped on
       it. `.shot` is content-sized, so a percentage here would be circular
       anyway. */
    max-width: min(1600px, 84vw); max-height: 82vh;
    border-radius: var(--r3);
    /* A black shadow in a black room does nothing, which is what the single
       rgba() here amounted to: 80px of blur nobody could see. The picture is
       the only light source in this room, so it throws its OWN colour under
       itself — the same gesture the sleeve makes in the inspector, the credits
       sheet and the Top Result panel, and the same measurement the mat in the
       inline frame is painted with. The black shadow stays underneath it to
       keep an edge on a pale photograph. */
    box-shadow:
      0 32px 80px rgba(0, 0, 0, 0.8),
      0 30px 96px -26px color-mix(in srgb, var(--tone-glow) 62%, transparent);
  }
  .shot-pending {
    display: block; width: min(560px, 70vw); max-height: 82vh;
    border-radius: var(--r3);
    background:
      radial-gradient(120% 110% at 18% 8%, color-mix(in srgb, var(--fg) 7%, transparent) 0%, transparent 58%),
      linear-gradient(146deg, var(--bg-3) 0%, var(--bg-2) 100%);
  }
  /* Caption weight, not title weight. The picture is the content; this only
     says whose it is and where you are in the set. */
  .shot-caption {
    display: flex; align-items: baseline; gap: var(--s3);
    font-family: var(--font-small); font-size: var(--t-12); color: var(--fg-2);
  }
  .shot-name { color: var(--fg-1); }
  .shot-n { color: var(--fg-3); font-size: var(--t-11); }

  /* The steppers sit against the window edges rather than under the picture,
     because the picture's own edges move with every step and a control that
     moves between clicks walks out from under the cursor. They are glass
     plates (.glass-plate, in the markup), like the controls over the Canvas,
     and light from inside under the pointer. */
  .shot-step, .shot-close {
    position: absolute;
    display: grid; place-items: center;
    border-radius: var(--rf); color: var(--fg-1);
    transition: color var(--d1) var(--ease);
  }
  .shot-step::after, .shot-close::after {
    content: ""; position: absolute; inset: 0; border-radius: inherit; pointer-events: none;
    background: rgba(255, 255, 255, 0); transition: background-color var(--d1) var(--ease);
  }
  .shot-step:hover, .shot-close:hover { color: var(--fg); }
  .shot-step:hover::after, .shot-close:hover::after { background: rgba(255, 255, 255, 0.1); }
  .shot-step { top: 50%; width: 44px; height: 44px; transform: translateY(-50%); }
  .shot-step.prev { left: var(--s5); }
  .shot-step.next { right: var(--s5); }
  .shot-close { top: var(--s5); right: var(--s5); width: 34px; height: 34px; }
</style>
