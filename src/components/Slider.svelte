<script>
  /**
   * Pointer-driven slider for seek and volume. Holds a local value while
   * dragging and reports on release, so the playhead does not fight the drag.
   *
   * The fill is driven by `--p` (0..1) and animated with `transform: scaleX`
   * rather than `width: %`. Width would force layout and paint on every
   * playhead tick, forever, which is exactly the cost this app exists to
   * avoid; a transform stays on the compositor.
   */
  let {
    min = 0,
    max = 100,
    value,
    onCommit,
    label = "",
    step = null,
    kind = "seek",
    onDragStart = null,
    onDragChange = null,
  } = $props();

  let track = $state(null);
  let drag = $state(null);
  const display = $derived(drag !== null ? drag : value);
  const p = $derived(max > min ? Math.min(1, Math.max(0, (display - min) / (max - min))) : 0);

  function fromClientX(x) {
    if (!track) return min;
    const r = track.getBoundingClientRect();
    if (r.width <= 0) return min;
    return min + Math.min(1, Math.max(0, (x - r.left) / r.width)) * (max - min);
  }

  function commit(v) {
    onCommit(Math.min(max, Math.max(min, v)));
  }

  function onPointerDown(e) {
    e.preventDefault();
    drag = fromClientX(e.clientX);
    onDragStart?.(drag);
    track.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e) {
    if (drag === null) return;
    drag = fromClientX(e.clientX);
    onDragChange?.(drag);
  }

  function onPointerUp() {
    if (drag === null) return;
    const v = drag;
    drag = null;
    commit(v);
  }

  function onKeyDown(e) {
    const span = max - min;
    const small = step ?? Math.max(1, span / 20);
    const big = span / 5;
    let next = null;
    if (e.key === "ArrowRight") next = display + small;
    else if (e.key === "ArrowLeft") next = display - small;
    else if (e.key === "PageUp") next = display + big;
    else if (e.key === "PageDown") next = display - big;
    else if (e.key === "Home") next = min;
    else if (e.key === "End") next = max;
    if (next !== null) {
      e.preventDefault();
      commit(next);
    }
  }
</script>

<span
  class={kind === "vol" ? "vol" : "rail-hit"}
  role="slider"
  tabindex="0"
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={Math.round(display)}
  bind:this={track}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  onkeydown={onKeyDown}
>
  {#if kind === "vol"}
    <span class="vol-rail"><span class="vol-fill" style:--p={p}></span></span>
  {:else}
    <span class="rail"><span class="rail-fill" style:--p={p}></span></span>
    <span class="rail-knob" style:--pl="{p * 100}%"></span>
  {/if}
</span>
