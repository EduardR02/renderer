<script>
  /**
   * A listbox that belongs to the design system.
   *
   * A native <select> on a dark theme paints its button and — unavoidably —
   * its popup with OS chrome: light grey on Windows, its own font, its own
   * radius, ignoring every token in the app. This is the smallest replacement
   * that keeps what the native control is actually good at: it is a real
   * button, it is labelled, it moves with the arrow keys, Escape and Tab close
   * it, and the selected option is announced.
   *
   * Deliberately not a general combobox — no typeahead, no async, no portal.
   * The app has one dropdown and it has five options.
   */
  import Icon from "./Icon.svelte";

  let {
    options = [],
    value = null,
    onchange = () => {},
    disabled = false,
    label = "",
    id = `select-${Math.random().toString(36).slice(2, 8)}`,
  } = $props();

  let open = $state(false);
  let active = $state(0);
  let root = $state(null);
  let button = $state(null);

  const index = $derived(Math.max(0, options.findIndex((o) => o.value === value)));
  const current = $derived(options[index] ?? options[0] ?? { label: "", value: null });

  function toggle() {
    if (disabled) return;
    open = !open;
    if (open) active = index;
  }

  function pick(option) {
    open = false;
    button?.focus();
    if (option.value !== value) onchange(option.value);
  }

  function onKeyDown(event) {
    if (disabled) return;
    const { key } = event;
    if (!open) {
      if (key === "ArrowDown" || key === "ArrowUp" || key === " " || key === "Enter") {
        event.preventDefault();
        open = true;
        active = index;
      }
      return;
    }
    if (key === "Escape") {
      event.preventDefault();
      event.stopPropagation(); // never let a dropdown dismissal close a dialog too
      open = false;
      button?.focus();
    } else if (key === "ArrowDown") {
      event.preventDefault();
      active = (active + 1) % options.length;
    } else if (key === "ArrowUp") {
      event.preventDefault();
      active = (active - 1 + options.length) % options.length;
    } else if (key === "Home") {
      event.preventDefault();
      active = 0;
    } else if (key === "End") {
      event.preventDefault();
      active = options.length - 1;
    } else if (key === "Enter" || key === " ") {
      event.preventDefault();
      pick(options[active]);
    } else if (key === "Tab") {
      open = false;
    }
  }

  // Pointer-down rather than click: a click that lands on another control
  // should dismiss the list *and* reach that control, in that order.
  $effect(() => {
    if (!open) return;
    function onDocDown(event) {
      if (!root?.contains(event.target)) open = false;
    }
    document.addEventListener("pointerdown", onDocDown, true);
    return () => document.removeEventListener("pointerdown", onDocDown, true);
  });
</script>

<div class="sel" bind:this={root}>
  <!-- The ARIA 1.2 select-only combobox: focus never enters the list, so the
       button owns every key and points at the highlighted option by id. -->
  <button
    class="sel-btn"
    class:open
    bind:this={button}
    type="button"
    {id}
    {disabled}
    role="combobox"
    aria-haspopup="listbox"
    aria-controls="{id}-list"
    aria-expanded={open}
    aria-activedescendant={open ? `${id}-opt-${active}` : undefined}
    aria-label={label || undefined}
    onclick={toggle}
    onkeydown={onKeyDown}
  >
    <span class="sel-value">{current.label}</span>
    <Icon name={open ? "chevron-up" : "chevron-down"} size={13} />
  </button>

  {#if open}
    <ul class="sel-list" id="{id}-list" role="listbox" aria-label={label || undefined}>
      {#each options as option, i (option.value)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <li
          id="{id}-opt-{i}"
          role="option"
          aria-selected={option.value === value}
          class:active={i === active}
          class:selected={option.value === value}
          onpointerenter={() => (active = i)}
          onclick={() => pick(option)}
        >
          <span>{option.label}</span>
          {#if option.value === value}<Icon name="check" size={13} />{/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>
