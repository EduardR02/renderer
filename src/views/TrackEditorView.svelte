<script>
  import { untrack } from "svelte";
  import { api, route, playback, getTrackEditorTrack, goBack, setNavigationGuard } from "../lib/state.svelte.js";
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import TrackEditWaveform from "../components/TrackEditWaveform.svelte";
  import { formatExactTime, formatTime, parseExactTime } from "../lib/time.js";

  const playlistId = $derived(route.param || null);
  const track = $derived(getTrackEditorTrack(route.id, playlistId));
  const duration = $derived(Math.max(1, Math.round(Number(track?.duration_ms) || 1)));
  const ready = $derived(!!track && !loading && !loadError);
  let loading = $state(true), saving = $state(false), loadError = $state(""), actionError = $state("");
  let cuts = $state([]), loopRange = $state(null), selected = $state(null);
  let cursorMs = $state(0), previewState = $state("idle"), previewSignature = $state("");
  let previewBusy = $state(false);
  let undoStack = $state([]), redoStack = $state([]);
  let enabled = $state(false), definitionExists = $state(false), baseline = $state("");
  let retryGeneration = $state(0), waveform = $state(null), waveformState = $state("idle"), waveformError = $state("");
  let definitionGeneration = 0, waveformGeneration = 0, previewGeneration = 0, keySequence = 0;
  let previewRequest = $state(null), previewQueueAwaiting = $state(null), previewQueueIdentity = $state("");

  function keyed(range, prefix) {
    return { _key: range?._key || `${prefix}-${++keySequence}`, start_ms: Math.round(Number(range?.start_ms) || 0), end_ms: Math.round(Number(range?.end_ms) || 0) };
  }
  function hydrateCuts(value) {
    return (value ?? []).map((range) => keyed(range, "cut")).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
  }
  function canonicalCuts(value = cuts) {
    return value.map(({ start_ms, end_ms }) => ({ start_ms: Math.round(start_ms), end_ms: Math.round(end_ms) })).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
  }
  function canonicalLoop(value = loopRange) {
    return value ? { start_ms: Math.round(value.start_ms), end_ms: Math.round(value.end_ms) } : null;
  }
  function snapshot(editCuts = cuts, editLoop = loopRange) {
    return JSON.stringify({ cuts: canonicalCuts(editCuts), loop_range: canonicalLoop(editLoop) });
  }
  function editSignature(edit) {
    return snapshot(edit?.cuts ?? [], edit?.loopRange ?? edit?.loop_range ?? null);
  }
  function copyEdit(editCuts = cuts, editLoop = loopRange) {
    return {
      cuts: editCuts.map((range) => ({ ...range })),
      loopRange: editLoop ? { ...editLoop } : null,
    };
  }
  function editKey(edit) {
    return JSON.stringify({
      cuts: (edit?.cuts ?? []).map(({ start_ms, end_ms }) => [start_ms, end_ms]),
      loop: edit?.loopRange ? [edit.loopRange.start_ms, edit.loopRange.end_ms] : null,
    });
  }
  function commitHistory(before, after = copyEdit()) {
    if (editKey(before) === editKey(after)) return;
    undoStack = [...undoStack, copyEdit(before.cuts, before.loopRange)];
    redoStack = [];
    markPreviewStale();
  }
  function applyEdit(edit) {
    cuts = (edit?.cuts ?? []).map((range) => ({ ...range }));
    loopRange = edit?.loopRange ? { ...edit.loopRange } : null;
    if (selected?.type === "loop" && !loopRange) selected = null;
    if (selected?.type === "cut" && !cuts.some((range) => range._key === selected.key)) selected = null;
  }
  function undo() {
    if (!undoStack.length || !ready || saving) return;
    const current = copyEdit();
    const target = undoStack[undoStack.length - 1];
    undoStack = undoStack.slice(0, -1);
    redoStack = [...redoStack, current];
    applyEdit(target);
    markPreviewStale();
  }
  function redo() {
    if (!redoStack.length || !ready || saving) return;
    const current = copyEdit();
    const target = redoStack[redoStack.length - 1];
    redoStack = redoStack.slice(0, -1);
    undoStack = [...undoStack, current];
    applyEdit(target);
    markPreviewStale();
  }
  function clearHistory() {
    undoStack = [];
    redoStack = [];
  }
  const dirty = $derived(ready && snapshot() !== baseline);
  const validation = $derived.by(() => {
    const cutErrors = cuts.map(() => "");
    let loopError = "";
    cuts.forEach((range, index) => {
      if (range.start_ms < 0 || range.start_ms >= range.end_ms) cutErrors[index] = "The start must be before the end.";
      else if (range.end_ms > duration) cutErrors[index] = "This cut extends past the track.";
      else if (index && range.start_ms < cuts[index - 1].end_ms) cutErrors[index] = "Cuts must be ordered and cannot overlap.";
    });
    if (loopRange) {
      if (loopRange.start_ms < 0 || loopRange.start_ms >= loopRange.end_ms) loopError = "The loop start must be before the end.";
      else if (loopRange.end_ms > duration) loopError = "The loop extends past the track.";
      else {
        const overlap = cuts.findIndex((cut) => cut.start_ms < loopRange.end_ms && loopRange.start_ms < cut.end_ms);
        if (overlap >= 0) {
          loopError = `The loop overlaps cut ${overlap + 1}.`;
          cutErrors[overlap] ||= "This cut overlaps the loop.";
        }
      }
    }
    return { cutErrors, loopError, firstError: cutErrors.find(Boolean) || loopError };
  });
  function active(id, sourcePlaylist, generation = definitionGeneration) {
    return generation === definitionGeneration && route.name === "track-editor" && route.id === id && (route.param || null) === sourcePlaylist;
  }
  function currentRange(type, key) {
    return type === "loop" ? loopRange : cuts.find((range) => range._key === key);
  }
  function setRange(type, key, field, value) {
    if (!ready || saving) return;
    const current = currentRange(type, key);
    if (!current) return;
    const before = copyEdit();
    let next = Math.max(0, Math.min(duration, Math.round(Number(value) || 0)));
    next = field === "start_ms" ? Math.min(next, current.end_ms - 1) : Math.max(next, current.start_ms + 1);
    if (type === "loop") loopRange = { ...current, [field]: next };
    else cuts = cuts.map((range) => range._key === key ? { ...range, [field]: next } : range).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms);
    selected = { type, key };
    actionError = "";
    commitHistory(before);
  }
  function commitTime(event, type, key, field) {
    const input = event.currentTarget, value = parseExactTime(input.value), current = currentRange(type, key);
    if (value === null || !current) {
      input.setCustomValidity("Use seconds, m:ss.mmm, or h:mm:ss.mmm.");
      input.reportValidity();
      if (current) input.value = formatExactTime(current[field]);
      return;
    }
    input.setCustomValidity("");
    setRange(type, key, field, value);
    input.value = formatExactTime(currentRange(type, key)[field]);
  }
  function nudgeTime(event, type, key, field) {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    const current = currentRange(type, key);
    if (!current) return;
    event.preventDefault();
    const amount = event.ctrlKey || event.metaKey ? 100 : event.shiftKey ? 10 : 1;
    setRange(type, key, field, current[field] + (event.key === "ArrowUp" ? amount : -amount));
    event.currentTarget.value = formatExactTime(currentRange(type, key)[field]);
  }
  function removeCut(key) {
    if (!ready || saving) return;
    const current = cuts.find((range) => range._key === key);
    if (!current) return;
    const before = copyEdit();
    cuts = cuts.filter((range) => range._key !== key);
    if (selected?.type === "cut" && selected.key === key) selected = null;
    commitHistory(before);
  }
  function removeLoop() {
    if (!ready || saving || !loopRange) return;
    const before = copyEdit();
    loopRange = null;
    if (selected?.type === "loop") selected = null;
    commitHistory(before);
  }
  async function save() {
    if (!dirty || !ready || !track || saving || validation.firstError || (!cuts.length && !loopRange)) return;
    const id = track.id, sourcePlaylist = playlistId, nextCuts = canonicalCuts(), nextLoop = canonicalLoop();
    saving = true;
    actionError = "";
    try {
      const definition = await api.saveTrackEdit(id, duration, nextCuts, nextLoop);
      if (!active(id, sourcePlaylist)) return;
      cuts = hydrateCuts(definition?.cuts ?? nextCuts);
      loopRange = definition?.loop_range ? keyed(definition.loop_range, "loop") : null;
      baseline = snapshot();
      definitionExists = true;
      selected = null;
      markPreviewStale();
    } catch (reason) {
      if (active(id, sourcePlaylist)) actionError = String(reason || "Could not save this edit.");
    } finally {
      if (active(id, sourcePlaylist)) saving = false;
    }
  }
  async function removeDefinition() {
    if (!ready || !track || saving || !definitionExists) return;
    const id = track.id, sourcePlaylist = playlistId;
    saving = true;
    actionError = "";
    try {
      await api.deleteTrackEdit(id);
      if (!active(id, sourcePlaylist)) return;
      cuts = [];
      loopRange = null;
      selected = null;
      baseline = snapshot([], null);
      definitionExists = false;
      enabled = false;
      markPreviewStale();
    } catch (reason) {
      if (active(id, sourcePlaylist)) actionError = String(reason || "Could not remove this edit.");
    } finally {
      if (active(id, sourcePlaylist)) saving = false;
    }
  }
  async function setEnabled(value) {
    if (!ready || !playlistId || !track || saving) return;
    const id = track.id, sourcePlaylist = playlistId;
    saving = true;
    actionError = "";
    try {
      await api.setPlaylistTrackEditEnabled(sourcePlaylist, id, value);
      if (active(id, sourcePlaylist)) enabled = value;
    } catch (reason) {
      if (active(id, sourcePlaylist)) actionError = String(reason || "Could not update this playlist.");
    } finally {
      if (active(id, sourcePlaylist)) saving = false;
    }
  }
  const currentQueueTrack = $derived(
    playback.current_index >= 0 ? (playback.queue[playback.current_index] ?? null) : null,
  );
  const editorTrackCurrent = $derived(
    !!track && !!currentQueueTrack &&
      (currentQueueTrack.id === track.id || currentQueueTrack.uri === track.uri),
  );
  const queueEditSignature = $derived(
    currentQueueTrack?.effective_edit ? editSignature(currentQueueTrack.effective_edit) : "",
  );
  const queueIdentity = $derived([
    playback.current_index,
    playback.queue.length,
    currentQueueTrack?.id || "",
    currentQueueTrack?.uri || "",
    currentQueueTrack?.context || "",
    queueEditSignature,
  ].join("|"));
  const draftSignature = $derived(snapshot());
  const previewQueueMatches = $derived(
    editorTrackCurrent &&
      !!previewSignature &&
      !!currentQueueTrack?.effective_edit &&
      queueEditSignature === previewSignature &&
      (!previewQueueIdentity || queueIdentity === previewQueueIdentity),
  );
  const previewQueueActive = $derived(
    previewQueueMatches &&
      !previewBusy &&
      (previewState === "playing" || previewState === "paused" || previewState === "stale"),
  );
  const previewNeedsUpdate = $derived(!!previewSignature && draftSignature !== previewSignature);
  const previewActionLabel = $derived(
    previewBusy ? "Preparing preview…" : previewNeedsUpdate ? "Update preview" : "Preview draft",
  );
  const previewStatus = $derived.by(() => {
    if (previewBusy) {
      return { label: "Preparing", copy: "Compiling this snapshot. Editing stays available.", active: false };
    }
    if (previewState === "playing") {
      return { label: "Playing", copy: "Preview queue active · cursor stays on the original timeline.", active: previewIsCurrent };
    }
    if (previewState === "paused") {
      return { label: "Paused", copy: "Preview is paused · resume or update the current draft.", active: previewIsCurrent };
    }
    if (previewState === "stale") {
      return { label: "Draft changed", copy: "Update preview to hear the latest cuts and loop.", active: false };
    }
    if (previewState === "error") {
      return { label: "Preview failed", copy: "Try again. Your editor changes are still live.", active: false };
    }
    return { label: "Ready", copy: "Preview replaces the queue temporarily.", active: false };
  });

  function markPreviewStale() {
    if (!previewSignature || draftSignature === previewSignature) return;
    if (previewState !== "idle" && previewState !== "stale") previewState = "stale";
  }

  function resetPreviewLifecycle() {
    previewGeneration += 1;
    previewRequest = null;
    previewQueueAwaiting = null;
    previewQueueIdentity = "";
    previewBusy = false;
    previewState = "idle";
    previewSignature = "";
  }

  function previewRequestActive(request) {
    return !!request &&
      previewRequest?.generation === request.generation &&
      previewGeneration === request.generation &&
      route.name === "track-editor" &&
      route.id === request.id &&
      (route.param || null) === request.sourcePlaylist &&
      track?.id === request.id;
  }

  function pausePreview() {
    if (!previewQueueActive) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.pause().then(() => {
      if (generation !== previewGeneration || route.name !== "track-editor" || route.id !== id ||
          (route.param || null) !== sourcePlaylist) return;
      if (previewState === "playing") previewState = "paused";
    }).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not pause preview.");
      }
    });
  }

  function resumePreview() {
    if (!previewQueueActive) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.play().then(() => {
      if (generation !== previewGeneration || route.name !== "track-editor" || route.id !== id ||
          (route.param || null) !== sourcePlaylist) return;
      if (previewState === "paused") previewState = "playing";
    }).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not resume preview.");
      }
    });
  }

  async function playPreview() {
    if (!ready || !track || previewBusy || previewQueueAwaiting || validation.firstError) return;
    const id = track.id;
    const sourcePlaylist = playlistId;
    const nextCuts = canonicalCuts();
    const nextLoop = canonicalLoop();
    const signature = snapshot(nextCuts, nextLoop);
    const request = {
      generation: ++previewGeneration,
      id,
      sourcePlaylist,
      signature,
      baselineQueue: queueIdentity,
      acceptedQueueIdentity: previewQueueIdentity,
    };
    previewRequest = request;
    previewQueueAwaiting = null;
    previewBusy = true;
    previewState = "loading";
    actionError = "";
    const position = Math.max(0, Math.min(duration, Math.round(Number(cursorMs) || 0)));
    try {
      await api.previewTrackEdit(track, nextCuts, nextLoop, position);
      if (!previewRequestActive(request)) return;
      const targetQueue = editorTrackCurrent && queueEditSignature === request.signature;
      previewSignature = request.signature;
      previewQueueIdentity = targetQueue ? queueIdentity : "";
      previewRequest = null;
      if (targetQueue) {
        previewQueueAwaiting = null;
        previewBusy = false;
        previewState = draftSignature === request.signature
          ? (playback.playing ? "playing" : "paused")
          : "stale";
      } else {
        previewQueueAwaiting = {
          generation: request.generation,
          baselineQueue: request.baselineQueue,
        };
      }
    } catch (reason) {
      if (!previewRequestActive(request)) return;
      previewRequest = null;
      previewBusy = false;
      previewState = "error";
      actionError = String(reason || "Could not preview this edit.");
    }
  }

  function sourceToCompiled(position) {
    const source = Math.max(0, Math.min(duration, Math.round(Number(position) || 0)));
    let removed = 0;
    for (const cut of canonicalCuts()) {
      if (source <= cut.start_ms) break;
      if (source < cut.end_ms) return Math.max(0, cut.start_ms - removed);
      removed += cut.end_ms - cut.start_ms;
    }
    const compiled = Math.max(0, source - removed);
    return playback.duration_ms > 0 ? Math.min(compiled, playback.duration_ms) : compiled;
  }

  function seekPreview(position) {
    if (!previewIsCurrent || !editorTrackCurrent) return;
    const generation = previewGeneration, id = track?.id, sourcePlaylist = playlistId;
    api.seek(sourceToCompiled(position)).catch((reason) => {
      if (generation === previewGeneration && route.name === "track-editor" && route.id === id &&
          (route.param || null) === sourcePlaylist) {
        actionError = String(reason || "Could not seek preview.");
      }
    });
  }
  const previewIsCurrent = $derived(
    previewQueueMatches &&
      draftSignature === previewSignature &&
      (previewState === "playing" || previewState === "paused"),
  );

  $effect(() => {
    const queueKey = queueIdentity;
    const request = previewRequest;
    const awaiting = previewQueueAwaiting;
    const requestTarget = !!request && editorTrackCurrent && queueEditSignature === request.signature;
    const requestAccepted = !!request &&
      !!previewSignature &&
      queueEditSignature === previewSignature &&
      queueKey === request.acceptedQueueIdentity;
    if (request && previewBusy) {
      if (requestTarget || requestAccepted || queueKey === request.baselineQueue) return;
      resetPreviewLifecycle();
      return;
    }
    if (!previewSignature) return;
    if (awaiting?.generation === previewGeneration) {
      if (!previewQueueMatches) {
        if (queueKey === awaiting.baselineQueue) return;
        resetPreviewLifecycle();
        return;
      }
      previewQueueIdentity = queueKey;
      previewQueueAwaiting = null;
      previewBusy = false;
      if (playback.error) {
        previewState = "error";
        actionError = String(playback.error);
      } else {
        previewState = draftSignature === previewSignature
          ? (playback.playing ? "playing" : "paused")
          : "stale";
      }
      return;
    }
    if (!previewQueueMatches) {
      resetPreviewLifecycle();
      return;
    }
    if (playback.error) {
      previewState = "error";
      actionError = String(playback.error);
      return;
    }
    if (draftSignature !== previewSignature) {
      if (previewState === "playing" || previewState === "paused") previewState = "stale";
      return;
    }
    if (previewState === "stale" || previewState === "playing" || previewState === "paused") {
      const nextState = playback.playing ? "playing" : "paused";
      if (previewState !== nextState) previewState = nextState;
    }
  });

  $effect(() => {
    const id = track?.id, sourcePlaylist = playlistId;
    retryGeneration;
    const generation = ++definitionGeneration;
    loading = !!id;
    loadError = actionError = "";
    cursorMs = 0;
    resetPreviewLifecycle();
    clearHistory();
    cuts = [];
    loopRange = selected = null;
    enabled = definitionExists = false;
    baseline = snapshot([], null);
    if (!id) return;
    untrack(() => api.getTrackEdit(id, sourcePlaylist).then((status) => {
      if (!active(id, sourcePlaylist, generation)) return;
      cuts = hydrateCuts(status?.definition?.cuts);
      loopRange = status?.definition?.loop_range ? keyed(status.definition.loop_range, "loop") : null;
      definitionExists = !!status?.definition;
      enabled = !!status?.enabled;
      baseline = snapshot();
    }).catch((reason) => {
      if (active(id, sourcePlaylist, generation)) loadError = String(reason || "Could not load this edit.");
    }).finally(() => {
      if (active(id, sourcePlaylist, generation)) loading = false;
    }));
  });
  $effect(() => {
    const id = track?.id, generation = ++waveformGeneration;
    waveform = null;
    waveformError = "";
    waveformState = id ? "loading" : "idle";
    if (!id) return;
    untrack(() => api.getTrackWaveform(id).then((result) => {
      if (generation !== waveformGeneration || route.name !== "track-editor" || route.id !== id) return;
      waveform = result;
      waveformState = "ready";
    }).catch((reason) => {
      if (generation !== waveformGeneration || route.name !== "track-editor" || route.id !== id) return;
      waveformError = String(reason || "Could not build the waveform.");
      waveformState = "error";
    }));
    return () => { api.cancelTrackWaveform(id).catch(() => {}); };
  });
  $effect(() => {
    if (!dirty) return;
    return setNavigationGuard(() => window.confirm("Discard your unsaved playback edit?"));
  });
  $effect(() => {
    if (!dirty) return;
    const warn = (event) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  });
