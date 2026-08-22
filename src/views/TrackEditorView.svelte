<script>
  import { untrack } from "svelte";
  import { api, route, trackEditor, goBack } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import { formatTime } from "../lib/time.js";

  const track = $derived(trackEditor.track?.id === route.id ? trackEditor.track : null);
  const playlistId = $derived(trackEditor.playlistId || route.param || null);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state("");
  let cuts = $state([]);
  let loopRange = $state(null);
  let enabled = $state(false);
  let definitionExists = $state(false);
  let waveform = $state([]);
  let waveformError = $state("");

  const duration = $derived(Math.max(1, Number(track?.duration_ms) || 1));
  const timelineStyle = (range) =>
    `left:${(range.start_ms / duration) * 100}%;width:${((range.end_ms - range.start_ms) / duration) * 100}%`;

  function seconds(ms) {
    return Math.round((Number(ms) / 1000) * 100) / 100;
  }

  function millis(value) {
    return Math.max(0, Math.min(duration, Math.round((Number(value) || 0) * 1000)));
  }

  function setCut(index, field, value) {
    const next = cuts[index];
    if (!next) return;
    next[field] = Math.max(0, Math.min(duration, Math.round(Number(value) || 0)));
  }

  function addCut() {
    const span = Math.min(10_000, Math.max(1_000, Math.round(duration / 10)));
    const start = cuts.length ? Math.min(duration - 1, cuts[cuts.length - 1].end_ms) : 0;
    cuts.push({ start_ms: start, end_ms: Math.min(duration, start + span) });
  }

  function removeCut(index) {
    cuts.splice(index, 1);
  }

  function toggleLoop() {
    loopRange = loopRange
      ? null
      : { start_ms: 0, end_ms: Math.min(duration, Math.max(1_000, Math.round(duration / 4))) };
  }

  async function save() {
    if (!track || saving) return;
    saving = true;
    error = "";
    try {
      const definition = await api.saveTrackEdit(track.id, duration, cuts, loopRange);
      cuts = (definition?.cuts ?? []).map((range) => ({ ...range }));
      loopRange = definition?.loop_range ? { ...definition.loop_range } : null;
      definitionExists = true;
      if (playlistId && enabled) {
        await api.setPlaylistTrackEditEnabled(playlistId, track.id, true);
      }
    } catch (reason) {
      error = String(reason || "Could not save this edit.");
    } finally {
      saving = false;
    }
  }

  async function removeDefinition() {
    if (!track || saving) return;
    saving = true;
    error = "";
    try {
      await api.deleteTrackEdit(track.id);
      cuts = [];
      loopRange = null;
      definitionExists = false;
      enabled = false;
    } catch (reason) {
      error = String(reason || "Could not remove this edit.");
    } finally {
      saving = false;
    }
  }

  async function setEnabled(value) {
    if (!playlistId || !track || saving) return;
    saving = true;
    error = "";
    try {
      await api.setPlaylistTrackEditEnabled(playlistId, track.id, value);
      enabled = value;
    } catch (reason) {
      error = String(reason || "Could not update this playlist.");
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    const id = track?.id;
    if (!id) {
      loading = false;
      return;
    }
    loading = true;
    error = "";
    waveform = [];
    waveformError = "";
    const sourcePlaylist = playlistId;
    untrack(() => {
      Promise.all([
        api.getTrackEdit(id, sourcePlaylist),
        api.extractTrackWaveform(id, 768).catch((reason) => {
          waveformError = String(reason || "Waveform unavailable.");
          return null;
        }),
      ])
        .then(([status, decoded]) => {
          if (track?.id !== id) return;
          cuts = (status?.definition?.cuts ?? []).map((range) => ({ ...range }));
          loopRange = status?.definition?.loop_range ? { ...status.definition.loop_range } : null;
          definitionExists = !!status?.definition;
          enabled = !!status?.enabled;
          waveform = decoded?.peaks ?? [];
        })
        .catch((reason) => {
          if (track?.id === id) error = String(reason || "Could not load this edit.");
        })
        .finally(() => {
          if (track?.id === id) loading = false;
        });
    });
  });
</script>

