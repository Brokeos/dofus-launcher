import { api, errorMessage, on, type AppState, type Progress, type Unlisten } from './api';

export type ToastKind = 'error' | 'info' | 'success';
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
}

class LauncherStore {
  state = $state<AppState | null>(null);
  progress = $state<Progress | null>(null);
  loadError = $state<string | null>(null);
  launching = $state(false);
  toasts = $state<Toast[]>([]);
  private toastId = 0;
  private unlisten: Unlisten[] = [];
  private refreshQueued = false;

  activeAccounts = $derived(this.state?.accounts.filter((a) => a.active && !a.errored) ?? []);
  mainAccount = $derived(this.state?.accounts.find((a) => a.isMain) ?? null);

  toast(text: string, kind: ToastKind = 'error') {
    const id = ++this.toastId;
    this.toasts.push({ id, kind, text });
    setTimeout(() => this.dismiss(id), kind === 'error' ? 7000 : 4000);
  }

  dismiss(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  /** Runs an action, turning failures into an error toast. Returns undefined on failure. */
  async run<T>(fn: () => Promise<T>, errorPrefix?: string): Promise<T | undefined> {
    try {
      return await fn();
    } catch (e) {
      const msg = errorMessage(e);
      this.toast(errorPrefix ? `${errorPrefix} : ${msg}` : msg);
      return undefined;
    }
  }

  async refresh() {
    try {
      const s = await api.getState();
      this.state = s;
      this.loadError = null;
      if (s.task?.progress) this.progress = s.task.progress;
      if (!s.task) this.progress = null;
    } catch (e) {
      this.loadError = errorMessage(e);
    }
  }

  /** Coalesces bursts of `state-changed` events into a single refresh. */
  private queueRefresh() {
    if (this.refreshQueued) return;
    this.refreshQueued = true;
    setTimeout(() => {
      this.refreshQueued = false;
      void this.refresh();
    }, 50);
  }

  async init() {
    this.unlisten.push(
      await on('task-progress', (p) => {
        this.progress = p;
      }),
      await on('task-finished', (r) => {
        this.progress = null;
        if (r.ok) this.toast('Opération terminée', 'success');
        else if (r.error) this.toast(r.error);
        this.queueRefresh();
      }),
      await on('state-changed', () => this.queueRefresh()),
      await on('game-exited', () => this.queueRefresh())
    );
    await this.refresh();
  }

  destroy() {
    this.unlisten.forEach((u) => u());
    this.unlisten = [];
  }
}

export const store = new LauncherStore();
