# Release 0.8.0 ledger

Plan: docs/superpowers/plans/2026-10-14-release-0.8.0.md

- Autorisation: « Continue alors » sur publication coordonnée CLI+Pro+site, après clarification que build local n'est pas release distribuable. Pas de confirmation répétée nécessaire pour ces actions.
- Bases: core99545d1, Pro6fa43da, landingcaed0dc. Original core main1fdc3e3 et Pro clean ; landing original contient docs/distribution non suivi, à préserver.
- Ruling: version0.8.0 choisie pour feature et changement de compatibilité pré1.0 ; ne pas réécrire0.7.0. Coût si inadéquat: numéro plus visible, sans perte de données.
- Ruling: Pro pointe sur commit git immuable du cœur release, publié d'abord sur branche de préparation. Évite dépendance locale et attente d'un tag final avant tests Pro. Le tag0.8.0 doit viser ce même code. Coût: SHA moins lisible qu'un tag, compensé par README/notes.
- Worktree Pro créé sibling provefab-pro-sandbox, branche release/native-sandbox-0.8.0 ; même convention isolée que core/site. Pas de mutation du main Pro.
- Préflights: gh auth valide avec accès repo/workflow ; Wrangler4.69.0 OAuth compte configuré disponible ;5 identités codesigning valides et notary profile provefab-notary retourne26 soumissions passées. Aucune clé/token affiché en clair. Cibles Rust macOS arm64+x86_64 installées.
- Pro baseline en cours, log /private/tmp/provefab-release-0.8.0/pro-baseline.log.

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