<section class="view page edit-page">
  <button class="edit-back" onclick={goBack}><Icon name="back" size={14} />Back</button>

  {#if !track}
    <div class="empty-state">
      <h1>Track unavailable</h1>
      <p>Open the editor from a track’s menu so its playback metadata is available.</p>
    </div>
  {:else}
    <header class="edit-head">
      <Cover src={track.cover_url} id={track.album_id || track.uri} name={track.name} size={88} lg />
      <div>
        <span class="tag">Playback edit</span>
        <h1>{track.name}</h1>
        <p>{(track.artist_names ?? []).join(", ")} · {formatTime(track.duration_ms)}</p>
      </div>
    </header>

    <section class="edit-card">
      <div class="edit-section-head">
        <div>
          <h2>Original timeline</h2>
          <p>Cuts and loops use the original song time. Existing queues keep the edit they were created with.</p>
        </div>
        <span class="tnum">{formatTime(track.duration_ms)}</span>
      </div>

      <div class="waveform" aria-label="Decoded track waveform">
        {#if waveform.length}
          {#each waveform as peak, index}
            {@const amplitude = Math.max(Math.abs(peak.min ?? 0), Math.abs(peak.max ?? 0))}
            <span style:height={`${Math.max(2, amplitude * 100)}%`}></span>
          {/each}
        {:else if loading}
          <div class="waveform-message">Decoding waveform…</div>
        {:else}
          <div class="waveform-message">{waveformError || "Waveform unavailable."}</div>
        {/if}
        {#each cuts as cut}
          <i class="region cut" style={timelineStyle(cut)} title="Cut region"></i>
        {/each}
        {#if loopRange}<i class="region loop" style={timelineStyle(loopRange)} title="Loop region"></i>{/if}
      </div>

      <div class="edit-ranges">
        <div class="edit-subhead"><h3>Cut regions</h3><button class="btn-ghost" onclick={addCut}>Add cut</button></div>
        {#if cuts.length === 0}<p class="edit-muted">No sections are removed.</p>{/if}
        {#each cuts as cut, index}
          <div class="range-row">
            <label>Start <input type="number" min="0" max={seconds(duration)} step="0.01" value={seconds(cut.start_ms)} onchange={(event) => setCut(index, "start_ms", millis(event.currentTarget.value))} /></label>
            <input class="range" aria-label={`Cut ${index + 1} start`} type="range" min="0" max={duration} step="10" value={cut.start_ms} oninput={(event) => setCut(index, "start_ms", event.currentTarget.value)} />
            <label>End <input type="number" min="0" max={seconds(duration)} step="0.01" value={seconds(cut.end_ms)} onchange={(event) => setCut(index, "end_ms", millis(event.currentTarget.value))} /></label>
            <input class="range" aria-label={`Cut ${index + 1} end`} type="range" min="0" max={duration} step="10" value={cut.end_ms} oninput={(event) => setCut(index, "end_ms", event.currentTarget.value)} />
            <button class="btn-icon" title="Remove cut" onclick={() => removeCut(index)}><Icon name="x" size={12} /></button>
          </div>
        {/each}
      </div>

      <div class="edit-ranges">
        <div class="edit-subhead"><h3>Exact loop</h3><button class="btn-ghost" onclick={toggleLoop}>{loopRange ? "Remove loop" : "Add loop"}</button></div>
        {#if loopRange}
          <div class="range-row loop-row">
            <label>Start <input type="number" min="0" max={seconds(duration)} step="0.01" value={seconds(loopRange.start_ms)} onchange={(event) => (loopRange.start_ms = millis(event.currentTarget.value))} /></label>
            <input class="range" aria-label="Loop start" type="range" min="0" max={duration} step="10" value={loopRange.start_ms} oninput={(event) => (loopRange.start_ms = Number(event.currentTarget.value))} />
            <label>End <input type="number" min="0" max={seconds(duration)} step="0.01" value={seconds(loopRange.end_ms)} onchange={(event) => (loopRange.end_ms = millis(event.currentTarget.value))} /></label>
            <input class="range" aria-label="Loop end" type="range" min="0" max={duration} step="10" value={loopRange.end_ms} oninput={(event) => (loopRange.end_ms = Number(event.currentTarget.value))} />
          </div>
        {:else}<p class="edit-muted">The track plays through once.</p>{/if}
      </div>

      {#if playlistId}
        <label class="enable-row">
          <input type="checkbox" checked={enabled} disabled={saving || !definitionExists} onchange={(event) => setEnabled(event.currentTarget.checked)} />
          <span><strong>Use edited version in this playlist</strong><small>Off by default. Other playlists still play the intact track.</small></span>
        </label>
      {/if}

      {#if error}<p class="edit-error" role="alert">{error}</p>{/if}
      <div class="edit-actions">
        <button class="btn-ghost danger" disabled={saving || !definitionExists} onclick={removeDefinition}>Delete edit</button>
        <button class="btn-accent" disabled={saving || (!cuts.length && !loopRange)} onclick={save}>{saving ? "Saving…" : "Save edit"}</button>
      </div>
    </section>
  {/if}
</section>

<style>
  .edit-page { max-width: 980px; margin: 0 auto; }
  .edit-back { display:flex; align-items:center; gap:7px; color:var(--fg-2); margin-bottom:22px; }
  .edit-head { display:flex; align-items:center; gap:20px; margin-bottom:28px; }
  .edit-head h1 { font-family:var(--font-display); font-size:clamp(28px,4vw,46px); line-height:1; margin:8px 0; }
  .edit-head p,.edit-muted,.edit-section-head p { color:var(--fg-2); }
  .edit-card { border:1px solid var(--line-2); background:var(--bg-2); border-radius:var(--r3); padding:24px; }
  .edit-section-head,.edit-subhead,.edit-actions { display:flex; align-items:center; justify-content:space-between; gap:18px; }
  .edit-section-head h2,.edit-subhead h3 { margin:0 0 5px; }
  .edit-section-head p { margin:0; max-width:650px; }
  .waveform { position:relative; height:150px; display:flex; align-items:center; gap:1px; margin:24px 0; overflow:hidden; border-radius:var(--r2); background:var(--bg-1); padding:8px; }
  .waveform > span { flex:1 1 1px; min-width:1px; max-height:100%; background:color-mix(in srgb,var(--fg-2) 55%,transparent); }
  .waveform-message { margin:auto; color:var(--fg-3); }
  .region { position:absolute; top:0; bottom:0; pointer-events:none; border-inline:1px solid currentColor; }
  .region.cut { color:var(--rose-ink); background:color-mix(in srgb,var(--rose-ink) 22%,transparent); }
  .region.loop { color:var(--gold); background:color-mix(in srgb,var(--gold) 16%,transparent); }
  .edit-ranges { padding:18px 0; border-top:1px solid var(--line-2); }
  .range-row { display:grid; grid-template-columns:112px minmax(120px,1fr) 112px minmax(120px,1fr) 30px; align-items:end; gap:10px; margin-top:12px; }
  .range-row label { display:grid; gap:5px; color:var(--fg-2); font-size:var(--t-12); }
  .range-row input[type="number"] { width:100%; padding:7px 8px; background:var(--bg-1); border:1px solid var(--line-2); border-radius:var(--r1); color:var(--fg); font-variant-numeric:tabular-nums; }
  .range { accent-color:var(--rose-ink); margin-bottom:8px; }
  .loop-row .range { accent-color:var(--gold); }
  .enable-row { display:flex; gap:12px; align-items:flex-start; padding:16px; border:1px solid var(--line-2); border-radius:var(--r2); }
  .enable-row input { accent-color:var(--rose-ink); margin-top:3px; }
  .enable-row span { display:grid; gap:4px; }.enable-row small { color:var(--fg-2); }
  .edit-error { color:var(--rose-ink); margin:14px 0 0; }
  .edit-actions { margin-top:20px; }.danger { color:var(--rose-ink); }
  @media (max-width:760px) { .range-row { grid-template-columns:100px 1fr 30px; }.range-row label:nth-of-type(2),.range-row input:nth-of-type(2) { grid-column:auto; }.edit-card { padding:16px; } }
</style>
