# Vague finale I1/I2

14 octobre 2026. Core `/Users/antoinehoriot/Projects/provefab/provefab-sandbox`, branche `feat/native-sandbox`, base initiale `1a9a8b4`, puis ancrage contrôleur `9c2c6c5`. Landing sibling `/Users/antoinehoriot/Projects/provefab/landing-sandbox`, base `c54c2e9`. Aucun sous-agent, push, merge, déploiement, global install, contrôle du Chrome utilisateur ou modification des main repos.

Budget : exactement une vague pour les deux findings Important I1/I2 de `final-review.md`. Aucun nouveau runtime, architecture, image, prix, branding ou layout. Les seules nouvelles fixtures utilisent des fichiers factices locaux et marqueurs inoffensifs. La suite native existante et approuvée a été répétée, aucune nouvelle démonstration offensive ni lecture de vrais secrets. Instructions AGENTS fournies, TDD + writing-good-tests, systematic-debugging et verification-before-completion lus. Aucun AGENTS.md supplémentaire trouvé dans les repos concernés.

## I1 : fermeture des composants trusted/secrets

Ancrage lu directement : `prepare_request`, `trusted_path_target`, préflight des sources contexte Codex, consommateurs Pi/Codex et ancien Claude. Le walker existant inspecte chaque prefix développé avant le suivi du lien, refuse les deux racines writables et borne les loops. La vérification lexicale précédente de `prepare_request` ne voyait pas un alias externe redirigé par un composant interne vers une destination externe.

Correctif : utiliser ce même walker pour les entrées trusted et secrets avant la canonicalisation finale. Pour une feuille absente, remonter au plus proche ancêtre existant par symlink_metadata. Les feuilles trusted absentes suivent aussi cette vérification, conservant la compatibilité des profils/fixtures existants. Les deux checks d’intersection canonicalisés restent. Décision : conserver les aliases externes stables aux consommateurs, car aucun composant de leur résolution acceptée ne traverse cwd ou sandbox-tmp. Les commandes sandboxées ne peuvent donc pas modifier ces composants. Aucun nouveau mécanisme de pin n’est nécessaire pour cette frontière, aucune garantie face à un administrateur concurrent n’est ajoutée. Décision et pourquoi commentés au site.

Régressions :

- `adapter_inputs_reject_indirect_mutable_aliases_including_absent_secrets` : trusted, secret existant et secret absent à plusieurs niveaux ; alias externe -> composant cwd ou sandbox-tmp -> destination externe.
- `stable_external_adapter_aliases_allow_present_and_absent_secrets` : alias externe stable accepté, denyWrite canonicalisé et denyRead des feuilles présentes/absentes vérifiés.
- `trusted_adapter_aliases_refuse_both_writable_roots_before_version_probe` : véritable PiWorker.prepare_command refuse les deux variantes avant un fake SRT version marker.

Rouges : `cargo test -p agent-workers adapter_inputs -- --nocapture`, exit101, « mutable adapter alias accepted » ; `cargo test -p agent-workers --test workers trusted_adapter_aliases -- --nocapture`, exit101, Command acceptée et probe fake invoqué. Le contrôle positif initial avait une assertion incorrecte sur la conservation du chemin lexical dans denyRead ; la normalisation existante canonicalise cette liste. L’assertion a été corrigée vers les destinations littérales attendues, sans changement de cette normalisation. Verts : `cargo test -p agent-workers adapter_ -- --nocapture`, deux unitaires et une intégration réussis, puis suite adaptateurs complète/final nextest.

## I2 : refus Claude universel

Ruling contrôleur lu dans le ledger : refus temporaire de chaque stage avant les probes version/auth, jusqu’à vérification exacte de tous les chargements de contexte compatibles avec auth et guard. Pas d’expérimentation bare/safe-mode ni de suppression de state.

`ClaudeCodeWorker.prepare_command` retourne immédiatement un io::Error indiquant que la frontière implicit host context n’est pas vérifiée et demandant de choisir Codex ou Pi. L’erreur arrive avant préparation filesystem, probes et stage. Worker.run propage ce même refus avant hooks/transcript. Les champs publics et le protocole/parser existants sont conservés ; le parser reste directement exercé par ses tests. Les anciens builders args/env, générateur settings Claude et préflights auth/settings/managed host devenus inutiles ont été retirés avec leurs tests. Aucun allow(dead_code), helper test-only production ou faux mode accepté dormant n’a été introduit. Les protections des defaults temporaires SRT/Claude restent dans la politique commune : leur runtime SRT utilise encore ces conventions, leur retrait élargirait les écritures.

Nouvelle régression `claude_all_profiles_refuse_before_version_auth_or_stage_invocation` : subprocess HOME propre, profil dédié settings {}, fake version 2.1.296/personal Max, quatre ToolProfiles y compris NoTools. Prepare et run doivent refuser avec orientation Codex/Pi, aucun fake CLI marker, événement, session créé ou modification du profil. Rouge sur l’ancien code, exit101 : une Command Claude est effectivement produite sous ce profil propre. Vert dans la suite complète adaptateurs **69 unitaires +14 intégrations**. Dirty-settings/state fixtures restent comme cas de refus avec motif universel ; les anciennes attentes de succès Claude et le test de modes particuliers sont supplantés par ce contrat universel.

