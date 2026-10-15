// Typed bridge to the Tauri backend (see docs/05-implementation-plan.md, "Tauri commands").
// When the app runs in a plain browser (`npm run dev`), a mock backend is used instead.

export type Lang = 'fr' | 'en' | 'de' | 'es' | 'pt' | (string & {});
/** Localized text: either a plain string or a map lang -> text (as found in zaap.yml). */
export type LocalizedText = string | Record<string, string>;

export interface AccountInfo {
  id: number;
  login: string;
  nickname: string | null;
  tag: string | null;
  security: string[];
  isMain: boolean;
  active: boolean;
  errored: boolean;
}

export type LoginOutcome =
  | { type: 'loggedIn'; account: AccountInfo }
  | { type: 'shieldRequired'; accountId: number; domain: string | null; otp: boolean };

export type Phase = 'preparing' | 'checking' | 'downloading' | 'finalizing' | 'done';

export interface Progress {
  phase: Phase;
  bytesDone: number;
  bytesTotal: number;
  filesDone: number;
  filesTotal: number;
  speedBps: number;
}

export type TaskKind = 'install' | 'update' | 'verify';

export interface TaskState {
  kind: TaskKind;
  progress: Progress | null;
}

export type CloseOnLaunch = 'minimize' | 'keep' | 'quit';

export interface LauncherSettings {
  installDir: string;
  closeOnLaunch: CloseOnLaunch;
  /** Download speed limit in bytes/s; null = unlimited. */
  speedLimitBps: number | null;
  maxConcurrency: number;
  language: Lang;
  /** Chosen portrait per account id: "<classId>-<0|1>". */
  avatars?: Record<string, string>;
}

export interface ReleaseState {
  remoteVersion: string | null;
  installedVersion: string | null;
  installDir: string;
  installed: boolean;
  updateAvailable: boolean;
  serverBlocked: boolean;
  updateBlocked: boolean;
}

export interface AppState {
  accounts: AccountInfo[];
  settings: LauncherSettings;
  release: ReleaseState;
  task: TaskState | null;
  runningInstances: number;
}

export type SettingValue = string | number | boolean;

export interface GameSettingOption {
  label: LocalizedText;
  value: SettingValue;
}

export interface GameSettingDef {
  name: string;
  default: SettingValue;
  label: Record<string, string>;
  description?: Record<string, string>;
  type: 'select' | 'checkbox';
  options?: GameSettingOption[];
}

export interface GameSettings {
  defs: GameSettingDef[];
  values: Record<string, SettingValue>;
}

export interface NewsItem {
  id: number | string;
  title: string;
  date: string;
  url: string;
  image: string | null;
}

export type ServerStatusKind = 'up' | 'down' | 'maintenance';

export interface ServerStatus {
  name: string;
  status: ServerStatusKind;
  tags: string[];
}

export interface TaskFinished {
  ok: boolean;
  error?: string | null;
}

export interface EventMap {
  'task-progress': Progress;
  'task-finished': TaskFinished;
  'state-changed': unknown;
  'game-exited': { pid: number };
}

export type Unlisten = () => void;

export interface Transport {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<K extends keyof EventMap>(event: K, cb: (payload: EventMap[K]) => void): Promise<Unlisten>;
}

export const isTauri: boolean =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

let transportPromise: Promise<Transport> | null = null;

function transport(): Promise<Transport> {
  if (!transportPromise) {
    transportPromise = isTauri
      ? Promise.all([import('@tauri-apps/api/core'), import('@tauri-apps/api/event')]).then(
          ([core, ev]) => ({
            invoke: <T>(cmd: string, args?: Record<string, unknown>) => core.invoke<T>(cmd, args),
            listen: <K extends keyof EventMap>(event: K, cb: (p: EventMap[K]) => void) =>
              ev.listen<EventMap[K]>(event, (e) => cb(e.payload))
          })
        )
      : import('./mock').then((m) => m.mockTransport);
  }
  return transportPromise;
}

/** Turns whatever the backend rejected with into a readable message. */
export function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object') {
    const o = e as Record<string, unknown>;
    if (typeof o.message === 'string') return o.message;
    if (typeof o.error === 'string') return o.error;
  }
  return 'Erreur inconnue';
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const t = await transport();
  return t.invoke<T>(cmd, args);
}

// ---- Normalizers (tolerate serde enum encodings) -------------------------------------------

const lcFirst = (s: string) => s.charAt(0).toLowerCase() + s.slice(1);

/** Accepts `{type:"loggedIn",account}`, `{type:"shieldRequired",...}` and serde's
 *  externally tagged `{"LoggedIn":{...}}` / `{"ShieldRequired":{...}}` forms. */
