<script>
  /**
   * The Liked Songs mark. One object, every size.
   *
   * There were two of these and they were not the same picture: the rail drew
   * a flat pale rose square with a dark heart punched out of it, and the
   * collection page drew a near-black tile with a rose light in the corner and
   * a rose heart on it. Same collection, two identities, and the small one was
   * the one you saw forty times a session.
   *
   * What survives is neither of them exactly: a lit rose surface falling into
   * plum shadow. The rail's flat pastel square was candy, and the page's
   * near-black tile with a rose corner spent most of its area in the dark
   * low-chroma warm that reads as brown — the one colour this palette cannot
   * afford. Rendered side by side at 32px and 176px, this is the only version
   * that says "rose" at both.
   *
   * The heart is drawn here rather than through <Icon>, because Icon pins its
   * glyph in pixels — correct for a row of controls, wrong for a mark that has
   * to hold its proportions from 32px to 200px. Sizing it as a PERCENTAGE of
   * the tile is what makes this one mark instead of two that resemble
   * each other.
   */
  let { size = 32, fill = false, class: cls = "" } = $props();

  /* Small marks need a proportionally larger glyph or they read as a smudge
     rather than as the same mark — the generated monogram tile makes exactly
     the same trade at exactly the same crossover. */
  const small = $derived(!fill && size <= 56);
</script>

<span
  class="liked-mark {cls}"
  class:sm={small}
  style:width={fill ? "100%" : `${size}px`}
  style:height={fill ? "100%" : `${size}px`}
  role="img"
  aria-label="Liked Songs"
>
  <svg class="glyph" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
    <use href="#i-heart-f" />
  </svg>
</span>

<style>
  .liked-mark {
    position: relative; display: grid; place-items: center; flex: none;
    border-radius: var(--r3);
    /* White, not rose. The tile carries the hue — that is the whole point of
       putting rose somewhere with AREA — so the mark on it is the app's own
       near-white and stays legible at 32px and at 200px. A rose heart on a
       rose field is one object hiding inside another. */
    color: var(--fg);
    /* One hue's own ramp, lit at the top-left and falling into shadow.
       Every stop is OPAQUE. That is not a detail: a ramp built from
       `color-mix(rose N%, transparent)` over a dark ground spends most of the
       tile between 40% and 60% alpha, and 50% of a pale warm over near-black
       lands around #806968 — a dark low-chroma warm, which is the colour that
       reads as brown. Naming the dark end explicitly puts it at #5a2b31, a
       deep plum with real chroma, so the tile is rose all the way down.
       Compared side by side, this and the alpha version are not close.

       Rose is a pale, low-chroma warm (L* 84, chroma 0.054): the only way it
       registers as a hue at all is as a field, which is why the mark is a
       gradient and the glyph is not. */
    background: linear-gradient(150deg,
      color-mix(in srgb, var(--rose) 90%, #ffffff) 0%,
      var(--rose) 18%,
      var(--rose-ink) 42%,
      #5a2b31 74%,
      #1b1013 100%);
    box-shadow: var(--ring), 0 18px 40px -16px color-mix(in srgb, var(--rose) 34%, transparent);
  }
  .glyph {
    display: block; width: 25%; height: 25%;
    filter: drop-shadow(0 2px 10px rgba(0, 0, 0, 0.32));
  }
  /* Small art keeps small corners, drops the cast shadow — at 32px in a 44px
     row it only muddies the row's own hover fill — and opens the glyph up,
     because 25% of 32px is nothing at all. */
  .liked-mark.sm { border-radius: var(--r1); box-shadow: var(--ring); }
  .liked-mark.sm .glyph { width: 46%; height: 46%; filter: none; }
</style>