L’ancienne fixture native `native_pi_bash_and_claude_refusal` exige maintenant le motif de frontière contexte universelle et l’orientation Codex/Pi, aucune dépendance à la présence de state host pour obtenir son succès.

## Synchronisation produit et prémisse du test init

README, security/configuration/agents/usage, exemple actif, spec et validation durable synchronisés. Les blocs actifs Claude sont retirés de l’exemple, qui fournit Codex standard/frontier. Pourquoi : un simple commentaire laissant ces modèles routables n’empêcherait pas leur sélection. Aucun modèle Pi/provider inventé : l’utilisateur est orienté vers le catalogue Pi installé pour ajouter un second fournisseur. Les docs demandent explicitement de remplacer/supprimer chaque bloc claude-code d’un catalogue existant ou créé par init ; login/doctor ne prouvent pas le support d’un stage.

Le premier full nextest a trouvé `setup::tests::the_catalog_comes_from_the_worker_clis_on_path` en échec. Prémisse vérifiée dans le source : init utilise des constantes CLI-detection, ne lit pas le fichier exemple à runtime. Son test supposait encore toutes ses entrées identiques à l’exemple copiable, volontairement devenu Codex-only. Correctif autorisé par le contrôleur : attentes complètes init indépendantes/littérales et égalité Codex + jev avec exemple conservée. Logique init/routing inchangée. Ciblé1/1 réussi ; ce n’est pas une assertion retirée pour masquer un changement de comportement.

## Checks et preuves fraîches core

| Commande | Résultat | Journal |
|---|---|---|
| `cargo test -p agent-workers -- --nocapture` | 69 unitaires +14 intégrations, exit0 | `/private/tmp/provefab-final-fixes-workers.log` |
| `cargo test -p provefab setup::tests::the_catalog_comes_from_the_worker_clis_on_path -- --exact` | 1/1 effectif, exit0 ; les autres binaires affichent0 filtré | `/private/tmp/provefab-final-fixes-setup.log` |
| `cargo fmt -- --check` | exit0 après dernier format et avant commit | sortie terminal |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit0,5,08s, aucun warning | `/private/tmp/provefab-final-fixes-clippy-final.log` |
| `cargo nextest run` final | 804/804,16ignorés,53,519s, exit0, aucun LEAK/FAIL/WARN/ERROR | `/private/tmp/provefab-final-fixes-nextest-final.log` |
| native suite explicite existante | 3/3,20,89s,exit0 | `/private/tmp/provefab-final-fixes-native.log` |
| `cargo build --release --locked --offline -p provefab` | exit0,18,18s | `/private/tmp/provefab-final-fixes-release.log` |
| `git diff --check` | exit0, sources/docs réouverts | sortie terminal |

La première vérification fmt a signalé uniquement le wrapping de la nouvelle assertion native ; cargo fmt puis check vert. Le premier full nextest s’est arrêté sur la prémisse init ci-dessus :466 tests,465 passés dont1leaky,1échec,338 non exécutés. LEAK signalé sur le parser Codex `failed_commands_turn_failures_and_usage_limits`, dont le corps ne lance aucun processus. Diagnostic isolé via `cargo nextest run -p agent-workers -E 'test(codex::tests::failed_commands_turn_failures_and_usage_limits)'` :1/1, aucun LEAK ; puis full final sans récidive. Cause initiale inconnue, aucune preuve universelle de nettoyage déduite. Log diagnostic `/private/tmp/provefab-final-fixes-leak-diagnostic.log` ; ancien échec `/private/tmp/provefab-final-fixes-nextest.log`. Total816 ->804 dû au retrait des anciens tests Claude avec leurs helpers/modes acceptés devenus obsolètes et à leur remplacement par les régressions universelles.

Native exacte : `PROVEFAB_NATIVE_SRT=/private/tmp/provefab-sandbox-eval-20261013/node_modules/.bin/srt`, Git/Python/Codex/Pi/profile paths du bloc durable, vrai Node24.21.0 `/Users/antoinehoriot/.vite-plus/js_runtime/node/24.21.0/bin/node`, modèle demandé gpt-5.5 ; `cargo test -p provefab --test native_sandbox -- --ignored --nocapture --test-threads=1`.

Artefacts relus : core `/private/tmp/pf-core-ViIKhp/evidence.json` ; Codex `/private/tmp/pf-codex-Bwi8eu/evidence.json` ; Pi `/private/tmp/pf-pi-LZYzTC/{pi-output.txt,claude-refusal.txt}`. Contrôle positif core17op/HTTP2/ref réelle ; sandbox core/Codex/Pi17résultats attendus/HTTP0/ref absente ; validateurs inchangés, gate exit1. Patch ordinaire event réellement terminé et bytes changés, protected apply_patch appel+sortie hook exacts dans rollout de ce test. Toujours pas de nouvelle expérience de race/refus OS isolé patch, source fs-helper0.162.1 distincte. Claude refuse universellement sans probe, message relu.

## Cinq risques vérifiés

