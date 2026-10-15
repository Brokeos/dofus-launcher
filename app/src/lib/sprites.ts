// Dofus 3 sprites extracted from the game's Unity bundles (Picto/UI/class_assets_*), see scripts/extract_sprites.py.
// Ids follow the game's breed ids (19 = unused).

export const CLASSES = [
  { id: 1, name: 'Féca' },
  { id: 2, name: 'Osamodas' },
  { id: 3, name: 'Enutrof' },
  { id: 4, name: 'Sram' },
  { id: 5, name: 'Xélor' },
  { id: 6, name: 'Ecaflip' },
  { id: 7, name: 'Eniripsa' },
  { id: 8, name: 'Iop' },
  { id: 9, name: 'Crâ' },
  { id: 10, name: 'Sadida' },
  { id: 11, name: 'Sacrieur' },
  { id: 12, name: 'Pandawa' },
  { id: 13, name: 'Roublard' },
  { id: 14, name: 'Zobal' },
  { id: 15, name: 'Steamer' },
  { id: 16, name: 'Eliotrope' },
  { id: 17, name: 'Huppermage' },
  { id: 18, name: 'Ouginak' },
  { id: 20, name: 'Forgelance' }
] as const;

export const asset = {
  egg: '/dofus/egg.webp',
  logo: '/dofus/logo.webp',
  hero: '/dofus/hero.webp'
};

/** Class symbol (64px). */
export const symbol = (classId: number) => `/dofus/symbols/${classId}.webp`;

/** Class head portrait (54x56); `female` picks the second variant. */
export const head = (classId: number, female = false) => `/dofus/heads/${classId}${female ? 1 : 0}.webp`;

/** Stable portrait for an account, derived from its id. */
export function accountHead(accountId: number): string {
  const n = Math.abs(Math.trunc(accountId));
  const c = CLASSES[n % CLASSES.length];
  return head(c.id, Math.floor(n / CLASSES.length) % 2 === 1);
}
