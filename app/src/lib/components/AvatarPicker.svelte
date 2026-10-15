<script lang="ts">
  import { api, type AccountInfo } from '$lib/api';
  import { accountName } from '$lib/format';
  import { CLASSES, head, parseAvatar } from '$lib/sprites';
  import { store } from '$lib/store.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    account: AccountInfo | null;
    onclose: () => void;
  }
  let { account, onclose }: Props = $props();

  const current = $derived(account ? parseAvatar(store.state?.settings.avatars?.[String(account.id)]) : null);

  async function pick(avatar: string | null) {
    if (!account) return;
    const id = account.id;
    onclose();
    await store.run(() => api.accountSetAvatar(id, avatar), 'Impossible de changer le portrait');
    await store.refresh();
  }
</script>

<Modal open={!!account} title={account ? `Classe de ${accountName(account)}` : ''} width="620px" {onclose}>
  <p class="intro">Choisissez la classe et le sexe de votre personnage principal.</p>
  <ul class="grid">
    {#each CLASSES as c (c.id)}
      <li>
        <span class="name">{c.name}</span>
        <div class="pair">
          {#each [false, true] as female (female)}
            {@const selected = current?.classId === c.id && current.female === female}
            <button
              class="head"
              class:selected
              aria-pressed={selected}
              aria-label={`${c.name} ${female ? 'femme' : 'homme'}`}
              title={`${c.name} ${female ? '♀' : '♂'}`}
              onclick={() => pick(`${c.id}-${female ? 1 : 0}`)}
            >
              <img src={head(c.id, female)} alt="" width="44" height="46" />
            </button>
          {/each}
        </div>
      </li>
    {/each}
  </ul>
  {#snippet footer()}
    <button class="btn" onclick={() => pick(null)} disabled={!current}>Aucune classe</button>
    <button class="btn primary" onclick={onclose}>Fermer</button>
  {/snippet}
</Modal>

<style>
  .intro {
    margin: 0 0 10px;
    color: var(--muted);
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: 8px;
  }
  li {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 6px 4px;
    border-radius: 8px;
    background: var(--bg-2);
    border: 1px solid var(--border);
  }
  .name {
    font-size: 12px;
    color: var(--muted);
  }
  .pair {
    display: flex;
    gap: 2px;
  }
  .head {
    padding: 2px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: transparent;
    line-height: 0;
  }
  .head:hover {
    background: var(--panel-2);
  }
  .head.selected {
    border-color: var(--lime);
    background: rgba(166, 218, 41, 0.12);
  }
</style>
