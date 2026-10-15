// In-browser simulated backend used when not running inside Tauri.
import type {
  AccountInfo,
  AppState,
  EventMap,
  GameSettingDef,
  GameSettings,
  LauncherSettings,
  NewsItem,
  Progress,
  ServerStatus,
  SettingValue,
  TaskKind,
  Transport,
  Unlisten
} from './api';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

type Listener = (payload: unknown) => void;
const listeners = new Map<string, Set<Listener>>();

function emit<K extends keyof EventMap>(event: K, payload: EventMap[K]) {
  listeners.get(event)?.forEach((cb) => cb(payload));
}

const REMOTE = '6.0_3.7.10.10';

const settings: LauncherSettings = {
  installDir: 'C:\\Users\\Joueur\\AppData\\Local\\Ankama\\Dofus-dofus3',
  closeOnLaunch: 'minimize',
  speedLimitBps: null,
  maxConcurrency: 8,
  language: 'fr',
  avatars: { '1001': '16-0' }
};

const state: AppState = {
  accounts: [
    {
      id: 1001,
      login: 'iop-du-dimanche',
      nickname: 'Goultard',
      tag: '1234',
      security: ['SHIELD'],
      isMain: true,
      active: true,
      errored: false
    }
  ],
  settings,
  release: {
    remoteVersion: REMOTE,
    installedVersion: null,
    installDir: settings.installDir,
    installed: false,
    updateAvailable: false,
    serverBlocked: false,
    updateBlocked: false
  },
  task: null,
  runningInstances: 0
};

const gameDefs: GameSettingDef[] = [
  {
    name: 'language',
    default: 'fr',
    label: { fr: 'Langue', en: 'Language' },
    description: { fr: 'Langue utilisée dans le jeu Dofus', en: 'Language used in the game Dofus' },
    type: 'select',
    options: [
      { label: 'Français', value: 'fr' },
      { label: 'English', value: 'en' },
      { label: 'Deutsch', value: 'de' },
      { label: 'Español', value: 'es' },
      { label: 'Português', value: 'pt' }
    ]
  },
  {
    name: 'autoConnectType',
    default: 1,
    label: { fr: 'Connexion', en: 'Connection' },
    description: { fr: 'Se connecter directement ...', en: 'Log in directly ...' },
    type: 'select',
    options: [
      { label: { fr: '... au choix de serveur', en: '... to server choice' }, value: 0 },
      { label: { fr: '... au choix de personnage', en: '... to character choice' }, value: 1 },
      { label: { fr: '... au jeu', en: '... to the game' }, value: 2 }
    ]
  },
  {
    name: 'connectionPort',
    default: '5555',
    label: { fr: 'Port de connexion', en: 'Connection port' },
    type: 'select',
    options: [
      { label: { fr: '5555' }, value: '5555' },
      { label: { fr: '443' }, value: '443' }
    ]
  },
  {
    name: 'directxVersion',
    default: '',
    label: { fr: 'Version de DirectX', en: 'DirectX version' },
    type: 'select',
    options: [
      { label: { fr: 'DirectX 12 (par défaut)', en: 'DirectX 12 (default)' }, value: '' },
      { label: { fr: 'DirectX 11', en: 'DirectX 11' }, value: '-force-d3d11' }
    ]
  },
  {
    name: 'hdReady',
    default: false,
    label: { fr: 'Télécharger Dofus HD Pack (Experimental)', en: 'Download the DOFUS HD Pack (Experimental)' },
    type: 'checkbox'
  },
  {
    name: '4kReady',
    default: false,
    label: { fr: 'Télécharger Dofus 4K Pack (Experimental)', en: 'Download the DOFUS 4K Pack (Experimental)' },
    type: 'checkbox'
  }
];
const gameValues: Record<string, SettingValue> = Object.fromEntries(
  gameDefs.map((d) => [d.name, d.default])
);

const servers: ServerStatus[] = [
  'Dakal', 'Brial', 'Mikhal', 'Salar', 'Rafal', 'Kourial', 'Imagiro', 'Orukam', 'Hell Mina',
  'Tylezia', 'Draconiros', 'Tal Kasha', 'Ombre'
].map((name, i) => ({
  name,
  status: i === 4 ? 'maintenance' : i === 11 ? 'down' : 'up',
  tags: ['dofus3']
}));

const img = (hue: number) =>
  'data:image/svg+xml,' +
  encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="160"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},45%,28%)"/><stop offset="1" stop-color="hsl(${hue + 40},40%,14%)"/></linearGradient></defs><rect width="320" height="160" fill="url(#g)"/><circle cx="250" cy="40" r="60" fill="hsla(80,70%,50%,.15)"/></svg>`
  );

