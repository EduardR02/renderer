<script>
  import { untrack } from "svelte";
  import {
    playback,
    ui,
    navigate,
    openCredits,
    trackCredits,
    loadTrackCredits,
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
    <!-- The artwork is the panel's subject, so it gets the full column width
         and throws a blurred copy of itself behind the top of the panel. That
         glow is the only place the interface takes colour from the music
         rather than from the palette, and it costs one composited layer. When
         the cover is missing, the generated tile supplies the same glow from
         the Rose Pine accents, so the block never looks empty. -->
    <div class="np-stage">
      <span class="np-glow" aria-hidden="true">
        <Cover src={current.cover_url} id={current.album_id || current.uri} name="" fill />
      </span>
      <div class="np-art">
        <Cover
          src={current.cover_url}
          id={current.album_id || current.uri}
          name={current.album_name || current.name}
          fill
          lg
          raised
        />
      </div>
    </div>

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
  {:else}
    <div class="np-empty">
      <p>Nothing playing</p>
      <span>Start a song to see its artwork, artists, album, and credits.</span>
    </div>
  {/if}
</aside>
