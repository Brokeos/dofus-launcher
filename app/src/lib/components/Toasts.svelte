<script lang="ts">
  import { store } from '$lib/store.svelte';

  let el: HTMLDivElement | undefined = $state();

  // Re-show as a manual popover on every new toast so it sits above any open <dialog> (top layer).
  $effect(() => {
    const n = store.toasts.length;
    if (!el || typeof el.showPopover !== 'function') return;
    if (el.matches(':popover-open')) el.hidePopover();
    if (n > 0) el.showPopover();
  });
</script>

<div class="toasts" role="region" aria-label="Notifications" popover="manual" bind:this={el}>
  {#each store.toasts as t (t.id)}
    <div class="toast {t.kind}" role={t.kind === 'error' ? 'alert' : 'status'}>
      <span>{t.text}</span>
      <button class="btn ghost small" aria-label="Fermer la notification" onclick={() => store.dismiss(t.id)}>✕</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    inset: auto;
    margin: 0;
    padding: 0;
    border: 0;
    background: transparent;
    overflow: visible;
    right: 16px;
    bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 100;
    max-width: 380px;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    justify-content: space-between;
    padding: 9px 8px 9px 12px;
    border-radius: 8px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-left: 3px solid var(--muted);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    font-size: 13px;
  }
  .toast span {
    padding-top: 2px;
  }
  .error {
    border-left-color: var(--danger);
  }
  .success {
    border-left-color: var(--lime);
  }
  .info {
    border-left-color: #6aa8d8;
  }
</style>
