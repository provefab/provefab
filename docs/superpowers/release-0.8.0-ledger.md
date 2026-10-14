# Release 0.8.0 ledger

Plan: docs/superpowers/plans/2026-10-14-release-0.8.0.md

- Autorisation: « Continue alors » sur publication coordonnée CLI+Pro+site, après clarification que build local n'est pas release distribuable. Pas de confirmation répétée nécessaire pour ces actions.
- Bases: core99545d1, Pro6fa43da, landingcaed0dc. Original core main1fdc3e3 et Pro clean ; landing original contient docs/distribution non suivi, à préserver.
- Ruling: version0.8.0 choisie pour feature et changement de compatibilité pré1.0 ; ne pas réécrire0.7.0. Coût si inadéquat: numéro plus visible, sans perte de données.
- Ruling: Pro pointe sur commit git immuable du cœur release, publié d'abord sur branche de préparation. Évite dépendance locale et attente d'un tag final avant tests Pro. Le tag0.8.0 doit viser ce même code. Coût: SHA moins lisible qu'un tag, compensé par README/notes.
- Worktree Pro créé sibling provefab-pro-sandbox, branche release/native-sandbox-0.8.0 ; même convention isolée que core/site. Pas de mutation du main Pro.
- Préflights: gh auth valide avec accès repo/workflow ; Wrangler4.69.0 OAuth compte configuré disponible ;5 identités codesigning valides et notary profile provefab-notary retourne26 soumissions passées. Aucune clé/token affiché en clair. Cibles Rust macOS arm64+x86_64 installées.
- Pro baseline en cours, log /private/tmp/provefab-release-0.8.0/pro-baseline.log.
- Pro baseline terminé110/110,0skip3.746s. Task1 déléguée à release_sources, corepréparation puis Pro après SHA publié.
- L'outil CUA a refusé Google Chrome (non autorisé), aucun contournement tenté. Aucun CLI/token Polar d'environnement disponible. L'utilisateur a choisi explicitement de déposer les archives Pro lui-même sur Polar. L'agent doit fournir les trois fichiers vérifiés ; site publié seulement après confirmation factuelle du dépôt, sans demander à nouveau une autorisation de déployer déjà donnée.
- Core release figé4d5556c883aa1db77c1908f289ddc66d65b988ed, préparation0.8.0 (version/lock/README/notes seulement). Agent fmt/clippy804nextest+16skip verts, dernier48.137s sans LEAK après diagnostic isolé d'un warning transitoire déjà documenté.
- Branche core feat/native-sandbox poussée, workflow_dispatch CI38539692861 lancé sur ce SHA. Ruling: prévalider les builds Linux sur branche avant main/tag, même source exacte, pour détecter incompatibilités avant publication. Coût: un run CI supplémentaire, pas de nouvelle version publique. Tag/main restent non publiés à ce stade.
- Signature/notarisation core démarrée via script existant, aucun dist0.8.0 préexistant. La production déjà revue est inchangée par la préparation release ; les archives ne sont pas publiées avant revue finale et validations. Log /private/tmp/provefab-release-0.8.0/core-macos.log.
- Remote landingmain7f9156d est ancêtre de la branche locale, pas de divergence ; les deux commits locaux de lancement antérieurs sont conservés. Remote Promain6fa43da correspond au baseline.

## Scan de cohérence
| Tâche(s) | Relation | Conclusion |
|---|---|---|
| 1 | Version/docs/gitrev et tests Pro | SHA core connu seulement après commit ; handshake push contrôleur explicite, pas de placeholder laissé dans Cargo.toml |
| 2 | Pin/source générée/checks | Consomme SHA1, aucun runtime modifié |
| 3 | CI/signature/publication | Consomme sources revues1/2 ; builds précèdent tags/releases, aucun asset ancien écrasé |
| 4 | Site publié/téléchargements | Dépend de3, bloque si Pro non livrable pour éviter promesse fausse |
| 1,2 | coreREADME -> site generated | Pin exact après commit release |
| 1,3 | coreSHA -> Pro dependency -> archives | Même source verrouillée dans Cargo.lock, aucune path dependency finale |
| 2,4 | site pin -> deploy | Dist reconstruit, pin rendu accessible avant merge site |
| 3,4 | release latest/Polar -> site | Vérifier archives et livraison Pro avant annonce |

