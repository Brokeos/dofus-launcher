# Mentions légales

## Non-affiliation

Ce projet est indépendant. Il n'est ni développé, ni approuvé, ni soutenu par Ankama. « Ankama », « Dofus », « Zaap », « Haapi », ainsi que les logos, personnages, sprites et illustrations associés, sont des marques ou des œuvres protégées appartenant à leurs titulaires.

## Rétro-ingénierie

Les documents de `docs/` décrivent le fonctionnement de l'Ankama Launcher 3.15.5 (authentification, mises à jour Cytrus, protocole local Zaap). Ils sont issus d'une rétro-ingénierie réalisée avec l'outil rea, dans un seul but : **l'interopérabilité**, c'est-à-dire permettre à un programme indépendant de dialoguer avec les services et le jeu officiels.

- L'analyse est **statique** : lecture du code JavaScript du launcher installé sur la machine de l'auteur.
- Aucun identifiant, jeton, clé d'API ni fichier d'utilisateur n'a été lu, déchiffré ou envoyé pour produire ces documents.
- Le code source du launcher d'Ankama n'est **pas redistribué** dans ce dépôt (le dossier `re/` est exclu par `.gitignore`). Le code de ce projet est une réécriture indépendante ; les documents décrivent des comportements et des formats, pas des copies de code.
- Aucune protection technique (chiffrement, anti-triche, DRM) n'est contournée. Les mêmes appels réseau que le launcher officiel sont réalisés, avec le compte et l'accord de l'utilisateur.

Dans l'Union européenne, la décomposition d'un programme à des fins d'interopérabilité est encadrée par l'article 6 de la directive 2009/24/CE (en France, article L122-6-1 IV du Code de la propriété intellectuelle). Ce texte n'est cité qu'à titre d'information et ne constitue pas un avis juridique.

## Contenus tiers inclus

- `app/static/dofus/` : images issues de Dofus (logo, têtes de classes, symboles, illustrations). Elles restent la propriété d'Ankama et ne sont pas couvertes par la licence du code.
- `core/tests/fixtures/` : extraits de données publiques du CDN d'Ankama (manifestes Cytrus, configuration de lancement), utilisés uniquement pour les tests.

Si vous êtes titulaire de droits sur l'un de ces éléments et souhaitez son retrait, ouvrez une issue sur le dépôt : il sera retiré rapidement.

## Usage et responsabilité

- Usage personnel, sans but commercial. Aucune donnée n'est collectée ni envoyée ailleurs que vers les services officiels d'Ankama.
- L'utilisation d'un launcher tiers peut ne pas être permise par les conditions d'utilisation d'Ankama. Vous l'utilisez à vos risques, y compris pour votre compte de jeu.
- Le logiciel est fourni « tel quel », sans garantie d'aucune sorte. L'auteur n'est pas responsable des dommages, pertes de données ou sanctions liés à son utilisation.
