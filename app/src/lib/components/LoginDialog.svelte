<script lang="ts">
  import { api, errorMessage } from '$lib/api';
  import { accountName } from '$lib/format';
  import { store } from '$lib/store.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    open: boolean;
    onclose: () => void;
  }
  let { open, onclose }: Props = $props();

  type Step =
    | { kind: 'idle' }
    | { kind: 'waiting' }
    | { kind: 'shield'; accountId: number; domain: string | null; otp: boolean }
    | { kind: 'error'; message: string };

  let step = $state<Step>({ kind: 'idle' });
  let code = $state('');
  let submitting = $state(false);
  let shieldError = $state<string | null>(null);
  let resent = $state<string | null>(null);
  let attempt = 0;
  let codeInput: HTMLInputElement | undefined = $state();

  $effect(() => {
    if (open && step.kind === 'idle') void start();
  });
  $effect(() => {
    if (step.kind === 'shield') codeInput?.focus();
  });

  async function start() {
    const my = ++attempt;
    step = { kind: 'waiting' };
    code = '';
    shieldError = null;
    resent = null;
    try {
      const out = await api.loginStart();
      if (my !== attempt) return;
      if (out.type === 'loggedIn') {
        store.toast(`Compte ${accountName(out.account)} ajouté`, 'success');
        await store.refresh();
        finish();
      } else {
        step = { kind: 'shield', accountId: out.accountId, domain: out.domain, otp: out.otp };
      }
    } catch (e) {
      if (my !== attempt) return;
      step = { kind: 'error', message: errorMessage(e) };
    }
  }

  function finish() {
    attempt++;
    step = { kind: 'idle' };
    onclose();
  }

  async function cancel() {
    if (step.kind === 'waiting') {
      attempt++;
      await store.run(() => api.loginCancel());
    }
    finish();
  }

  async function validate(e: SubmitEvent) {
    e.preventDefault();
    if (step.kind !== 'shield') return;
    const c = code.trim();
    if (!c) {
      shieldError = 'Saisissez le code reçu.';
      return;
    }
    submitting = true;
    shieldError = null;
    try {
      const acc = await api.shieldValidate(step.accountId, c);
      store.toast(`Compte ${accountName(acc)} ajouté`, 'success');
      await store.refresh();
      finish();
    } catch (err) {
      shieldError = errorMessage(err);
    } finally {
      submitting = false;
    }
  }

  async function resend(sms: boolean) {
    if (step.kind !== 'shield') return;
    resent = null;
    const id = step.accountId;
    const ok = await store.run(() => api.shieldResend(id, sms).then(() => true), "Échec de l'envoi");
    if (ok) resent = sms ? 'Code renvoyé par SMS.' : 'Code renvoyé par e-mail.';
  }
</script>

<Modal {open} title="Ajouter un compte Ankama" onclose={cancel}>
  {#if step.kind === 'waiting' || step.kind === 'idle'}
    <div class="center" role="status" aria-live="polite">
      <div class="spinner" aria-hidden="true"></div>
      <p><strong>Connexion dans votre navigateur…</strong></p>
      <p class="muted">
        Une page de connexion Ankama s'est ouverte. Identifiez-vous puis revenez ici : le compte sera ajouté
        automatiquement.
      </p>
      <button class="btn" onclick={cancel}>Annuler</button>
    </div>
  {:else if step.kind === 'shield'}
    <form onsubmit={validate}>
      <p>
        <strong>Ankama Shield</strong> — cet appareil doit être autorisé.
      </p>
      <p class="muted">
        {#if step.otp}
          Saisissez le code à 6 chiffres affiché par votre application d'authentification (Ankama
          Authenticator).
        {:else}
          Un code de sécurité a été envoyé {step.domain ? `à votre adresse @${step.domain}` : 'par e-mail'}.
        {/if}
      </p>
      <div class="field">
        <label for="shield-code">Code de sécurité</label>
        <input
          id="shield-code"
          class="input code"
          bind:this={codeInput}
          bind:value={code}
          inputmode="numeric"
          autocomplete="one-time-code"
          maxlength="12"
          aria-invalid={shieldError ? 'true' : undefined}
          aria-describedby={shieldError ? 'shield-err' : undefined}
        />
        {#if shieldError}<span id="shield-err" class="error" role="alert">{shieldError}</span>{/if}
        {#if resent}<span class="hint" role="status">{resent}</span>{/if}
      </div>
      <div class="row between">
        {#if !step.otp}
          <div class="row">
            <button type="button" class="btn ghost small" onclick={() => resend(false)}>Renvoyer par e-mail</button>
            <button type="button" class="btn ghost small" onclick={() => resend(true)}>Renvoyer par SMS</button>
          </div>
        {:else}
          <span></span>
        {/if}
        <div class="row">
          <button type="button" class="btn" onclick={cancel}>Annuler</button>
          <button type="submit" class="btn primary" disabled={submitting}>
            {submitting ? 'Validation…' : 'Valider'}
          </button>
        </div>
      </div>
    </form>
  {:else if step.kind === 'error'}
    <div class="center">
      <p class="error" role="alert">La connexion a échoué : {step.message}</p>
      <div class="row">
        <button class="btn" onclick={finish}>Fermer</button>
        <button class="btn primary" onclick={start}>Réessayer</button>
      </div>
    </div>
  {/if}
</Modal>

<style>
  .center {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 6px;
    padding: 10px 0 6px;
  }
  .center p {
    margin: 0;
  }
  .center .btn,
  .center .row {
    margin-top: 10px;
  }
  .muted {
    color: var(--muted);
    font-size: 13px;
  }
  form p {
    margin: 0 0 8px;
  }
  .code {
    font-size: 20px;
    letter-spacing: 0.3em;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
  .error {
    color: #ffb4a6;
    font-size: 13px;
  }
  .between {
    justify-content: space-between;
  }
  .spinner {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    border: 3px solid var(--border);
    border-top-color: var(--lime);
    animation: spin 0.9s linear infinite;
    margin-bottom: 6px;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
