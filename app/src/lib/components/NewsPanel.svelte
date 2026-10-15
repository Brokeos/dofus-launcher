<script lang="ts">
  import { api, errorMessage, openExternal, type NewsItem } from '$lib/api';
  import { formatDate } from '$lib/format';
  import { store } from '$lib/store.svelte';
  import { onMount } from 'svelte';

  let items = $state<NewsItem[] | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);

  async function load() {
    loading = true;
    error = null;
    try {
      items = await api.newsGet('fr');
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  function open(url: string) {
    void store.run(() => openExternal(url), "Impossible d'ouvrir le lien");
  }

  onMount(load);
</script>

<section class="panel news" aria-labelledby="news-title" aria-busy={loading}>
  <div class="panel-head">
    <h2 id="news-title">Actualités</h2>
    <button class="btn ghost icon" aria-label="Actualiser les actualités" title="Actualiser" disabled={loading} onclick={load}>
      <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2"
        ><path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7" /></svg
      >
    </button>
  </div>
  {#if error}
    <p class="empty" role="alert">Impossible de charger les actualités.<br /><small>{error}</small></p>
  {:else if !items}
    <p class="empty">Chargement…</p>
  {:else if items.length === 0}
    <p class="empty">Aucune actualité pour le moment.</p>
  {:else}
    <ul class="grid scroll">
      {#each items as n (n.id)}
        <li>
          <button class="card" onclick={() => open(n.url)} title={n.title}>
            <div class="img">
              {#if n.image}<img src={n.image} alt="" loading="lazy" decoding="async" />{/if}
            </div>
            <div class="txt">
              <span class="title">{n.title}</span>
              <time datetime={n.date}>{formatDate(n.date)}</time>
            </div>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .news {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 2px;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(165px, 1fr));
    gap: 10px;
    min-height: 0;
  }
  .card {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    text-align: left;
    padding: 0;
    border-radius: 9px;
    border: 1px solid var(--border);
    background: var(--bg-2);
    overflow: hidden;
    transition: border-color 0.12s;
  }
  .card:hover {
    border-color: var(--green-hover);
  }
  .img {
    aspect-ratio: 2 / 1;
    width: 100%;
    background: linear-gradient(135deg, #2a3320, #161a12);
  }
  .img img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .txt {
    padding: 8px 10px 10px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  time {
    font-size: 11.5px;
    color: var(--muted);
  }
</style>
