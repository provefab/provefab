# Revue globale native sandbox

2026-10-14. Core `1fdc3e3..8dacc5f`, landing `2d1ed39..c54c2e9`. Revue en lecture seule des sources de production, des diffs fournis, du spec/plan/ledger, des tests natifs et des preuves déjà exécutées. Aucun nouveau scénario de contournement, accès à des secrets réels, exécution modèle, changement de checkout/index/HEAD ou sous-agent. Les deux seuls fichiers écrits par cette revue sont les rapports dans ce dossier ignoré.

## Verdict

**Conformité globale : corrections requises. Qualité globale : corrections requises. Prêt à intégrer : non.**

Task 4 seule reste approuvée dans `task-4-review.md`. Deux problèmes **Important** de fermeture des chemins/configurations restent dans le code global. Aucun Critical établi. La preuve native macOS existante reste valable pour les fixtures réellement exécutées ; elle ne couvre pas les cas ci-dessous.

## Points solides

- Une seule dérivation de politique relie le pipeline, les gates ordinaires/reproduction/risque/post-merge/revert, les reviews PR et la maintenance NoTools. L'option de réseau local refuse explicitement toutes les plateformes.
- Codex contrôle sa version exacte, ses sources système/gérées/cloud, ses hooks et son contexte avant le démarrage host ; Pi n'enregistre plus les fabriques de fichiers non sandboxées.
- Les tests natifs vérifient les témoins HTTP/Git, les octets des validateurs et les événements terminés des outils. Ils distinguent correctement le refus du hook apply_patch de la preuve OS issue de la revue du helper exact.
- La documentation nomme les limites : Linux non vérifié, aucune exécution Claude réussie constatée, protection explicite sans fermeture automatique des dépendances, socket macOS long susceptible de refuser.

## Important I1 : chemins de confiance non épinglés après une traversée indirecte mutable

**Source :** `crates/agent-workers/src/sandbox.rs:568-574`, `crates/agent-workers/src/pi.rs:63-65,79,89,98`, `crates/agent-workers/src/claude.rs:105-116`, `crates/agent-workers/src/codex.rs:100`.

`prepare_request` vérifie les préfixes lexicaux par `reject_worktree_symlink`, puis canonicalise seulement le résultat final. Contrairement à `trusted_path_target` dans ce même module, ce contrôle ne développe pas chaque cible de lien. Une entrée de confiance externe dont la résolution traverse un lien situé dans le worktree ou son temp privé, puis revient vers un fichier externe, n'est donc pas reconnue comme mutable. Le contrôle d'intersection final voit uniquement la destination externe et la liste deny_write protège uniquement cette destination.

Les consommateurs conservent pourtant le chemin original : Pi charge son extension/skills avec `self.package` et invoque le guard avec `self.provefab_bin`; Claude utilise `self.plugin_dir`, `self.config_dir` et `self.provefab_bin`; le guard Codex reçoit aussi le chemin original du binaire. Le composant intermédiaire dans une racine writable reste modifiable après la préparation. Ce n'est pas le cas du runtime principal, qui est effectivement lancé sous son chemin canonique.

**Impact :** dans une configuration comportant cet alias indirect, un chemin déclaré de confiance peut être redirigé vers du code ou de la configuration modifiable au moment de son utilisation host. Cela contredit l'immutabilité des plugins/guard/profils promise par le spec. Le risque concerne les profils acceptés, pas seulement un outil refusé sur cette machine. Cette conclusion vient du flux de source, sans démonstration d'exploitation.

**Correction bornée :** appliquer le walker existant `trusted_path_target` à tous les chemins trusted/secrets utilisés hors sandbox, en refusant toute traversée par les deux racines writables avant canonicalisation. Épingler les chemins réellement utilisés lorsque nécessaire. Conserver la prise en charge des feuilles secrètes absentes via un contrôle du plus proche ancêtre existant. Régressions attendues : alias externe -> composant mutable -> destination externe, dans worktree et sandbox-tmp, refusé avant un marqueur fake ; alias externe stable accepté. Pas de nouveau runtime.

## Important I2 : le mode Claude encore accepté n'encadre pas ses lectures automatiques de contexte host

**Source :** `crates/agent-workers/src/claude.rs:39-46,109-129`, `crates/agent-workers/src/sandbox.rs:1047-1083`, `docs/guide/security.md:47-49`.

Claude est lancé avec `--setting-sources user`. `check_claude_settings` exige un objet settings vide et refuse les caches/state connus, mais ne refuse ni `CLAUDE.md` ni les règles utilisateur ou autres sources de mémoire implicites. Les paramètres générés ne désactivent pas ces lectures. La limitation des outils à Bash ne contrôle pas une lecture effectuée avant les outils par le CLI host.

