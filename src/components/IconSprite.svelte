<!--
  Every icon defined once as an SVG <symbol>; components reference them
  with <use href="#i-name">. Rendered once at the app root. Inlining the
  paths per instance would multiply DOM nodes across a virtualized list
  of hundreds of rows, each carrying three or four icons.
-->

<!-- Taken out of flow AND zero-sized AND overflow-clipped: the sprite host
     itself can never contribute a pixel of layout, whatever it contains. -->
<svg
  width="0"
  height="0"
  style="position:absolute;left:0;top:0;width:0;height:0;overflow:hidden"
  aria-hidden="true"
><defs>
<symbol id="i-home" viewBox="0 0 24 24"><path fill="currentColor" d="M12 3.4 4.2 9.3c-.3.2-.5.6-.5 1V19c0 .5.4.9.9.9h4.3v-4.8a3.1 3.1 0 0 1 6.2 0v4.8h4.3c.5 0 .9-.4.9-.9v-8.7c0-.4-.2-.8-.5-1L12 3.4Z"/></symbol>
<symbol id="i-search" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><circle cx="10.6" cy="10.6" r="6.4"/><path d="m15.6 15.6 5 5"/></symbol>
<symbol id="i-queue" viewBox="0 0 24 24" fill="currentColor"><rect x="3.4" y="6.2" width="11" height="1.9" rx=".9"/><rect x="3.4" y="11.1" width="8" height="1.9" rx=".9"/><rect x="3.4" y="16" width="5" height="1.9" rx=".9"/><path d="M16.8 9.3v5.4c0 .6.6 1 1.2.7l4.7-2.7a.8.8 0 0 0 0-1.4L18 8.6c-.6-.3-1.2.1-1.2.7Z"/></symbol>
<symbol id="i-settings" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="3.2"/><path d="M12 2.6v2.2M12 19.2v2.2M2.6 12h2.2M19.2 12h2.2M5.4 5.4l1.6 1.6M17 17l1.6 1.6M18.6 5.4 17 7M7 17l-1.6 1.6"/></symbol>
<symbol id="i-plus" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M12 5.5v13M5.5 12h13"/></symbol>
<symbol id="i-play" viewBox="0 0 24 24"><path fill="currentColor" d="M7.8 5.1v13.8c0 .9 1 1.4 1.7.9L20 12.9a1.1 1.1 0 0 0 0-1.8L9.5 4.2c-.7-.5-1.7 0-1.7.9Z"/></symbol>
<symbol id="i-pause" viewBox="0 0 24 24" fill="currentColor"><rect x="6.4" y="4.9" width="3.6" height="14.2" rx="1.1"/><rect x="14" y="4.9" width="3.6" height="14.2" rx="1.1"/></symbol>
<symbol id="i-next" viewBox="0 0 24 24" fill="currentColor"><path d="M6.2 5.7v12.6c0 .8.9 1.3 1.6.9l9.2-6.3a1 1 0 0 0 0-1.8L7.8 4.8c-.7-.5-1.6 0-1.6.9Z"/><rect x="18.6" y="4.9" width="1.8" height="14.2" rx=".9"/></symbol>
<symbol id="i-prev" viewBox="0 0 24 24" fill="currentColor"><path d="M17.8 5.7v12.6c0 .8-.9 1.3-1.6.9l-9.2-6.3a1 1 0 0 1 0-1.8l9.2-6.3c.7-.5 1.6 0 1.6.9Z"/><rect x="3.6" y="4.9" width="1.8" height="14.2" rx=".9"/></symbol>
<symbol id="i-shuffle" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 18h1.4c1.3 0 2.5-.6 3.2-1.7l6-8.6c.8-1.1 2-1.7 3.3-1.7h5"/><path d="m18 2.6 3.6 3.4-3.6 3.4"/><path d="M2.5 6h1.8c1.4 0 2.7.9 3.4 2.1"/><path d="M21.5 18h-5.3c-1.3 0-2.5-.7-3.2-1.8l-.5-.7"/><path d="m18 14.6 3.6 3.4-3.6 3.4"/></symbol>
<symbol id="i-repeat" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m17 2.6 3.6 3.4L17 9.4"/><path d="M3.4 11v-1a4 4 0 0 1 4-4h13.2"/><path d="m7 21.4-3.6-3.4L7 14.6"/><path d="M20.6 13v1a4 4 0 0 1-4 4H3.4"/></symbol>
<symbol id="i-heart" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M12 20.6l-1.3-1.2C6 15.1 3 12.3 3 8.9 3 6.2 5.1 4 7.8 4c1.5 0 3 .7 4.2 2 1.1-1.3 2.6-2 4.2-2C18.9 4 21 6.2 21 8.9c0 3.4-3 6.2-7.7 10.5L12 20.6Z"/></symbol>
<symbol id="i-heart-f" viewBox="0 0 24 24" fill="currentColor"><path d="M12 20.6l-1.3-1.2C6 15.1 3 12.3 3 8.9 3 6.2 5.1 4 7.8 4c1.5 0 3 .7 4.2 2 1.1-1.3 2.6-2 4.2-2C18.9 4 21 6.2 21 8.9c0 3.4-3 6.2-7.7 10.5L12 20.6Z"/></symbol>
<symbol id="i-more" viewBox="0 0 24 24" fill="currentColor"><circle cx="5.4" cy="12" r="1.7"/><circle cx="12" cy="12" r="1.7"/><circle cx="18.6" cy="12" r="1.7"/></symbol>
<symbol id="i-back" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m14.5 5-7 7 7 7"/></symbol>
<symbol id="i-fwd" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m9.5 5 7 7-7 7"/></symbol>
<symbol id="i-clock" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="8.4"/><path d="M12 7.4V12l3 2.5"/></symbol>
<symbol id="i-vol" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5.2 6.6 8.9H3.4v6.2h3.2L11 18.8V5.2Z" fill="currentColor"/><path d="M15.3 9.3a3.8 3.8 0 0 1 0 5.4"/><path d="M18.1 6.5a7.7 7.7 0 0 1 0 11"/></symbol>
<symbol id="i-x" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M17.5 6.5 6.5 17.5M6.5 6.5l11 11"/></symbol>
<symbol id="i-note" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><path d="M9.3 17.6V6.1l9.4-2.1v11.7"/><circle cx="6.8" cy="17.6" r="2.6" fill="currentColor" stroke="none"/><circle cx="16.2" cy="15.7" r="2.6" fill="currentColor" stroke="none"/></symbol>
<symbol id="i-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7"/></symbol>
<symbol id="i-library" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M3.6 21v-8.2M8.4 21V5.8M13.2 21V9.2"/><path d="M17.4 8.6l3.4 11.4"/></symbol>
<symbol id="i-repeat-one" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m17 2.6 3.6 3.4L17 9.4"/><path d="M3.4 11v-1a4 4 0 0 1 4-4h13.2"/><path d="m7 21.4-3.6-3.4L7 14.6"/><path d="M20.6 13v1a4 4 0 0 1-4 4H3.4"/><path d="M11.2 10.4h1.2v4"/></symbol>
<symbol id="i-chevron-up" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m18 15-6-6-6 6"/></symbol>
<symbol id="i-chevron-down" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></symbol>
<symbol id="i-logout" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/></symbol>
<symbol id="i-login" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M10 17l5-5-5-5M15 12H3"/></symbol>
</defs></svg>
