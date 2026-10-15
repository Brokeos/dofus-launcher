import type { Phase } from './api';

const nf1 = new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 1, minimumFractionDigits: 1 });
const nf0 = new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 0 });

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '–';
  if (n < 1024) return `${nf0.format(n)} o`;
  const units = ['Ko', 'Mo', 'Go', 'To'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${nf1.format(v)} ${units[i]}`;
}

export function formatSpeed(bps: number): string {
  return `${nf1.format(bps / 1_000_000)} Mo/s`;
}

export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '–';
  const s = Math.round(seconds);
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ${String(s % 60).padStart(2, '0')} s`;
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} min`;
}

export function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString('fr-FR', { day: 'numeric', month: 'long', year: 'numeric' });
}

export const phaseLabel: Record<Phase, string> = {
  preparing: 'Préparation…',
  checking: 'Vérification des fichiers…',
  downloading: 'Téléchargement…',
  finalizing: 'Finalisation…',
  done: 'Terminé'
};

export function accountName(a: { nickname: string | null; tag: string | null; login: string }): string {
  return a.nickname ? (a.tag ? `${a.nickname}#${a.tag}` : a.nickname) : a.login;
}