1. Un alias indirect effacé par canonicalization : tests trusted/secret existant/absent, chacun des deux roots ; walker appliqué avant canonical_path dans prepare_request.
2. Sur-refus des aliases externes stables ou feuilles absentes : contrôle positif avec deux feuilles, rules canonicalisées ; suite adapters et native Codex profil réel verts.
3. Claude clean/NoTools encore accepté ou probé : fake log couvre toutes invocations, prepare/run quatre profils, session/profile immuables ; native motif universel.
4. Exemple sélectionnant encore Claude ou init silencieusement modifié : blocs actifs Codex seulement, test init complet maintenu ; docs action explicite sur catalogues init, aucune logique changée.
5. Promesses dépassant la preuve : Linux, Keychain search API, secrets arbitraires, dépendances transitives, concurrents administrateurs, race patch et anciennes preuves host-state restent clairement limités/historiques.

## Commits et landing

Core code/tests/docs commit FIRST : **`ba98690fa5fd0e097d3a2ff38760bbd33dc106c9`**, 13 fichiers explicitement indexés, worktree clean après commit. Le contrôleur a reçu cet ancrage avant l’arrêt du rapport pour commencer sa revue et ses reruns indépendants.

Micro correction docs après re-review : **`19ad4743435147df2af82767e2bb99debfeff406`**. Le workflow agents exit4 recommandait Pi alors qu’init ne détecte que Claude/Codex. Source revérifiée, texte corrigé pour installer Codex dans ce workflow, avec catalogue manuel pour Pi-only. Code/tests inchangés, aucun changement de logique init. Fichier rouvert, diff check vert, core clean. Ce SHA est le product-source final.

Landing `vendor/provefab` fetch local du core, detached checkout du product-source final **19ad474**, aucun fetch distant. Textes existants Hero/FAQ/meta/Markdown et notes de support des guides synchronisés ; anciennes consignes de login incompatibles corrigées. Aucun prix/layout/image changé. `pnpm --filter site test` final après repin réussi :**116/116**, build**23pages/21copies Markdown**, links/legal/SEO/content OK, exit0. Log `/private/tmp/provefab-final-fixes-site-final.log`. Le premier run avant micro correctif était également vert (`/private/tmp/provefab-final-fixes-site.log`). Le build inclus est la reconstruction finale, aucune suite non affectée artificiellement répétée. Warning i18n préexistant uniquement.

Sources et output générés réouverts : HTML landing agents Codex/Pi + Claude indisponible, Markdown landing/config/security refus universel + motif ; exemple actif codex-frontier ; guide agents final Codex workflow/init + Pi-only catalogue manuel. Assert script sur ces contenus exit0. Scan lignes ajoutées sans tiret cadratin, `git diff --check` et staged check verts. Preuve durable actuelle dans `docs/superpowers/native-sandbox-copy-validation.md`, historique Task4 distingué.

Commit landing final : **`caed0dcf8be7c2342e2d185608db3af950386166`**, 13fichiers explicitement indexés incluant le submodule exact19ad474. Landing et submodule clean. Tous fichiers owned core sont committés à19ad474 ; le dernier status signale seulement le ledger en cours du contrôleur, non édité par cet agent. Le rapport est ignoré localement. Les futurs commits contrôleur ledger/review-only ne nécessitent pas de repin.

Aucun contrôle navigateur effectué par cet agent. Le contrôleur a vérifié séparément le rendu en Chrome headless avec profil temporaire supprimé ensuite, sans contrôle du Chrome utilisateur. Résultats `/private/tmp/provefab-mobile-final/results.json` relus, captures landing/configuration/sécurité à390px, bodyWidth390. Menu docs réellement ouvert puis refermé avec état popover mesuré sur config et sécurité. Capture configuration ouverte inspectée également par cet agent via fichier local. Attente initiale aria-expanded incorrecte corrigée après lecture de source Starlight popovertarget ; pas de défaut produit masqué. Cette preuve ferme la limite mobile initiale, gardée historique courte dans la preuve durable. Thème sombre non applicable au site LightTheme, aucun PNG commité.

Vérification indépendante contrôleur, rapportée après arrêt code/tests : fmt/clippy verts, nextest804/804+16skip avec1LEAK parser pur puis diagnostic1/1clean, native3/3 en15,33s. Cela est distinct de notre full final sansLEAK et ne prouve pas une cause résolue du signal intermittent. Release final19ad474 recompilé par le contrôleur15,57s, `--version`0.7.0, SHA256 `e0f2e4995ac73bc00ba90e1b59ace29802a95c5d52a7dc2bc4526e53e7ba837f`, aucune distribution publiée/signée/universal revendiquée. Notre propre release démarre aussi en0.7.0.

Le reviewer scoped a indiqué I1/I2 addressed, aucun Important/Critical, minor agents résolu ; il attendait seulement les commits et ce rapport gelés pour son ancrage final. Arrêt des modifications. Aucun travail hors vague bornée ; limites Linux/Keychain/secrets arbitraires/dépendances transitives/concurrents admin/course patch maintenues. Aucune autre édition production, push, merge ou déploiement prévue dans cette sous-tâche.