const news: NewsItem[] = [
  { id: 1, title: 'Mise à jour 3.7 : les Dofus Primordiaux', date: '2026-10-14T09:00:00Z', url: 'https://www.dofus.com/fr/mmorpg/actualites', image: img(90) },
  { id: 2, title: 'Maintenance hebdomadaire du mardi', date: '2026-10-13T06:00:00Z', url: 'https://www.dofus.com/fr/mmorpg/actualites', image: img(200) },
  { id: 3, title: 'Événement : la Nuit des Bouftous', date: '2026-10-10T12:00:00Z', url: 'https://www.dofus.com/fr/mmorpg/actualites', image: img(30) },
  { id: 4, title: 'Devblog : refonte des Sadidas', date: '2026-10-07T15:30:00Z', url: 'https://www.dofus.com/fr/mmorpg/actualites', image: null }
];

// ---- simulated task ----
let cancelFlag = false;

async function runTask(kind: TaskKind) {
  cancelFlag = false;
  const verify = kind === 'verify';
  const bytesTotal = verify ? 0 : kind === 'install' ? 4_820_000_000 : 312_000_000;
  const filesTotal = verify ? 18_420 : kind === 'install' ? 18_420 : 214;
  const p: Progress = { phase: 'preparing', bytesDone: 0, bytesTotal, filesDone: 0, filesTotal, speedBps: 0 };
  const push = () => {
    state.task = { kind, progress: { ...p } };
    emit('task-progress', { ...p });
  };
  state.task = { kind, progress: null };
  emit('state-changed', null);
  push();
  await sleep(800);
  try {
    p.phase = 'checking';
    const checkSteps = verify ? 40 : 6;
    for (let i = 1; i <= checkSteps; i++) {
      if (cancelFlag) throw new Error('cancelled');
      p.filesDone = Math.round((filesTotal * i) / checkSteps);
      push();
      await sleep(250);
    }
    if (!verify) {
      p.phase = 'downloading';
      p.filesDone = 0;
      const steps = 60;
      for (let i = 1; i <= steps; i++) {
        if (cancelFlag) throw new Error('cancelled');
        const limit = settings.speedLimitBps ?? Infinity;
        p.speedBps = Math.min(limit, 38_000_000 + Math.round(Math.random() * 12_000_000));
        p.bytesDone = Math.round((bytesTotal * i) / steps);
        p.filesDone = Math.round((filesTotal * i) / steps);
        push();
        await sleep(250);
      }
    }
    p.phase = 'finalizing';
    p.speedBps = 0;
    push();
    await sleep(700);
    p.phase = 'done';
    push();
    state.release.installed = true;
    state.release.installedVersion = REMOTE;
    state.release.updateAvailable = false;
    state.task = null;
    emit('task-finished', { ok: true });
  } catch {
    state.task = null;
    emit('task-finished', { ok: false, error: cancelFlag ? 'Opération annulée' : 'Erreur simulée' });
  }
  emit('state-changed', null);
}

// ---- simulated login ----
let loginAbort: (() => void) | null = null;
let loginCount = 0;
let nextId = 2000;
const pending = new Map<number, AccountInfo>();

function newAccount(): AccountInfo {
  const id = nextId++;
  const names = ['Sramoche', 'Eniripsa-Zen', 'Panda-Rouge', 'Xelor-Tic'];
  return {
    id,
    login: `compte${id}`,
    nickname: names[id % names.length],
    tag: String(1000 + ((id * 37) % 9000)),
    security: ['SHIELD'],
    isMain: state.accounts.length === 0,
    active: true,
    errored: false
  };
}

function account(id: number): AccountInfo {
  const a = state.accounts.find((x) => x.id === id);
  if (!a) throw 'Compte introuvable';
  return a;
}

