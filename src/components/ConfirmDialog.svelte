<script>
  let {
    open,
    title,
    message,
    confirmLabel = "Confirm",
    busy = false,
    error = "",
    onConfirm,
    onCancel,
  } = $props();

  let dialog = $state(null);
  let cancelButton = $state(null);
  const titleId = "confirm-dialog-title";
  const messageId = "confirm-dialog-message";

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      dialog.showModal();
      queueMicrotask(() => cancelButton?.focus());
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  function cancel() {
    if (!busy) onCancel?.();
  }

  function onNativeCancel(event) {
    event.preventDefault();
    cancel();
  }

  function onBackdropClick(event) {
    if (event.target === dialog) cancel();
  }
</script>

<dialog
  class="confirm-dialog"
  bind:this={dialog}
  aria-labelledby={titleId}
  aria-describedby={messageId}
  aria-busy={busy}
  oncancel={onNativeCancel}
  onclick={onBackdropClick}
>
  <div class="dialog-sheet">
    <div class="dialog-copy">
      <h2 id={titleId}>{title}</h2>
      <p id={messageId}>{message}</p>
      {#if error}<p class="dialog-error" role="alert">{error}</p>{/if}
    </div>
    <div class="dialog-actions">
      <button class="btn-ghost" bind:this={cancelButton} disabled={busy} onclick={cancel}>Cancel</button>
      <button class="btn-danger" disabled={busy} onclick={onConfirm}>
        {busy ? "Deleting…" : confirmLabel}
      </button>
    </div>
  </div>
</dialog>
