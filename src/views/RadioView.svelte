<script>
  import {
    detail,
    playback,
    togglePlay,
    api,
    retryDetail,
    ui,
  } from "../lib/state.svelte.js";
  import TrackList from "../components/TrackList.svelte";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { coverTone } from "../lib/covertone.svelte.js";
  import { detailArtSize } from "../lib/layout.js";
  import { formatTotal } from "../lib/time.js";

  const radio = $derived(detail.radio);
  const seed = $derived(radio?.seed ?? null);
  const tracks = $derived(radio?.tracks ?? []);
  const title = $derived(seed?.name ? `${seed.name} Radio` : "Song Radio");
  const tone = $derived(coverTone(seed?.cover_url ?? "", seed?.id ?? "radio"));
  const artSize = $derived(detailArtSize(ui.paneWidth));

  const playingThis = $derived.by(() => {
    if (!tracks.length || playback.queue.length !== tracks.length) return false;
    return tracks.every((track, index) => playback.queue[index]?.uri === track.uri);
  });

  function playFrom(index) {
    if (!tracks[index]) return;
    api.playQueue(tracks, index).catch(() => {});
  }

  function playOrToggle() {
    if (playingThis) {
      togglePlay();
      return;
    }
    if (tracks.length) api.playQueue(tracks, 0).catch(() => {});
  }

  function shufflePlay() {
    if (!tracks.length) return;
    api.setShuffle(true).catch(() => {});
    api.playQueue(tracks, 0).catch(() => {});
  }
</script>

<section
  class="view page wash"
  style:--tone-wash={tone.wash}
  style:--tone-wash-deep={tone.washDeep}
  style:--tone-glow={tone.glow}
>
  {#if detail.error && !radio}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="tag">Song Radio</span>
        <h1 class="detail-title">Unavailable</h1>
      </div>
    </header>
    <div class="empty failed">
      <p class="h">This song radio could not be loaded.</p>
      <p class="why">{detail.error}</p>
      <div class="actions">
        <button class="btn-ghost" onclick={retryDetail}>Try again</button>
      </div>
    </div>
  {:else if !radio}
    <header class="detail-head">
      <span class="art lg skeleton" style:width="{artSize}px" style:height="{artSize}px"></span>
      <div>
        <span class="skeleton line sm" style="width:84px;height:19px;border-radius:var(--rf)"></span>
        <span class="skeleton line lg" style="height:46px;width:min(440px,72%)"></span>
        <span class="skeleton line sm" style="width:180px"></span>
      </div>
    </header>
    <div class="tl" style="margin-top:var(--s6);--cols:28px 36px minmax(0,1fr) 52px" aria-hidden="true">
      {#each Array.from({ length: 7 }) as _, i (i)}
        <div class="sk-row">
          <span class="sk" style="width:12px"></span>
          <span class="sk art"></span>
          <span class="sk-stack">
            <span class="sk a" style="width:{64 - ((i * 7) % 24)}%"></span>
            <span class="sk b" style="width:{32 - ((i * 5) % 11)}%"></span>
          </span>
          <span class="sk" style="width:28px;justify-self:end"></span>
        </div>
      {/each}
    </div>
  {:else}
    <header class="detail-head">
      <Cover
        src={seed.cover_url}
        id={seed.album_id || seed.id}
        name={seed.album_name || seed.name}
        size={artSize}
        lg
        raised
      />
      <div>
        <span class="tag">Song Radio</span>
        <h1 class="detail-title">{title}</h1>
        <p class="detail-meta">
          <span class="who">Spotify</span>
          <span class="sep">/</span><span class="num">{tracks.length} songs</span>
          {#if tracks.length}
            <span class="sep">/</span><span class="num">{formatTotal(tracks)}</span>
          {/if}
        </p>
        <div class="actions">
          <button
            class="play-lg"
            title={playingThis ? (playback.playing ? "Pause" : "Resume") : "Play"}
            onclick={playOrToggle}
            disabled={!tracks.length}
          >
            <Icon name={playingThis && playback.playing ? "pause" : "play"} size={19} />
          </button>
          <button class="btn-ghost" onclick={shufflePlay} disabled={!tracks.length}>
            <Icon name="shuffle" size={14} />Shuffle
          </button>
        </div>
      </div>
    </header>

    {#if tracks.length}
      <div style="margin-top:var(--s6)">
        <TrackList {tracks} {playFrom} showArtist />
      </div>
    {:else}
      <div class="empty"><p>No recommendations are available for this song.</p></div>
    {/if}
  {/if}
</section>
