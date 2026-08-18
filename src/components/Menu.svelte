<script>
  /** Anchored popover menu. Trigger content via the default `children` snippet. */
  let { children, items = [], variant = "plain", width = 220 } = $props();

  let open = $state(false);
  let pos = $state({ left: 0, top: 0 });

  function toggle(e) {
    if (open) {
      open = false;
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    pos = {
      left: Math.max(8, Math.min(r.left, window.innerWidth - width - 8)),
      top: r.bottom + 6,
    };
    open = true;
  }

  $effect(() => {
    if (!open) return;
    const onDown = (e) => {
      const el = e.target;
      if (el && typeof el.closest === "function" && el.closest(".menu-anchor, .menu")) return;
      open = false;
    };
    const onKey = (e) => {
      if (e.key === "Escape") open = false;
    };
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  });

  function pick(it) {
    open = false;
    it.action?.();
  }
</script>

<div class="menu-anchor">
  <button
    class="menu-trigger {variant === 'dots' ? 'dots' : ''}"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    {@render children?.()}
  </button>
  {#if open}
    <div class="menu" role="menu" style:left={pos.left + "px"} style:top={pos.top + "px"} style:width={width + "px"}>
      {#each items as it}
        <button
          class="menu-item"
          class:danger={it.danger}
          role="menuitem"
          disabled={it.disabled}
          onclick={() => pick(it)}
        >
          <span class="label">{it.label}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .menu-anchor {
    position: relative;
    display: inline-flex;
  }
  .menu-trigger {
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .menu-trigger.dots {
    width: 44px;
    height: 44px;
    border-radius: var(--radius-full);
    color: var(--text-secondary);
    transition: color var(--transition-fast), background-color var(--transition-fast);
  }
  .menu-trigger.dots:hover {
    color: var(--text-primary);
    background: rgba(255, 255, 255, 0.08);
  }
  .menu {
    position: fixed;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    padding: 4px;
    border-radius: var(--radius-sm);
    background: var(--bg-menu);
    box-shadow: 0 16px 24px rgba(0, 0, 0, 0.6), 0 2px 8px rgba(0, 0, 0, 0.4);
  }
  .menu-item {
    display: flex;
    align-items: center;
    height: 36px;
    padding: 0 12px;
    border-radius: 2px;
    font-size: var(--font-md);
    color: var(--text-primary);
    text-align: left;
    white-space: nowrap;
    transition: background-color var(--transition-fast);
  }
  .menu-item:hover:not(:disabled) {
    background: var(--bg-menu-hover);
  }
  .menu-item.danger {
    color: var(--danger);
  }
  .menu-item .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