## Avancement release
- Task 1: complete (core df622a4..4d5556c, Pro6fa43da..f85a86f, revue spec et qualité approuvée sans finding). Rapport scratch task-1-review.md.
- Vérification indépendante après agent Pro: fmt/clippy verts, nextest110/110 en4.950s, vrai binaire0.8.0. Log /private/tmp/provefab-release-0.8.0/pro-independent.log.
- Core macOS universel signé et notarisé Accepted, submission8ba14dbc-4078-4f5b-830e-cee73e1eb627. Vérifications indépendantes codesign strict, lipo x86_64+arm64, version0.8.0 et checksum OK. SHA256540ee48d64c999c7ceb95b07fbe8d6296b30f7df3e928ef5ef240ead8e309619.
- Pro branche de préparation poussée f85a86f, workflow_dispatch38540548462 ; macOS script en cours. Core CI38539692861 check réussi, deux builds musl en cours.
- Task2 release_site démarrée sur landingcaed0dc pour pin exact4d5556c et génération/tests.
- Vérification indépendante core après agent: fmt, clippy all-targets/all-features, nextest804/804 et16skip65.297s, aucun LEAK observé. Log /private/tmp/provefab-release-0.8.0/core-independent.log.
- Pro macOS universel signé, Apple Accepted submission126bce16-53aa-4015-9b16-c1c71ceb4edc. Vérifications indépendantes codesign strict, architecturesx86_64+arm64, version0.8.0, checksum OK389798d67b64d00dc21a0645512e96f71c7bd4e10407f0e6a1c484b9ee6d4937.
- Core CI38539692861 réussie: check et builds musl x86_64/aarch64, tests static x86. Téléchargement des artefacts de prévalidation dans ../release-artifacts/0.8.0/core-preflight.
- Ruling: créer la release GitHub en brouillon après tag vérifié, laisser le workflow tag ajouter Linux, puis publier seulement une fois les six assets vérifiés. Évite un latest ne contenant que macOS. Coût si mauvais: attente supplémentaire, aucun asset précédent écrasé.
- Task2 implémentation58f0573 exactement gitlink, tests/build verts et HTTP local5routes/13assertions ; revue finale des sources en cours.
- Vérification indépendante site après agent116tests + build23pages + liens/legal/SEO/contenu verts. Log /private/tmp/provefab-release-0.8.0/site-independent.log.
- Revue finale finding Important: install.sh conseillait encore login claude malgré refus0.8.0. Fix wave confiée à release_site, limitée au texte des étapes suivantes et tests directs, avant publication. Ruling: corriger cette copie existante car elle fait partie de la promesse installation0.8.0 ; logique téléchargement/checksum inchangée. Coût: quelques lignes et vérification site supplémentaires.
- Archives core préflight vérifiées localement: SHA256 et ELF architecture correcte et INSTALL0.8.0. ARM ed35a02ea15769858a52fe1b30a10dded1e7cc4faac602d79ed6cb14d2743900 ; x86 2513184d2244fa423fe18b97a390645765288bf77032c8566071a6908b57afee. Artefacts finaux tag pourront différer (timestamps tar), seront vérifiés séparément.
- Task 2: complete (landingcaed0dc..58f0573 puis fix83bbabc, I1 résolu ; conformité et qualité coordonnées approuvées). Revue finale source release propre ; aucun nouveau finding. Vérification indépendante site après fix116tests/build/links/legal/SEO/contenu verts, log site-final-independent.log.
- Core main fast-forward1fdc3e3..4d5556c poussé, tag annotév0.8.0 vise4d5556c. Release GitHub en brouillon avec macOS+checksum. Workflow tag38541643646 doit construire/joindre Linux avant publication latest ; aucun asset existant0.7.0 touché.
- Pro CI38540548462 réussie: check110/110 et tests staticx86 110/110, builds deux architectures verts. Archives SHA256/ELF/INSTALL0.8.0 vérifiées, réunies ../release-artifacts/0.8.0/pro. ARM851129d84b764e9bbf7d5115170e93dcca7e916976beee054c2cadec597b03f9 ; x86 79562d766a2205eba01f5da71705452de12836a086d843e4fb9f9f19cfc8d257 ; mac389798d67b64d00dc21a0645512e96f71c7bd4e10407f0e6a1c484b9ee6d4937.
- Pro main fast-forward6fa43da..f85a86f poussé ; tag annotév0.8.0 vise même source. Trois liens archives remis utilisateur pour upload Polar manuel, confirmation factuelle demandée et en attente. Aucun dépôt supposé.
- Simulation Cloudflare du site final réussie: wrangler deploy --dry-run, Worker2.32KiB et bindingASSETS. Aucune mise en production ; log /private/tmp/provefab-release-0.8.0/site-deploy-dry-run.log.
- Core tagCI38541643646 entièrement réussie, six assets joints au brouillon puis retéléchargés/vérifiés avant publication. Release publique https://github.com/provefab/provefab/releases/tag/v0.8.0 à2026-10-14T09:31:10Z, latest confirmé anonymement. Tous six téléchargements publics accessibles et SHA256 valides. Hash ARMfinal7eab5ea6ee8248e74aa08dbb1c28445b9b526a64a8c41d5a4cfa6c8ea7d6ec3f ; x86final190b7c73f08fce1f86f7f42cd5615f7a450d3c6004664f187f68802703cb55d0 ; mac inchangé540ee48d64c999c7ceb95b07fbe8d6296b30f7df3e928ef5ef240ead8e309619.
- Installation réelle avec install.sh final et endpoints publics, PROVEFAB_INSTALL_DIR=/private/tmp/provefab-release-0.8.0/public-install : succès, version0.8.0 et codesign strict valides. Aucun binaire utilisateur ni profil shell modifié. Log public-install.log.
- Premier fetch urllib a échoué car Python3.14 ne trouve pas son certificat racine local ; vérification poursuivie via curl normal avec validation TLS, aucun contournement TLS.
- Pro tagCI38542454840 répète la validation sur le même SHA f85a86f, en cours ; les trois archives remises proviennent du workflow_dispatch38540548462 entièrement vert et de macOS signé/notarisé. TagCI ne publie ni ne remplace de fichier Polar.
- Restant: confirmation utilisateur du dépôt effectif des trois archives Polar, puis FF/push landingmain, wrangler deploy, contrôles live. Site source final83bbabc, branche de préparation poussée ; aucun déploiement production présumé. Revue navigateur Chrome indisponible (refus CUA antérieur), à rapporter si toujours indisponible au contrôle live.