</script>

<section class="view page edit-page">
  <button class="edit-back" onclick={goBack}><Icon name="back" size={14} />Back</button>
  {#if !track}
    <div class="empty-state"><h1>Track unavailable</h1><p>Open the editor from a track’s menu so its playback metadata is available.</p></div>
  {:else}
    <header class="edit-head">
      <Cover src={track.cover_url} id={track.album_id || track.uri} name={track.name} size={76} lg />
      <div class="edit-title">
        <span class="tag">Song repair</span>
        <h1>{track.name}</h1>
        <p>{(track.artist_names ?? []).join(", ")} · <span class="tnum">{formatTime(track.duration_ms)}</span> original</p>
      </div>
    </header>
    <section class="repair-sheet" aria-busy={loading}>
      <div class="sheet-head">
        <div><p class="caps">Original timeline</p><h2>Remove damage. Keep the timing exact.</h2></div>
        <p>Edits stay on the original timeline. Progress, seeking, and resampling remain unchanged.</p>
      </div>
      <div class="preview-strip">
        <div class="preview-actions">
          <button class="preview-button" disabled={!ready || previewBusy || !!validation.firstError} onclick={playPreview} title={previewActionLabel}>
            <Icon name="play" size={14} />
            {previewActionLabel}
          </button>
          {#if previewQueueActive}
            <button class="preview-pause" onclick={playback.playing ? pausePreview : resumePreview} title={playback.playing ? "Pause preview" : "Resume preview"}>
              <Icon name={playback.playing ? "pause" : "play"} size={13} />
              {playback.playing ? "Pause" : "Resume"}
            </button>
          {/if}
        </div>
        <div class="preview-copy">
          <strong>{previewStatus.label}</strong>
          <span>{previewStatus.copy} · cursor {formatExactTime(cursorMs)}</span>
        </div>
        <span class="preview-status" class:active={previewStatus.active} class:stale={previewState === "stale"}>{previewStatus.label}</span>
      </div>
      <TrackEditWaveform
        durationMs={duration}
        {waveform}
        {waveformState}
        {waveformError}
        disabled={!ready || saving}
        bind:cuts
        bind:loopRange
        bind:selected
        bind:cursorMs
        previewActive={previewIsCurrent}
        onsave={save}
        oncommit={commitHistory}
        onundo={undo}
        onredo={redo}
        onseek={seekPreview}
      />
      {#if loading}<p class="definition-status">Loading the saved definition before editing is enabled…</p>
      {:else if loadError}<div class="definition-error" role="alert"><span>{loadError}</span><button class="btn-ghost" onclick={() => (retryGeneration += 1)}>Try again</button></div>{/if}

      <section class="range-section">
        <div class="range-heading"><div><h3>Cuts</h3><p>These sections are removed from playback.</p></div><span class="range-count tnum">{cuts.length}</span></div>
        {#if !cuts.length}<p class="empty-range">No cuts yet. Shift-drag across empty waveform space or press C.</p>{/if}
        <div class="range-list">
          {#each cuts as cut, index (cut._key)}
            <div class="exact-row" class:selected={selected?.type === "cut" && selected.key === cut._key}>
              <button class="region-index" onclick={() => (selected = { type: "cut", key: cut._key })} aria-label={`Select cut ${index + 1}`}><span></span>{String(index + 1).padStart(2, "0")}</button>
              <label>Start<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(cut.start_ms)} inputmode="decimal" aria-label={`Cut ${index + 1} start time`} onkeydown={(event) => nudgeTime(event, "cut", cut._key, "start_ms")} onchange={(event) => commitTime(event, "cut", cut._key, "start_ms")} /></label>
              <span class="arrow">→</span>
              <label>End<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(cut.end_ms)} inputmode="decimal" aria-label={`Cut ${index + 1} end time`} onkeydown={(event) => nudgeTime(event, "cut", cut._key, "end_ms")} onchange={(event) => commitTime(event, "cut", cut._key, "end_ms")} /></label>
              <span class="range-duration tnum">{formatExactTime(cut.end_ms - cut.start_ms)}</span>
              <button class="remove-region" disabled={!ready || saving} title={`Remove cut ${index + 1}`} onclick={() => removeCut(cut._key)}><Icon name="x" size={12} /></button>
              {#if validation.cutErrors[index]}<p class="range-error" role="alert">{validation.cutErrors[index]}</p>{/if}
            </div>
          {/each}
        </div>
      </section>

      <section class="range-section">
        <div class="range-heading"><div><h3>Loop</h3><p>Optionally repeat one clean section.</p></div><span class="loop-chip">{loopRange ? "Set" : "Off"}</span></div>
        {#if loopRange}
          <div class="exact-row loop-row" class:selected={selected?.type === "loop"}>
            <button class="region-index" onclick={() => (selected = { type: "loop", key: loopRange._key })} aria-label="Select loop"><span></span>LP</button>
            <label>Start<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(loopRange.start_ms)} inputmode="decimal" aria-label="Loop start time" onkeydown={(event) => nudgeTime(event, "loop", loopRange._key, "start_ms")} onchange={(event) => commitTime(event, "loop", loopRange._key, "start_ms")} /></label>
            <span class="arrow">→</span>
            <label>End<input class="time-input tnum" disabled={!ready || saving} value={formatExactTime(loopRange.end_ms)} inputmode="decimal" aria-label="Loop end time" onkeydown={(event) => nudgeTime(event, "loop", loopRange._key, "end_ms")} onchange={(event) => commitTime(event, "loop", loopRange._key, "end_ms")} /></label>
            <span class="range-duration tnum">{formatExactTime(loopRange.end_ms - loopRange.start_ms)}</span>
            <button class="remove-region" disabled={!ready || saving} title="Remove loop" onclick={removeLoop}><Icon name="x" size={12} /></button>
            {#if validation.loopError}<p class="range-error" role="alert">{validation.loopError}</p>{/if}
          </div>
        {:else}<p class="empty-range">No loop. Use Add loop or press L to create one.</p>{/if}
      </section>

      {#if playlistId}<label class="enable-row"><input type="checkbox" checked={enabled} disabled={!ready || saving || !definitionExists} onchange={(event) => setEnabled(event.currentTarget.checked)} /><span><strong>Use repaired version in this playlist</strong><small>Off by default. Other playlists still play the intact track.</small></span></label>{/if}
      {#if actionError}<p class="edit-error" role="alert">{actionError}</p>{/if}
      {#if validation.firstError}<p class="edit-error" role="alert">Fix the highlighted range before saving.</p>{/if}
      <footer class="edit-footer">
        <div class="footer-left">
          <div class="history-actions" aria-label="Edit history">
            <button class="btn-ghost compact" disabled={!undoStack.length || !ready || saving} onclick={undo} title="Undo (Ctrl+Z)">Undo</button>
            <button class="btn-ghost compact" disabled={!redoStack.length || !ready || saving} onclick={redo} title="Redo (Ctrl+Shift+Z)">Redo</button>
          </div>
          <div class="save-state" class:dirty><span></span>{saving ? "Saving definition…" : dirty ? "Unsaved changes" : definitionExists ? "Saved" : "No saved definition"}</div>
        </div>
        <div><button class="btn-ghost danger" disabled={!ready || saving || !definitionExists} onclick={removeDefinition}>Delete edit</button><button class="btn-accent" disabled={!dirty || !ready || saving || !!validation.firstError || (!cuts.length && !loopRange)} onclick={save}>{saving ? "Saving…" : "Save edit"}</button></div>
      </footer>
    </section>
  {/if}
</section>

<style>
  .edit-page { max-width: 1240px; margin: auto; }
  .edit-back { display: flex; align-items: center; gap: 7px; min-height: 32px; margin-bottom: var(--s4); color: var(--fg-2); }
  .edit-head { display: flex; align-items: center; gap: var(--s4); margin-bottom: var(--s6); }
  .edit-title { min-width: 0; }
  .edit-head h1 { margin: 7px 0 5px; overflow: hidden; font: var(--w-bold) clamp(28px, 4vw, 42px)/1.02 var(--font-display); letter-spacing: -.025em; text-overflow: ellipsis; white-space: nowrap; }
  .edit-head p,.sheet-head>p,.range-heading p,.empty-range { color: var(--fg-2); }
  .repair-sheet { overflow: clip; border: 1px solid var(--line-2); border-radius: var(--r3); background: color-mix(in srgb, var(--bg-2) 74%, var(--bg-1)); }
  .sheet-head { display: flex; align-items: end; justify-content: space-between; gap: var(--s6); padding: var(--s5) var(--s6) var(--s4); border-top: 2px solid color-mix(in srgb, var(--foam) 58%, transparent); }
  .sheet-head h2 { margin-top: 4px; font: var(--w-bold) var(--t-20) var(--font-display); }
  .sheet-head>p { max-width: 430px; font-size: var(--t-12); text-align: right; }
  .preview-strip { display: flex; align-items: center; gap: var(--s3); margin: 0 var(--s6) var(--s3); padding: var(--s3); border: 1px solid color-mix(in srgb, var(--accent) 26%, var(--line)); border-radius: var(--r2); background: color-mix(in srgb, var(--accent) 7%, var(--bg-1)); }
  .preview-button { display: inline-flex; align-items: center; gap: 7px; min-height: 34px; padding: 0 var(--s3); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); border-radius: var(--r2); background: var(--accent); color: var(--accent-ink); font-weight: var(--w-med); }
  .preview-actions { display: flex; align-items: center; gap: 6px; flex: none; }
  .preview-pause { display: inline-flex; align-items: center; gap: 6px; min-height: 34px; padding: 0 10px; border: 1px solid var(--line-2); border-radius: var(--r2); background: var(--bg-2); color: var(--fg-1); font-size: var(--t-11); }
  .preview-pause:hover { border-color: color-mix(in srgb, var(--fg) 22%, transparent); background: var(--bg-3); }
  .preview-button:hover:not(:disabled) { filter: brightness(1.08); }
  .preview-copy { display: grid; min-width: 0; gap: 2px; color: var(--fg-2); font-size: var(--t-11); }
  .preview-copy strong { color: var(--fg-1); font-size: var(--t-12); }
  .preview-status.stale { border-color: color-mix(in srgb, var(--gold) 40%, transparent); color: var(--gold); }
  .preview-copy span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .preview-status { margin-left: auto; padding: 3px 7px; border: 1px solid var(--line); border-radius: var(--rf); color: var(--fg-3); font: var(--t-11) var(--font-small); white-space: nowrap; }
  .preview-status.active { border-color: color-mix(in srgb, var(--accent) 40%, transparent); color: var(--accent); }
  :global(.repair-sheet>.waveform-editor) { padding: 0 var(--s6) var(--s4); }
  .definition-status,.definition-error { min-height: 44px; margin: 0 var(--s6); padding: 10px var(--s3); border-top: 1px solid var(--line); color: var(--fg-2); }
  .definition-error { display: flex; align-items: center; justify-content: space-between; color: var(--rose); }
  .range-section { padding: var(--s5) var(--s6); border-top: 1px solid var(--line-2); }
  .range-heading { display: flex; align-items: center; justify-content: space-between; margin-bottom: var(--s3); }
  .range-heading h3 { font: var(--w-bold) var(--t-15) var(--font-display); }
  .range-heading p,.empty-range { font: var(--t-12) var(--font-small); }
  .range-count,.loop-chip { padding: 3px 8px; border: 1px solid var(--line); border-radius: var(--rf); color: var(--fg-2); font-size: var(--t-11); }
  .loop-chip { color: var(--gold); }
  .range-list { display: grid; gap: 5px; }
  .exact-row { display: grid; grid-template-columns: 48px minmax(128px,174px) 18px minmax(128px,174px) minmax(104px,1fr) 44px; align-items: end; min-height: 64px; padding: 8px 4px 8px 0; border: 1px solid transparent; border-radius: var(--r2); background: color-mix(in srgb, var(--bg-1) 62%, transparent); }
  .exact-row.selected { border-color: color-mix(in srgb, var(--rose-ink) 35%, var(--line)); }
  .loop-row.selected { border-color: color-mix(in srgb, var(--gold) 35%, var(--line)); }
  .region-index { align-self: stretch; display: flex; align-items: center; gap: 7px; padding-left: 10px; color: var(--fg-2); font: 10px var(--font-mono); }
  .region-index span { width: 3px; height: 22px; border-radius: var(--rf); background: var(--rose-ink); }
  .loop-row .region-index span { background: var(--gold); }
  .exact-row label { display: grid; gap: 4px; color: var(--fg-2); font: var(--t-11) var(--font-small); }
  .time-input { width: 100%; height: 34px; padding: 0 9px; border: 1px solid var(--line-2); border-radius: var(--r1); outline: 0; background: var(--bg-2); color: var(--fg); font: var(--t-12) var(--font-mono); }
  .time-input:focus { border-color: var(--accent); }
  .arrow,.range-duration,.remove-region { align-self: center; margin-top: 16px; }
  .arrow { color: var(--fg-3); }
  .range-duration { padding: 0 var(--s4); color: var(--fg-2); font: var(--t-11) var(--font-mono); }
  .remove-region { display: grid; place-items: center; width: 44px; height: 44px; border-radius: 50%; color: var(--fg-2); }
  .range-error { grid-column: 2/-1; color: var(--rose-ink); font-size: var(--t-11); }
  .enable-row { display: flex; gap: var(--s3); margin: 0 var(--s6) var(--s5); padding: var(--s4); border: 1px solid var(--line-2); border-radius: var(--r2); }
  .enable-row span { display: grid; }.enable-row small { color: var(--fg-2); }
  .edit-error { margin: 0 var(--s6) var(--s3); color: var(--rose-ink); }
  .edit-footer { position: sticky; z-index: 8; bottom: 0; display: flex; align-items: center; justify-content: space-between; min-height: 66px; padding: var(--s3) var(--s6); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--bg-sheet) 92%, transparent); backdrop-filter: blur(18px); }
  .edit-footer>div,.footer-left,.history-actions { display: flex; align-items: center; gap: var(--s2); }
  .btn-ghost.compact { min-height: 30px; padding-inline: 10px; font-size: var(--t-11); }
  .save-state { color: var(--fg-2); font-size: var(--t-12); white-space: nowrap; }
  .save-state>span { display: inline-block; width: 7px; height: 7px; margin-right: 7px; border-radius: 50%; background: var(--fg-3); }
  .save-state.dirty>span { background: var(--gold); }
  @media(max-width:760px) {
    .sheet-head { padding: var(--s4); }
    .preview-strip { margin-inline: var(--s4); }
    :global(.repair-sheet>.waveform-editor) { padding-inline: var(--s4); }
    .range-section { padding: var(--s4); }
    .exact-row { grid-template-columns: 40px 1fr 18px 1fr 44px; }
    .range-duration { display: none; }
    .edit-footer { padding-inline: var(--s4); }
  }
  @media(max-width:560px) {
    .sheet-head { display: block; }.sheet-head>p { margin-top: var(--s2); text-align: left; }
    .preview-strip { align-items: flex-start; flex-wrap: wrap; }.preview-status { margin-left: 0; }
    .exact-row { grid-template-columns: 38px 1fr 44px; row-gap: 6px; }
    .exact-row label:first-of-type { grid-column: 2; }.exact-row label:nth-of-type(2) { grid-column: 2; grid-row: 2; }
    .region-index { grid-row: 1/3; }.arrow { display: none; }.remove-region { grid-column: 3; grid-row: 1/3; }
    .edit-footer { align-items: flex-start; flex-direction: column; }.edit-footer>div:last-child { width: 100%; justify-content: flex-end; }
  }
  @media(max-width:420px) { .edit-head :global(.art) { display: none; }.edit-head { margin-bottom: var(--s4); } }
</style>