**Ancrage exact disponible localement :** `/Users/antoinehoriot/.local/share/claude/versions/2.1.296`, source JavaScript embarquée de cette version, inspectée sans lancer le binaire ni lire un profil utilisateur :

- fonction `cn`, offset octet **188329629**, vérifie la présence de la source dans la liste des settings activés ; la source `user` est activée par l'adaptateur ;
- `qZn`, offset **195902106**, charge la mémoire User lorsque `cn("userSettings")` est vrai, appelle `t1` pour le fichier et `zae` pour les règles, et contient aussi la découverte AutoMem ;
- `t1` appelle `XNe` et traite les imports de contexte ; `XNe`, offset **195893038**, utilise `Yk` pour la lecture host ;
- `Yk`, offset **187585677**, effectue `stat` puis `readFile` sur le filesystem fourni. Aucune politique SRT de commandes ou `PROVEFAB_SANDBOX_POLICY` n'intervient dans ce parcours ;
- `j3` choisit le `CLAUDE.md` utilisateur, distinct des settings JSON vides exigés par l'adaptateur.

**Impact :** le contrat de deny_read ne couvre pas ces fichiers automatiquement intégrés au contexte dans un profil Claude 2.1.296 autrement accepté. Le refus de `~/.claude/state` sur la machine actuelle ne prouve rien pour un profil où ce fichier est absent. La correction Codex/Pi a traité ce même type de frontière, mais pas la branche Claude acceptée. Aucun succès d'exfiltration ou de course Claude n'est revendiqué ici.

**Correction bornée :** soit démontrer par source exacte les contrôles qui désactivent toutes ces lectures, les appliquer et en vérifier les préconditions, soit refuser temporairement tous les stages Claude avant les probes host, avec documentation synchronisée. Ne pas activer aveuglément `--bare` ou `--safe-mode` : leur aide embarquée mentionne également la désactivation de hooks/plugins/auth, ce qui peut supprimer le guard ou changer le mode d'authentification. Pas de suppression de fichiers utilisateur pour satisfaire le contrôle.

## Cinq risques nommés vérifiés

1. **Route alternative sans politique :** appels production de `StageRequest`/`run_gates` tracés dans `pipeline.rs:2236,2522`, `pr_review.rs:598`, `rules.rs:913` et `agents.rs:96`. Aucun autre appel production non protégé identifié.
2. **Alias mutable de validateur/runtime/politique :** dérivation `sandbox.rs:76-111`, walker `agent-workers/src/sandbox.rs:746`, écriture de politique avec O_NOFOLLOW et garde de politique examinés. Les validateurs/contextes utilisent le walker, mais I1 subsiste pour les chemins de confiance des adaptateurs.
3. **Helpers/contexte/environnement host :** allowlist d'environnement, contrôles Codex avant app-server et worker, overrides communs, absence des outils fichiers Pi/Claude examinés. I2 subsiste dans le mode Claude accepté.
4. **Fallback/lifecycle :** préparation des gates retourne un résultat échoué sur erreur ; timeout nettoie le groupe ; runtime/probe manquant ne lance pas la commande. `process.rs:245-250` conserve sa limite antérieure explicite sur les descendants déjà détachés et reparentés. Aucune nouvelle garantie de nettoyage universel n'est inventée.
5. **Promesses dépassant les preuves :** artefacts `/private/tmp/pf-core-ada80j/evidence.json`, `/private/tmp/pf-codex-egh2oA/evidence.json`, `/private/tmp/pf-pi-wdWtEV/claude-refusal.txt` inspectés. Le core/Codex montre 17 résultats attendus, HTTP0/ref absente, validateur exit1 ; patch ordinaire terminé et refus du hook documentés. La copie distingue ces éléments des limites Linux/Claude et de la revue de source.

## Vérification existante et limites

Le contrôleur a exécuté après arrêt des edits production : fmt, clippy workspace/all-targets, nextest **816/816 + 16 ignorés**, suite native **3/3**. Les résumés des journaux `nextest-final.log` et `native-final.log` ont été lus. Build release offline réussi selon le journal et le contrôleur, binaire arm64 démarre ; aucun livrable signé/universal/publié revendiqué. Site : 116 tests/build 23 pages, tests et build racine valides, relecture desktop par l'implémenteur. Mobile replié/déplié non vérifié ; dark non applicable car la configuration impose LightTheme.

Pas de répétition de ces suites pour cette revue. Les correctifs I1/I2 nécessitent leurs régressions focalisées, les checks Rust affectés et une relecture ciblée ; le changement de politique de support doit se refléter dans les guides/landing et leur pin de source.
