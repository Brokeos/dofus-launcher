<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    open: boolean;
    title: string;
    onclose: () => void;
    width?: string;
    children: Snippet;
    footer?: Snippet;
  }
  let { open, title, onclose, width = '460px', children, footer }: Props = $props();

  let dialog: HTMLDialogElement | undefined = $state();
  const titleId = `modal-${Math.random().toString(36).slice(2, 8)}`;

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  });
</script>

<dialog
  bind:this={dialog}
  aria-labelledby={titleId}
  style:width
  oncancel={(e) => {
    e.preventDefault();
    onclose();
  }}
  onclick={(e) => {
    if (e.target === dialog) onclose();
  }}
>
  {#if open}
    <div class="inner">
      <header>
        <h2 id={titleId}>{title}</h2>
        <button class="btn ghost icon" aria-label="Fermer" onclick={onclose}>
          <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"
            ><path d="M3 3l10 10M13 3L3 13" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" /></svg
          >
        </button>
      </header>
      <div class="body scroll">{@render children()}</div>
      {#if footer}<footer>{@render footer()}</footer>{/if}
    </div>
  {/if}
</dialog>

<style>
  dialog {
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--panel);
    color: var(--text);
    max-width: calc(100vw - 40px);
    max-height: calc(100vh - 40px);
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.55);
  }
  dialog::backdrop {
    background: rgba(5, 7, 4, 0.65);
  }
  .inner {
    display: flex;
    flex-direction: column;
    max-height: calc(100vh - 42px);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 12px 8px 18px;
  }
  header h2 {
    font-size: 16px;
  }
  .body {
    padding: 4px 18px 14px;
    min-height: 0;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 18px;
    border-top: 1px solid var(--border);
  }
</style>
