<script>
  /**
   * Custom pointer-driven slider used for seek and volume.
   * Local value while dragging; `onCommit` fires on release (and on keyboard).
   */
  let {
    min = 0,
    max = 100,
    value,
    onCommit,
    label = "",
    step = null,
    className = "",
    onDragStart = null,
    onDragChange = null,
  } = $props();

  let track = $state(null);
  let drag = $state(null);
  let dragging = $derived(drag !== null);
  let display = $derived(dragging ? drag : value);

  const pct = $derived(max > min ? ((display - min) / (max - min)) * 100 : 0);

  function fromClientX(x) {
    if (!track) return min;
    const r = track.getBoundingClientRect();
    if (r.width <= 0) return min;
    const p = Math.min(1, Math.max(0, (x - r.left) / r.width));
    return min + p * (max - min);
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

  function onPointerUp(e) {
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

<div
  class="slider {className}"
  class:dragging
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
  <div class="rail">
    <div class="fill" style:width={pct + "%"}></div>
    <div class="knob" style:left={pct + "%"}></div>
  </div>
</div>

<style>
  .slider {
    position: relative;
    display: flex;
    align-items: center;
    width: 100%;
    height: 14px;
    cursor: pointer;
    touch-action: none;
    outline: none;
  }
  .rail {
    position: relative;
    width: 100%;
    height: 4px;
    border-radius: var(--radius-full);
    background: rgba(255, 255, 255, 0.25);
    transition: height var(--transition-fast), background-color var(--transition-fast);
  }
  .slider:hover .rail,
  .slider.dragging .rail {
    height: 5px;
    background: rgba(255, 255, 255, 0.35);
  }
  .slider:focus-visible .rail {
    box-shadow: 0 0 0 3px rgba(255, 255, 255, 0.25);
  }
  .fill {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    border-radius: inherit;
    background: #fff;
  }
  .knob {
    position: absolute;
    top: 50%;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.45);
    transform: translate(-50%, -50%) scale(0);
    transition: transform var(--transition-fast);
  }
  .slider:hover .knob,
  .slider.dragging .knob,
  .slider:focus-visible .knob {
    transform: translate(-50%, -50%) scale(1);
  }
</style>
