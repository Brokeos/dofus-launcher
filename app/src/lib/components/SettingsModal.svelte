<script lang="ts">
  import {
    api,
    errorMessage,
    tr,
    type CloseOnLaunch,
    type GameSettingDef,
    type LauncherSettings,
    type SettingValue
  } from '$lib/api';
  import { store } from '$lib/store.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    onclose: () => void;
  }
  let { open, onclose }: Props = $props();

  const LANG = 'fr';

  let launcher = $state<LauncherSettings | null>(null);
  let unlimited = $state(true);
  let limitMo = $state(10);
  let defs = $state<GameSettingDef[]>([]);
  let values = $state<Record<string, SettingValue>>({});
  let initialValues: Record<string, SettingValue> = {};
  let loadError = $state<string | null>(null);
  let saving = $state(false);

  $effect(() => {
    if (open) void load();
  });

  async function load() {
    launcher = null;
    loadError = null;
    try {
      const [s, g] = await Promise.all([api.settingsGet(), api.gameSettingsGet()]);
      launcher = s;
      unlimited = s.speedLimitBps == null || s.speedLimitBps <= 0;
      limitMo = unlimited ? 10 : Math.max(0.1, Math.round((s.speedLimitBps! / 1_000_000) * 10) / 10);
      defs = g.defs;
      const v: Record<string, SettingValue> = {};
      for (const d of g.defs) v[d.name] = g.values[d.name] ?? d.default;
      values = v;
      initialValues = { ...v };
    } catch (e) {
      loadError = errorMessage(e);
    }
  }

  async function browse() {
    if (!launcher) return;
    const dir = await api.pickFolder(launcher.installDir);
    if (dir) launcher.installDir = dir;
  }

  function optionIndex(d: GameSettingDef): number {
    const cur = values[d.name];
    // Loose comparison on purpose: a stored "1" must match option value 1.
    return (d.options ?? []).findIndex((o) => String(o.value) === String(cur));
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!launcher) return;
    saving = true;
    try {
      const next: LauncherSettings = {
        ...$state.snapshot(launcher),
        installDir: launcher.installDir.trim(),
        speedLimitBps: unlimited ? null : Math.round(Math.max(0.1, limitMo) * 1_000_000),
        maxConcurrency: Math.min(32, Math.max(1, Math.round(launcher.maxConcurrency || 1)))
      };
      await api.settingsSet(next);
      for (const d of defs) {
        if (values[d.name] !== initialValues[d.name]) await api.gameSettingsSet(d.name, values[d.name]);
      }
      store.toast('Paramètres enregistrés', 'success');
      await store.refresh();
      onclose();
    } catch (err) {
      store.toast(`Enregistrement impossible : ${errorMessage(err)}`);
    } finally {
      saving = false;
    }
  }

  const closeOptions: { value: CloseOnLaunch; label: string }[] = [
    { value: 'minimize', label: 'Réduire le launcher' },
    { value: 'keep', label: 'Laisser le launcher ouvert' },
    { value: 'quit', label: 'Fermer le launcher' }
  ];
  const taskRunning = $derived(!!store.state?.task);
</script>

<Modal {open} title="Paramètres" {onclose} width="560px">
  {#if loadError}
    <p class="empty" role="alert">Impossible de charger les paramètres : {loadError}</p>
  {:else if !launcher}
    <p class="empty">Chargement…</p>
  {:else}
    <form id="settings-form" onsubmit={save}>
      <fieldset>
        <legend>Launcher</legend>
        <div class="field">
          <label for="s-dir">Dossier d'installation</label>
          <div class="row">
            <input id="s-dir" class="input" bind:value={launcher.installDir} spellcheck="false" disabled={taskRunning} />
            <button type="button" class="btn" onclick={browse} disabled={taskRunning}>Parcourir…</button>
          </div>
          {#if store.state?.release.installed}
            <span class="hint">Changer ce dossier ne déplace pas les fichiers déjà installés.</span>
          {/if}
        </div>

        <div class="field">
          <label for="s-close">Au lancement du jeu</label>
          <select id="s-close" class="input" bind:value={launcher.closeOnLaunch}>
            {#each closeOptions as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
          </select>
        </div>

        <div class="grid2">
          <div class="field">
            <span class="label" id="s-speed-label">Limite de téléchargement</span>
            <label class="check">
              <input type="checkbox" bind:checked={unlimited} />
              Illimitée
            </label>
            <div class="row">
              <input
                type="number"
                class="input"
                min="0.1"
                step="0.5"
                bind:value={limitMo}
                disabled={unlimited}
                aria-labelledby="s-speed-label"
                aria-describedby="s-speed-unit"
              />
              <span id="s-speed-unit" class="unit">Mo/s</span>
            </div>
          </div>
          <div class="field">
            <label for="s-conc">Téléchargements simultanés</label>
            <input id="s-conc" type="number" class="input" min="1" max="32" step="1" bind:value={launcher.maxConcurrency} />
            <span class="hint">Entre 1 et 32 (défaut : 8).</span>
          </div>
        </div>

        <div class="row links">
          <button type="button" class="btn small" onclick={() => store.run(() => api.openInstallDir())}>
            Ouvrir le dossier du jeu
          </button>
          <button type="button" class="btn small" onclick={() => store.run(() => api.openLogsDir())}>
            Ouvrir les journaux
          </button>
        </div>
      </fieldset>

      <fieldset>
        <legend>Jeu (Dofus 3)</legend>
        {#each defs as d (d.name)}
          {@const desc = tr(d.description, LANG)}
          {#if d.type === 'checkbox'}
            <div class="field">
              <label class="check">
                <input
                  type="checkbox"
                  checked={values[d.name] === true || values[d.name] === 'true'}
                  onchange={(e) => (values[d.name] = e.currentTarget.checked)}
                  aria-describedby={desc ? `g-${d.name}-d` : undefined}
                />
                {tr(d.label, LANG)}
              </label>
              {#if desc}<span id={`g-${d.name}-d`} class="hint">{desc}</span>{/if}
            </div>
          {:else}
            <div class="field">
              <label for={`g-${d.name}`}>{tr(d.label, LANG)}</label>
              <select
                id={`g-${d.name}`}
                class="input"
                value={optionIndex(d)}
                onchange={(e) => {
                  const o = d.options?.[Number(e.currentTarget.value)];
                  if (o) values[d.name] = o.value;
                }}
                aria-describedby={desc ? `g-${d.name}-d` : undefined}
              >
                {#if optionIndex(d) < 0}<option value={-1} disabled>—</option>{/if}
                {#each d.options ?? [] as o, i (i)}
                  <option value={i}>{tr(o.label, LANG)}</option>
                {/each}
              </select>
              {#if desc}<span id={`g-${d.name}-d`} class="hint">{desc}</span>{/if}
            </div>
          {/if}
        {:else}
          <p class="empty">Les paramètres du jeu seront disponibles après l'installation.</p>
        {/each}
      </fieldset>
    </form>
  {/if}

  {#snippet footer()}
    <button class="btn" onclick={onclose}>Annuler</button>
    <button class="btn primary" type="submit" form="settings-form" disabled={!launcher || saving}>
      {saving ? 'Enregistrement…' : 'Enregistrer'}
    </button>
  {/snippet}
</Modal>

<style>
  fieldset {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 10px 14px 4px;
    margin: 0 0 14px;
  }
  legend {
    padding: 0 6px;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--lime);
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .unit {
    color: var(--muted);
    font-size: 12.5px;
  }
  .links {
    margin-bottom: 10px;
  }
</style>