const handlers: Record<string, (args: Record<string, any>) => unknown | Promise<unknown>> = {
  get_state: () => structuredClone(state),
  login_start: () =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        loginAbort = null;
        const acc = newAccount();
        loginCount++;
        if (loginCount % 2 === 1) {
          pending.set(acc.id, acc);
          resolve({ type: 'shieldRequired', accountId: acc.id, domain: 'gmail.com', otp: loginCount === 3 });
        } else {
          state.accounts.push(acc);
          emit('state-changed', null);
          resolve({ type: 'loggedIn', account: acc });
        }
      }, 3000);
      loginAbort = () => {
        clearTimeout(t);
        reject('Connexion annulée');
      };
    }),
  login_cancel: () => {
    loginAbort?.();
    loginAbort = null;
  },
  shield_validate: async ({ accountId, code }) => {
    await sleep(600);
    const acc = pending.get(accountId);
    if (!acc) throw 'Session de validation expirée';
    if (!/^\d{6}$/.test(String(code))) throw 'Code invalide (6 chiffres attendus)';
    pending.delete(accountId);
    state.accounts.push(acc);
    emit('state-changed', null);
    return acc;
  },
  shield_resend: async () => {
    await sleep(400);
  },
  account_set_main: ({ accountId }) => {
    account(accountId);
    state.accounts.forEach((a) => (a.isMain = a.id === accountId));
    emit('state-changed', null);
  },
  account_set_active: ({ accountId, active }) => {
    account(accountId).active = !!active;
    emit('state-changed', null);
  },
  account_set_avatar: ({ accountId, avatar }) => {
    const avatars = { ...(settings.avatars ?? {}) };
    if (avatar) avatars[String(accountId)] = String(avatar);
    else delete avatars[String(accountId)];
    settings.avatars = avatars;
    emit('state-changed', null);
  },
  account_logout: async ({ accountId }) => {
    await sleep(300);
    const wasMain = account(accountId).isMain;
    state.accounts = state.accounts.filter((a) => a.id !== accountId);
    if (wasMain && state.accounts[0]) state.accounts[0].isMain = true;
    emit('state-changed', null);
  },
  game_install: ({ installDir }) => {
    if (state.task) throw 'Une opération est déjà en cours';
    if (installDir) {
      settings.installDir = installDir;
      state.release.installDir = installDir;
    }
    void runTask('install');
  },
  game_update: () => {
    if (state.task) throw 'Une opération est déjà en cours';
    void runTask('update');
  },
  game_verify: () => {
    if (state.task) throw 'Une opération est déjà en cours';
    if (!state.release.installed) throw "Le jeu n'est pas installé";
    void runTask('verify');
  },
  task_cancel: () => {
    cancelFlag = true;
  },
  game_play: async ({ accountIds }) => {
    if (!state.release.installed) throw "Le jeu n'est pas installé";
    if (state.release.updateAvailable) throw 'Une mise à jour est requise';
    await sleep(900);
    const ids: number[] = accountIds.length ? accountIds : [0];
    const pids = ids.map(() => 10000 + Math.floor(Math.random() * 50000));
    state.runningInstances += pids.length;
    emit('state-changed', null);
    pids.forEach((pid, i) =>
      setTimeout(() => {
        state.runningInstances = Math.max(0, state.runningInstances - 1);
        emit('game-exited', { pid });
        emit('state-changed', null);
      }, 20000 + i * 3000)
    );
    return pids;
  },
  settings_get: () => ({ ...settings }),
  settings_set: ({ settings: s }) => {
    Object.assign(settings, s, { avatars: settings.avatars });
    state.release.installDir = settings.installDir;
    emit('state-changed', null);
    return { ...settings };
  },
  game_settings_get: (): GameSettings => ({ defs: structuredClone(gameDefs), values: { ...gameValues } }),
  game_settings_set: ({ key, value }): GameSettings => {
    if (!gameDefs.some((d) => d.name === key)) throw `Paramètre inconnu : ${key}`;
    gameValues[key] = value;
    if ((key === 'hdReady' || key === '4kReady') && value && state.release.installed) {
      state.release.updateAvailable = true;
      emit('state-changed', null);
    }
    return { defs: structuredClone(gameDefs), values: { ...gameValues } };
  },
  news_get: async () => {
    await sleep(500);
    return structuredClone(news);
  },
  server_status_get: async () => {
    await sleep(400);
    return structuredClone(servers);
  },
  open_logs_dir: () => undefined,
  open_install_dir: () => undefined,
  pick_folder: () => {
    const v = window.prompt('Dossier d’installation (simulation du sélecteur) :', settings.installDir);
    return v && v.trim() ? v.trim() : null;
  }
};

export const mockTransport: Transport = {
  async invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
    const h = handlers[cmd];
    if (!h) throw `Commande inconnue : ${cmd}`;
    await sleep(60);
    return (await h(args)) as T;
  },
  async listen<K extends keyof EventMap>(event: K, cb: (p: EventMap[K]) => void): Promise<Unlisten> {
    let set = listeners.get(event);
    if (!set) listeners.set(event, (set = new Set()));
    const l = cb as Listener;
    set.add(l);
    return () => set!.delete(l);
  }
};

if (typeof console !== 'undefined') console.info('[dofus-launcher] Backend simulé (hors Tauri)');
