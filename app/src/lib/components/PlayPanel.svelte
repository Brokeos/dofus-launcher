<script lang="ts">
  import { api } from '$lib/api';
  import { formatBytes, formatEta, formatSpeed, phaseLabel } from '$lib/format';
  import { store } from '$lib/store.svelte';

  const s = $derived(store.state);
  const release = $derived(s?.release);
  const task = $derived(s?.task ?? null);
  const progress = $derived(store.progress ?? task?.progress ?? null);
  const running = $derived(s?.runningInstances ?? 0);

  let cancelling = $state(false);
  let installDir = $state('');
  let dirTouched = $state(false);

  $effect(() => {
    if (!dirTouched && s) installDir = s.settings.installDir || s.release.installDir;
  });
  $effect(() => {
    if (!task) cancelling = false;
  });

  type Mode = 'loading' | 'busy' | 'launching' | 'install' | 'update' | 'play';
  const mode: Mode = $derived(
    !release
      ? 'loading'
      : task
        ? 'busy'
        : store.launching
          ? 'launching'
          : !release.installed
            ? 'install'
            : release.updateAvailable
              ? 'update'
              : 'play'
  );

  const pct = $derived.by(() => {
    if (!progress) return 0;
    if (progress.phase === 'downloading' && progress.bytesTotal > 0)
      return (progress.bytesDone / progress.bytesTotal) * 100;
    if (progress.filesTotal > 0) return (progress.filesDone / progress.filesTotal) * 100;
    return progress.phase === 'done' ? 100 : 0;
  });
  const eta = $derived(
    progress && progress.speedBps > 0
      ? (progress.bytesTotal - progress.bytesDone) / progress.speedBps
      : NaN
  );

  const taskTitle = $derived(
    task?.kind === 'install' ? 'Installation' : task?.kind === 'verify' ? 'Vérification' : 'Mise à jour'
  );

  const noAccount = $derived(store.activeAccounts.length === 0);

  async function primary() {
    switch (mode) {
      case 'busy':
        cancelling = true;
        await store.run(() => api.taskCancel(), "Impossible d'annuler");
        return;
      case 'install':
        if (!installDir.trim()) {
          store.toast("Choisissez un dossier d'installation");
          return;
        }
        await store.run(() => api.gameInstall(installDir.trim()), "Échec de l'installation");
        await store.refresh();
        return;
      case 'update':
        await store.run(() => api.gameUpdate(), 'Échec de la mise à jour');
        await store.refresh();
        return;
      case 'play': {
        const ids = store.activeAccounts.map((a) => a.id);
        store.launching = true;
        const pids = await store.run(() => api.gamePlay(ids), 'Lancement impossible');
        store.launching = false;
        if (pids)
          store.toast(
            pids.length > 1 ? `${pids.length} instances de Dofus lancées` : 'Dofus est lancé',
            'success'
          );
        await store.refresh();
        return;
      }
    }
  }

  async function browse() {
    const dir = await api.pickFolder(installDir);
    if (dir) {
      installDir = dir;
      dirTouched = true;
    }
  }

  const buttonLabel = $derived(
    {
      loading: 'Chargement…',
      busy: cancelling ? 'Annulation…' : 'Annuler',
      launching: 'En cours…',
      install: 'Installer',
      update: 'Mettre à jour',
      play: 'Jouer'
    }[mode]
  );
  const buttonDisabled = $derived(
    mode === 'loading' ||
      mode === 'launching' ||
      (mode === 'busy' && cancelling) ||
      (mode === 'update' && !!release?.updateBlocked) ||
      (mode === 'play' && noAccount)
  );
</script>

