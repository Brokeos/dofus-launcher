<script lang="ts">
  import { onMount } from 'svelte';
  import { isTauri } from '$lib/api';
  import { store } from '$lib/store.svelte';
  import Header from '$lib/components/Header.svelte';
  import PlayPanel from '$lib/components/PlayPanel.svelte';
  import AccountsPanel from '$lib/components/AccountsPanel.svelte';
  import ServerPanel from '$lib/components/ServerPanel.svelte';
  import NewsPanel from '$lib/components/NewsPanel.svelte';
  import LoginDialog from '$lib/components/LoginDialog.svelte';
  import SettingsModal from '$lib/components/SettingsModal.svelte';
  import Toasts from '$lib/components/Toasts.svelte';

  let loginOpen = $state(false);
  let settingsOpen = $state(false);

  onMount(() => {
    void store.init();
    return () => store.destroy();
  });
</script>

<div class="app">
  <Header onsettings={() => (settingsOpen = true)} onaddaccount={() => (loginOpen = true)} />
  <main>
    <div class="left">
      <PlayPanel />
      <NewsPanel />
    </div>
    <aside class="right" aria-label="Comptes et serveurs">
      <AccountsPanel onadd={() => (loginOpen = true)} />
      <ServerPanel />
    </aside>
  </main>
  {#if !isTauri}<div class="mock-flag" aria-hidden="true">Simulation</div>{/if}
</div>

<LoginDialog open={loginOpen} onclose={() => (loginOpen = false)} />
<SettingsModal open={settingsOpen} onclose={() => (settingsOpen = false)} />
<Toasts />

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  main {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 330px;
    gap: 14px;
    padding: 14px;
  }
  .left,
  .right {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }
  .right > :global(:first-child) {
    flex: 1 1 50%;
  }
  .right > :global(:last-child) {
    flex: 1 1 50%;
  }
  .mock-flag {
    position: fixed;
    left: 10px;
    bottom: 8px;
    font-size: 11px;
    color: var(--muted);
    opacity: 0.6;
    pointer-events: none;
  }
</style>