export function normalizeLoginOutcome(raw: unknown): LoginOutcome {
  const o = (raw ?? {}) as Record<string, any>;
  if (typeof o.type === 'string') {
    const type = lcFirst(o.type);
    if (type === 'loggedIn') return { type, account: (o.account ?? o) as AccountInfo };
    if (type === 'shieldRequired')
      return { type, accountId: o.accountId, domain: o.domain ?? null, otp: !!o.otp };
  }
  const logged = o.loggedIn ?? o.LoggedIn;
  if (logged) return { type: 'loggedIn', account: logged as AccountInfo };
  const shield = o.shieldRequired ?? o.ShieldRequired;
  if (shield)
    return {
      type: 'shieldRequired',
      accountId: shield.accountId ?? shield.account_id,
      domain: shield.domain ?? null,
      otp: !!shield.otp
    };
  throw new Error('Réponse de connexion inattendue');
}

export function normalizeProgress(p: Progress): Progress {
  return { ...p, phase: lcFirst(String(p.phase)) as Phase };
}

function normalizeServer(s: ServerStatus): ServerStatus {
  return { ...s, status: String(s.status).toLowerCase() as ServerStatusKind, tags: s.tags ?? [] };
}

// ---- Commands ------------------------------------------------------------------------------

export const api = {
  getState: () =>
    call<AppState>('get_state').then((s) => ({
      ...s,
      task: s.task ? { ...s.task, progress: s.task.progress && normalizeProgress(s.task.progress) } : null
    })),

  loginStart: () => call<unknown>('login_start').then(normalizeLoginOutcome),
  loginCancel: () => call<void>('login_cancel'),
  shieldValidate: (accountId: number, code: string) =>
    call<AccountInfo>('shield_validate', { accountId, code }),
  shieldResend: (accountId: number, sms: boolean) =>
    call<void>('shield_resend', { accountId, sms }),

  accountSetMain: (accountId: number) => call<void>('account_set_main', { accountId }),
  accountSetActive: (accountId: number, active: boolean) =>
    call<void>('account_set_active', { accountId, active }),
  accountLogout: (accountId: number) => call<void>('account_logout', { accountId }),
  /** `avatar` = "<classId>-<0|1>", null resets to the default egg. */
  accountSetAvatar: (accountId: number, avatar: string | null) =>
    call<void>('account_set_avatar', { accountId, avatar }),

  gameInstall: (installDir?: string) =>
    call<void>('game_install', installDir ? { installDir } : {}),
  gameUpdate: () => call<void>('game_update'),
  gameVerify: () => call<void>('game_verify'),
  taskCancel: () => call<void>('task_cancel'),
  gamePlay: (accountIds: number[]) => call<number[]>('game_play', { accountIds }),

  settingsGet: () => call<LauncherSettings>('settings_get'),
  settingsSet: (settings: LauncherSettings) => call<LauncherSettings>('settings_set', { settings }),
  gameSettingsGet: () => call<GameSettings>('game_settings_get'),
  gameSettingsSet: (key: string, value: SettingValue) =>
    call<GameSettings>('game_settings_set', { key, value }),

  newsGet: (lang: Lang = 'fr') => call<NewsItem[]>('news_get', { lang }),
  serverStatusGet: (lang: Lang = 'fr') =>
    call<ServerStatus[]>('server_status_get', { lang }).then((l) => l.map(normalizeServer)),

  openLogsDir: () => call<void>('open_logs_dir'),
  openInstallDir: () => call<void>('open_install_dir'),

  /** Native folder picker. Resolves null when cancelled or unsupported by the backend. */
  pickFolder: async (initial?: string): Promise<string | null> => {
    try {
      return (await call<string | null>('pick_folder', initial ? { initial } : {})) ?? null;
    } catch {
      return null;
    }
  }
};

export async function on<K extends keyof EventMap>(
  event: K,
  cb: (payload: EventMap[K]) => void
): Promise<Unlisten> {
  const t = await transport();
  if (event === 'task-progress') {
    return t.listen('task-progress', (p) => (cb as (p: Progress) => void)(normalizeProgress(p)));
  }
  return t.listen(event, cb);
}

/** Opens an URL in the system browser. */
export async function openExternal(url: string): Promise<void> {
  if (isTauri) {
    const { openUrl } = await import('@tauri-apps/plugin-opener');
    await openUrl(url);
  } else {
    window.open(url, '_blank', 'noopener');
  }
}

/** Picks a localized string, falling back to fr, en, then the first available. */
export function tr(text: LocalizedText | undefined | null, lang: string = 'fr'): string {
  if (text == null) return '';
  if (typeof text === 'string') return text;
  return text[lang] ?? text.fr ?? text.en ?? Object.values(text)[0] ?? '';
}