<section class="panel hero" aria-labelledby="play-title">
  <div class="top">
    <div>
      <h2 id="play-title">DOFUS <span class="lime">3</span></h2>
      <dl class="versions">
        <div>
          <dt>Installée</dt>
          <dd>{release?.installedVersion ?? '—'}</dd>
        </div>
        <div>
          <dt>Disponible</dt>
          <dd>{release?.remoteVersion ?? '—'}</dd>
        </div>
        {#if running > 0}
          <div>
            <dt>En jeu</dt>
            <dd class="lime">{running} instance{running > 1 ? 's' : ''}</dd>
          </div>
        {/if}
      </dl>
    </div>
    <div class="secondary">
      <button
        class="btn small"
        disabled={!release?.installed || !!task || running > 0}
        title={running > 0 ? 'Fermez Dofus pour vérifier les fichiers' : undefined}
        onclick={async () => {
          await store.run(() => api.gameVerify(), 'Vérification impossible');
          await store.refresh();
        }}>Vérifier les fichiers</button
      >
    </div>
  </div>

  {#if release?.serverBlocked}
    <div class="banner warn" role="status">Les serveurs Dofus sont actuellement fermés (maintenance en cours).</div>
  {/if}
  {#if release?.updateBlocked}
    <div class="banner warn" role="status">Les mises à jour sont temporairement bloquées par Ankama. Réessayez plus tard.</div>
  {/if}

  {#if mode === 'install'}
    <div class="field install-dir">
      <label for="install-dir">Dossier d'installation</label>
      <div class="row">
        <input
          id="install-dir"
          class="input"
          bind:value={installDir}
          oninput={() => (dirTouched = true)}
          spellcheck="false"
        />
        <button class="btn" onclick={browse}>Parcourir…</button>
      </div>
    </div>
  {/if}

  <div class="actions">
    <button
      class="play"
      class:cancel={mode === 'busy'}
      disabled={buttonDisabled}
      onclick={primary}
      aria-describedby={mode === 'play' && noAccount ? 'play-hint' : undefined}
    >
      {buttonLabel}
    </button>

    <div class="status" aria-live="polite">
      {#if task}
        <div class="phase">
          <strong>{taskTitle}</strong>
          <span>{progress ? phaseLabel[progress.phase] : 'Préparation…'}</span>
          <span class="pct">{pct.toFixed(0)} %</span>
        </div>
        <div
          class="bar"
          role="progressbar"
          aria-label={`${taskTitle} en cours`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(pct)}
        >
          <div class="fill" style:width={`${pct}%`}></div>
        </div>
        {#if progress}
          <div class="stats">
            {#if progress.bytesTotal > 0}
              <span>{formatBytes(progress.bytesDone)} / {formatBytes(progress.bytesTotal)}</span>
            {/if}
            {#if progress.filesTotal > 0}
              <span>{progress.filesDone.toLocaleString('fr-FR')} / {progress.filesTotal.toLocaleString('fr-FR')} fichiers</span>
            {/if}
            {#if progress.phase === 'downloading'}
              <span>{formatSpeed(progress.speedBps)}</span>
              <span>Reste {formatEta(eta)}</span>
            {/if}
          </div>
        {/if}
      {:else if mode === 'play' && noAccount}
        <p id="play-hint" class="hint">Ajoutez ou activez un compte pour jouer.</p>
      {:else if mode === 'play'}
        <p class="hint">
          {store.activeAccounts.length > 1
            ? `${store.activeAccounts.length} comptes actifs : une instance par compte.`
            : 'Le jeu est à jour.'}
        </p>
      {:else if mode === 'update'}
        <p class="hint">Une nouvelle version est disponible.</p>
      {:else if mode === 'install'}
        <p class="hint">Dofus 3 n'est pas encore installé sur cet ordinateur.</p>
      {:else if mode === 'loading' && store.loadError}
        <p class="hint err" role="alert">{store.loadError}</p>
      {/if}
    </div>
  </div>
</section>

<style>
  .hero {
    padding: 18px 20px;
    background:
      linear-gradient(120deg, rgba(76, 110, 26, 0.28), transparent 55%),
      var(--panel);
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }
  h2 {
    font-size: 26px;
    font-weight: 800;
    letter-spacing: 0.04em;
  }
  .lime {
    color: var(--lime);
  }
  .versions {
    display: flex;
    gap: 22px;
    margin: 6px 0 0;
  }
  .versions dt {
    font-size: 11.5px;
    color: var(--muted);
  }
  .versions dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
    font-size: 13px;
  }
  .banner {
    margin-top: 12px;
    padding: 8px 12px;
    border-radius: 8px;
    font-size: 13px;
  }
  .banner.warn {
    background: rgba(227, 167, 47, 0.12);
    border: 1px solid rgba(227, 167, 47, 0.4);
    color: #f3d08a;
  }
  .install-dir {
    margin: 14px 0 0;
  }
  .actions {
    margin-top: 16px;
    display: flex;
    gap: 20px;
    align-items: center;
  }
  .play {
    flex: none;
    width: 210px;
    height: 58px;
    border-radius: 12px;
    border: 1px solid #6c9a24;
    background: linear-gradient(180deg, #6d9b22, var(--green));
    color: #fff;
    font-size: 20px;
    font-weight: 700;
    letter-spacing: 0.03em;
    box-shadow: 0 6px 18px rgba(76, 110, 26, 0.35), inset 0 1px 0 rgba(255, 255, 255, 0.15);
    transition: filter 0.12s;
  }
  .play:hover:not(:disabled) {
    filter: brightness(1.1);
  }
  .play:disabled {
    filter: grayscale(0.6) brightness(0.75);
    cursor: not-allowed;
  }
  .play.cancel {
    background: var(--panel-2);
    border-color: #5a3a32;
    color: #ffb4a6;
    box-shadow: none;
  }
  .status {
    flex: 1;
    min-width: 0;
  }
  .phase {
    display: flex;
    gap: 10px;
    align-items: baseline;
    font-size: 13px;
  }
  .phase span {
    color: var(--muted);
  }
  .phase .pct {
    margin-left: auto;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    margin: 7px 0;
    height: 8px;
    border-radius: 99px;
    background: #0d100a;
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: linear-gradient(90deg, var(--green), var(--lime));
    transition: width 0.25s linear;
  }
  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 16px;
    font-size: 12px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .hint {
    margin: 0;
    color: var(--muted);
  }
  .hint.err {
    color: #ffb4a6;
  }
</style>
