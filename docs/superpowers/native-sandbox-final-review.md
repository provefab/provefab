# Relecture ciblée finale I1/I2

14 octobre 2026. Périmètre strict : findings I1/I2 de `final-review.md` et régressions introduites par leur correctif, sans nouvel audit global.

Ancrages relus : core `9c2c6c5..ba98690fa5fd0e097d3a2ff38760bbd33dc106c9`, micro correction documentaire `ba98690..19ad4743435147df2af82767e2bb99debfeff406`, landing `c54c2e9..caed0dcf8be7c2342e2d185608db3af950386166`. Le submodule landing pointe exactement sur `19ad4743435147df2af82767e2bb99debfeff406` et le checkout landing est propre.

## Verdicts

- **I1 : ADDRESSED.**
- **I2 : ADDRESSED.**
- **Conformité : APPROUVÉE. Qualité : APPROUVÉE.**
- Aucun nouveau problème Important ou Critical identifié dans les diffs correctifs. Prêt pour intégration au regard de cette revue ; aucune autorisation de publication/déploiement n'est impliquée.

## I1 : résolution des chemins de confiance

`crates/agent-workers/src/sandbox.rs:568-591` applique désormais `trusted_path_target` avant canonicalisation à chaque entrée trusted/secrets. Pour une feuille absente, le contrôle remonte par `symlink_metadata` au plus proche ancêtre existant puis inspecte tous ses composants développés contre cwd et sandbox-tmp. Les erreurs d'inspection refusent. Les checks d'intersection après canonicalisation restent présents.

Les aliases externes stables peuvent rester utilisés par les consommateurs : leur résolution ne traverse plus une racine writable acceptée. Aucun composant mutable n'est effacé silencieusement par canonicalisation avant la vérification. La stabilité face à une modification administrateur externe n'est pas revendiquée.

Régressions relues : `sandbox.rs:1403` couvre trusted, secret existant et absent à plusieurs niveaux via chacune des deux racines ; `sandbox.rs:1432` conserve un contrôle positif d'alias externe stable avec règles canonicalisées ; `tests/workers.rs:97` vérifie le refus réel Pi avant marqueur du probe. Le rapport durable consigne les rouges sur le code précédent puis les verts.

## I2 : aucun stage Claude accepté sans frontière vérifiée

`crates/agent-workers/src/claude.rs:24-30` retourne une erreur inconditionnelle avant toute préparation ou probe, avec orientation Codex/Pi et instruction de conserver les données utilisateur. `run` propage l'erreur à la ligne 42 avant hooks, transcript et exécution. Les anciens générateurs de commandes et préflights Claude ont été retirés ; leurs tests d'acceptation ne décrivent plus un mode supporté dormant.

`tests/workers.rs:137` vérifie prepare/run avec HOME propre, profil settings vide et les quatre ToolProfiles, dont NoTools : aucun marqueur version/auth/stage, événement ou répertoire session, et profil inchangé. La fixture native exige désormais le motif universel, indépendamment de l'état host. Réouvrir Claude demande une frontière de contexte vérifiée compatible avec auth/hooks et une véritable exécution supportée ; aucun raccourci bare/safe-mode n'a été ajouté.

## Risques de régression contrôlés dans le correctif

1. **Alias stable ou feuille absente refusés à tort :** contrôle positif conservé et assertions sur destinations canonicalisées ; suite native Codex avec profil dédié réel réussie.
2. **Un profil Claude échappe au refus :** erreur inconditionnelle unique, appelée avant toute opération par Worker.run ; matrice des quatre profils relue.
3. **Exemple actif ou init modifiés accidentellement :** exemple Codex standard/frontier ; aucune logique init/routing modifiée. Le test `setup.rs:770` vérifie désormais les entrées init par valeurs complètes indépendantes et garde l'égalité Codex/jev avec l'exemple. Les docs exigent de remplacer les anciennes entrées Claude.
4. **Copie et pin annoncent encore un mode Claude accepté :** Hero, FAQ, métadonnées, Markdown et notes de support relus. La source générée est épinglée au core corrigé. Les preuves antérieures restent clairement historiques dans les rapports.
5. **Instruction init/Pi erronée introduite :** Minor signalé à `docs/guide/agents.md:29`, corrigé dans `19ad474` : le workflow init dirige désormais vers Codex et précise qu'un setup Pi-only demande un catalogue manuel. **Résolu**, aucun finding ouvert.

## Vérification lue, sans répétition des suites

- Implémenteur : fmt et clippy workspace/all-targets réussis ; nextest **804/804, 16 ignorés**, sans LEAK au dernier run ; natif **3/3** ; build release offline réussi. Journaux de fin consultés et rapport `final-fix-report.md`/validation durable relus.
- Contrôleur après gel production : fmt/clippy réussis ; `nextest-reviewed.log` **804/804, 16 ignorés**, avec **1 LEAK** sur `claude::tests::maps_max_turns_and_rate_limits`. `leak-reviewed.log` : rerun isolé **1/1**, sans LEAK. Cause inconnue ; le corps du test de parsing ne lance aucun processus selon la vérification du contrôleur. Ne pas convertir cette observation en preuve de nettoyage universel ni prétendre un run global sans warning.
- `native-reviewed.log` : suite native indépendante **3/3 en 15,33 s**, aucun échec. Linux reste non vérifié et la preuve du patch garde sa distinction hook/source OS/absence d'expérience de course.
- Landing après pin final : rapport durable **116/116**, build **23 pages**, **21 copies Markdown**, liens/legal/SEO/contenu valides. Dernier rerun indépendant après `caed0dc` terminé exit0 avec les mêmes résultats ; fin du journal `/private/tmp/provefab-final-checks-20261014/site-reviewed.log` consultée.
- Mobile : `/private/tmp/provefab-mobile-final/results.json` relu. Landing/configuration/sécurité : largeur 390 et bodyWidth 390. Menus docs réellement ouverts puis refermés ; les booléens mesurent `:popover-open`, pas aria-expanded. Captures inspectées visuellement par le contrôleur dans une session Chrome headless séparée, sans profil/focus utilisateur. Cela clôt la limite mobile initiale ; thème sombre absent de la configuration LightTheme.

Révision en lecture seule : aucun code/index/HEAD modifié, aucune nouvelle démonstration de contournement, aucun appel modèle ou sous-agent. Seul ce rapport de relecture est écrit.
