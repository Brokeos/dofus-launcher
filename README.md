# Dofus Launcher

Launcher Windows léger qui remplace l'Ankama Launcher, uniquement pour Dofus 3. Usage personnel.

- **Rapide** : Tauri 2 + Rust, exe d'environ 11 Mo, aucune vidéo, chat ou boutique, aucune interrogation périodique en arrière-plan.
- **Mises à jour fiables** :
  - écritures atomiques (fichier temporaire, vérification SHA-1, puis renommage) ;
  - verrou exclusif sur le dossier d'installation ;
  - reprise après interruption sans tout revérifier ;
  - refus de mettre à jour pendant que Dofus tourne.
- **Multi-comptes** : connexion OAuth Ankama dans le navigateur, avec Shield. Les secrets sont chiffrés avec DPAPI (Windows).
- **Statut des serveurs** et **actualités Dofus**.

## Structure

- `docs/` : specs issues du reverse engineering (avec rea) de l'Ankama Launcher 3.15.5 et plan d'implémentation.
- `core/` : crate `dofus_core`, toute la logique (auth, cytrus, game, zaap, extras). Tests : `cargo test -p dofus_core`.
- `app/` : application Tauri (commandes dans `src-tauri/`) et interface SvelteKit (`src/`).

## Développement

```sh
cd app && npm install
npx tauri dev          # dev
npx tauri build        # installeur NSIS dans target/release/bundle/nsis
```

Données du launcher : `%APPDATA%\dofus-launcher` (réglages, comptes, logs). Jeu : `%LOCALAPPDATA%\Ankama\Dofus-dofus3` par défaut.
Le dossier d'installation reste compatible avec l'Ankama Launcher officiel (`manifest.json`, `.release.infos.json`).
Ne lancez pas les deux launchers en même temps : ils utilisent tous les deux le port 26116.
