<script>
  /** Inline SVG icon set matching Spotify's glyph language. */
  let { name, size = 24 } = $props();

  const filled = $derived(FILLED.has(name));
  const inner = $derived(ICONS[name] ?? ICONS.note);
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill={filled ? "currentColor" : "none"}
  stroke={filled ? "none" : "currentColor"}
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {@html inner}
</svg>

<script module>
  const FILLED = new Set(["home", "play", "pause", "next", "previous", "heart", "heart-filled", "more"]);

  const ICONS = {
    home: `<path d="M12 3.2 3.9 9.3c-.33.24-.53.62-.53 1.03V19.6c0 .5.4.9.9.9h4.9v-5.4a2.83 2.83 0 0 1 5.66 0v5.4h4.9c.5 0 .9-.4.9-.9v-9.27c0-.41-.2-.79-.53-1.03L12 3.2Z"/>`,
    search: `<circle cx="10.6" cy="10.6" r="6.4"/><path d="m15.6 15.6 5.2 5.2"/>`,
    library: `<path d="M3.6 21.4v-8.4M8.4 21.4V5.6M13.2 21.4v-12"/>`,
    play: `<path d="M8.2 4.9v14.2c0 .93 1.03 1.46 1.8.93l10.9-7.1a1.13 1.13 0 0 0 0-1.86L10 3.97c-.77-.53-1.8 0-1.8.93Z"/>`,
    pause: `<rect x="6.3" y="4.8" width="3.7" height="14.4" rx="1.1"/><rect x="14" y="4.8" width="3.7" height="14.4" rx="1.1"/>`,
    next: `<path d="M6.2 5.6v12.8c0 .85.94 1.36 1.64.88l9.3-6.4a1.06 1.06 0 0 0 0-1.76L7.84 4.72c-.7-.48-1.64.03-1.64.88Z"/><path d="M18.6 4.8h1.7v14.4h-1.7z"/>`,
    previous: `<path d="M17.8 5.6v12.8c0 .85-.94 1.36-1.64.88l-9.3-6.4a1.06 1.06 0 0 1 0-1.76l9.3-6.4c.7-.48 1.64.03 1.64.88Z"/><path d="M3.7 4.8h1.7v14.4H3.7z"/>`,
    shuffle: `<path d="M2 18h1.4c1.3 0 2.5-.6 3.3-1.7l6.1-8.6c.8-1.1 2-1.7 3.3-1.7H22"/><path d="m18 2 4 4-4 4"/><path d="M2 6h1.9c1.5 0 2.9.9 3.6 2.2"/><path d="M22 18h-5.9c-1.3 0-2.6-.7-3.3-1.8l-.5-.8"/><path d="m18 14 4 4-4 4"/>`,
    repeat: `<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>`,
    "repeat-one": `<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1.4v4"/>`,
    heart: `<path d="M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35Z"/>`,
    "heart-filled": `<path d="M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35Z"/>`,
    more: `<circle cx="5.5" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="18.5" cy="12" r="1.7"/>`,
    settings: `<circle cx="12" cy="12" r="3.4"/><path d="M12 2.2v2.4M12 19.4v2.4M2.2 12h2.4M19.4 12h2.4M5.2 5.2l1.7 1.7M17.1 17.1l1.7 1.7M18.8 5.2l-1.7 1.7M6.9 17.1l-1.7 1.7"/>`,
    plus: `<path d="M12 5v14M5 12h14"/>`,
    queue: `<path d="M3.4 6.4h11.2v1.9H3.4zM3.4 11.2h8.2v1.9H3.4zM3.4 16h5.2v1.9H3.4z"/><path d="M16.8 9.3v5.4c0 .6.65.95 1.2.7l4.7-2.7a.8.8 0 0 0 0-1.4L18 8.6c-.55-.25-1.2.1-1.2.7Z" fill="currentColor" stroke="none"/>`,
    note: `<path d="M9.3 17.6V6.1l9.4-2.1v11.7"/><circle cx="6.8" cy="17.6" r="2.7" fill="currentColor" stroke="none"/><circle cx="16.2" cy="15.7" r="2.7" fill="currentColor" stroke="none"/>`,
    x: `<path d="M18 6 6 18M6 6l12 12"/>`,
    "chevron-up": `<path d="m18 15-6-6-6 6"/>`,
    "chevron-down": `<path d="m6 9 6 6 6-6"/>`,
    logout: `<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/>`,
    login: `<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M10 17l5-5-5-5M15 12H3"/>`,
    clock: `<circle cx="12" cy="12" r="8.5"/><path d="M12 7.4V12l3 2.6"/>`,
  };
</script>
