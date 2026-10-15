<script lang="ts">
  import { api, type AccountInfo } from '$lib/api';
  import { accountName } from '$lib/format';
  import { store } from '$lib/store.svelte';

  interface Props {
    onadd: () => void;
  }
  let { onadd }: Props = $props();

  const accounts = $derived(store.state?.accounts ?? []);
  let busy = $state<number | null>(null);

  async function logout(a: AccountInfo) {
    if (!confirm(`Déconnecter le compte ${accountName(a)} ?`)) return;
    busy = a.id;
    await store.run(() => api.accountLogout(a.id), 'Déconnexion impossible');
    busy = null;
    await store.refresh();
  }
</script>

<section class="panel accounts" aria-labelledby="acc-title">
  <div class="panel-head">
    <h2 id="acc-title">Comptes</h2>
    <button class="btn small primary" onclick={onadd}>+ Ajouter un compte</button>
  </div>

  <ul class="scroll">
    {#each accounts as a (a.id)}
      <li class:inactive={!a.active}>
        <label class="toggle" title={a.active ? 'Actif : lancé avec « Jouer »' : 'Inactif'}>
          <input
            type="checkbox"
            checked={a.active}
            aria-label={`Activer ${accountName(a)} pour le lancement`}
            onchange={(e) => store.run(() => api.accountSetActive(a.id, e.currentTarget.checked))}
          />
        </label>
        <div class="who">
          <div class="nick">
            <span>{a.nickname ?? a.login}</span>{#if a.tag}<span class="tag">#{a.tag}</span>{/if}
            {#if a.isMain}<span class="badge">Principal</span>{/if}
          </div>
          <div class="login">
            {a.login}
            {#if a.errored}<span class="err">· session expirée, reconnectez-vous</span>{/if}
          </div>
        </div>
        <div class="acts">
          {#if !a.isMain}
            <button
              class="btn ghost icon"
              title="Définir comme principal"
              aria-label={`Définir ${accountName(a)} comme compte principal`}
              onclick={() => store.run(() => api.accountSetMain(a.id))}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2"
                ><path d="M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z" /></svg
              >
            </button>
          {/if}
          <button
            class="btn ghost icon danger"
            title="Déconnecter"
            aria-label={`Déconnecter ${accountName(a)}`}
            disabled={busy === a.id}
            onclick={() => logout(a)}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2"
              ><path d="M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10" /></svg
            >
          </button>
        </div>
      </li>
    {:else}
      <li class="empty-li">
        <p class="empty">Aucun compte. Ajoutez votre compte Ankama pour jouer.</p>
      </li>
    {/each}
  </ul>
</section>

<style>
  .accounts {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
    flex: 1;
  }
  li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 8px;
    border-radius: 8px;
    background: var(--bg-2);
    border: 1px solid var(--border);
  }
  li.inactive .who {
    opacity: 0.55;
  }
  li.empty-li {
    background: none;
    border: none;
    justify-content: center;
  }
  .toggle {
    display: flex;
    cursor: pointer;
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  .nick {
    display: flex;
    align-items: center;
    gap: 2px;
    font-weight: 600;
    white-space: nowrap;
  }
  .nick > span:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tag {
    color: var(--muted);
    font-weight: 400;
  }
  .badge {
    margin-left: 6px;
    font-size: 10.5px;
    font-weight: 500;
    padding: 1px 6px;
    border-radius: 999px;
    color: var(--lime);
    border: 1px solid rgba(166, 218, 41, 0.4);
  }
  .login {
    font-size: 12px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    color: #ffb4a6;
  }
  .acts {
    display: flex;
  }
  .acts .btn.icon {
    width: 28px;
    height: 28px;
    padding: 4px;
  }
</style>
