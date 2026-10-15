<script lang="ts">
  import { symbol } from '$lib/sprites';
  import { api, errorMessage, type ServerStatus, type ServerStatusKind } from '$lib/api';
  import { onMount } from 'svelte';

  let servers = $state<ServerStatus[] | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);

  const labels: Record<ServerStatusKind, string> = {
    up: 'En ligne',
    down: 'Hors ligne',
    maintenance: 'Maintenance'
  };

  async function load() {
    loading = true;
    error = null;
    try {
      servers = await api.serverStatusGet('fr');
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<section class="panel servers" aria-labelledby="srv-title" aria-busy={loading}>
  <div class="panel-head">
    <h2 id="srv-title"><img src={symbol(16)} alt="" width="18" height="18" />Serveurs</h2>
    <button class="btn ghost icon" aria-label="Actualiser l'état des serveurs" title="Actualiser" disabled={loading} onclick={load}>
      <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2"
        ><path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7" /></svg
      >
    </button>
  </div>
  {#if error}
    <p class="empty" role="alert">Impossible de récupérer l'état des serveurs.<br /><small>{error}</small></p>
  {:else if !servers}
    <p class="empty">Chargement…</p>
  {:else if servers.length === 0}
    <p class="empty">Aucun serveur Dofus 3 trouvé.</p>
  {:else}
    <ul class="scroll">
      {#each servers as s (s.name)}
        <li>
          <span class="name">{s.name}</span>
          <span class="chip {s.status}"><span class="dot" aria-hidden="true"></span>{labels[s.status] ?? s.status}</span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .servers {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .panel-head {
    margin-bottom: 4px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 10px;
    min-height: 0;
  }
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    padding: 3px 0;
    font-size: 13px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--border);
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }
  .up {
    color: var(--ok);
  }
  .down {
    color: var(--danger);
  }
  .maintenance {
    color: var(--warn);
  }
</style>
