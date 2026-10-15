<script lang="ts">
  import { api } from '$lib/api';
  import { accountName } from '$lib/format';
  import { store } from '$lib/store.svelte';
  import { accountHead, asset } from '$lib/sprites';

  interface Props {
    onsettings: () => void;
    onaddaccount: () => void;
  }
  let { onsettings, onaddaccount }: Props = $props();

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();

  const accounts = $derived(store.state?.accounts ?? []);
  const label = $derived.by(() => {
    const active = store.activeAccounts;
    if (accounts.length === 0) return 'Aucun compte';
    if (active.length === 0) return 'Aucun compte actif';
    const main = active.find((a) => a.isMain) ?? active[0];
    return active.length > 1 ? `${accountName(main)} +${active.length - 1}` : accountName(main);
  });

  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }
  function onKey(e: KeyboardEvent) {
    if (open && e.key === 'Escape') {
      open = false;
      root?.querySelector<HTMLButtonElement>('.switcher')?.focus();
    }
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onKey} />

<header>
  <div class="brand">
    <img class="egg" src={asset.egg} alt="" width="28" height="28" />
    <h1>Dofus Launcher</h1>
  </div>

  <div class="right">
    <div class="menu-wrap" bind:this={root}>
      <button
        class="btn switcher"
        aria-haspopup="true"
        aria-expanded={open}
        aria-controls="account-menu"
        onclick={() => (open = !open)}
      >
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"
          ><circle cx="8" cy="5.5" r="3" fill="currentColor" /><path
            d="M2 14c.6-3 3-4.5 6-4.5s5.4 1.5 6 4.5"
            fill="currentColor"
          /></svg
        >
        <span>{label}</span>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"
          ><path d="M2 3.5l3 3 3-3" stroke="currentColor" stroke-width="1.5" fill="none" /></svg
        >
      </button>
      {#if open}
        <div id="account-menu" class="menu" role="group" aria-label="Comptes actifs">
          {#each accounts as a (a.id)}
            <div class="item">
              <label class="check">
                <input
                  type="checkbox"
                  checked={a.active}
                  onchange={(e) =>
                    store.run(() => api.accountSetActive(a.id, e.currentTarget.checked))}
                />
                <img class="avatar" src={accountHead(a.id)} alt="" width="22" height="22" />
                <span class="name">{accountName(a)}</span>
              </label>
              {#if a.isMain}
                <span class="badge">Principal</span>
              {:else}
                <button
                  class="btn ghost small"
                  onclick={() => store.run(() => api.accountSetMain(a.id))}
                  aria-label={`Définir ${accountName(a)} comme compte principal`}>Principal</button
                >
              {/if}
            </div>
          {:else}
            <p class="empty">Aucun compte connecté.</p>
          {/each}
          <button
            class="btn small add"
            onclick={() => {
              open = false;
              onaddaccount();
            }}>+ Ajouter un compte</button
          >
        </div>
      {/if}
    </div>
    <button class="btn icon ghost" aria-label="Paramètres" title="Paramètres" onclick={onsettings}>
      <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8">
        <circle cx="12" cy="12" r="3" />
        <path
          d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"
        />
      </svg>
    </button>
  </div>
</header>

<style>
  header {
    height: 52px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 14px 0 16px;
    border-bottom: 1px solid var(--border);
    background: rgba(18, 21, 15, 0.7);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .egg {
    filter: drop-shadow(0 0 6px rgba(166, 218, 41, 0.35));
  }
  .avatar {
    border-radius: 50%;
    background: var(--bg-2);
    object-fit: cover;
  }
  h1 {
    font-size: 16px;
    letter-spacing: 0.02em;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .menu-wrap {
    position: relative;
  }
  .switcher span {
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 50;
    width: 300px;
    padding: 8px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 4px 6px;
    border-radius: 6px;
  }
  .item:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    font-size: 11px;
    padding: 2px 7px;
    border-radius: 999px;
    color: var(--lime);
    border: 1px solid rgba(166, 218, 41, 0.4);
  }
  .add {
    margin-top: 6px;
  }
</style>
