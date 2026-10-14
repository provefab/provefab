# Validation native du sandbox

Validation historique initiale du 14 octobre 2026, branche `feat/native-sandbox`, base `337aafe`. Périmètre : trois tests opt-in, fixtures locales factices, gates core et adaptateurs existants. Aucun push, déploiement, installation globale ou accès à un secret réel. Seul changement de configuration autorisé : pinner `untrusted` les worktrees scratch créés par ce test, dans le profil Codex dédié, comme le fait le parcours core normal.

## Correctifs finaux I1/I2, 14 octobre 2026

Une seule vague bornée depuis `9c2c6c5`, après [la revue globale](native-sandbox-review.md). Ces résultats remplacent les affirmations de support Claude ci-dessous, conservées comme historique. Tous les stages Claude refusent maintenant avant préparation, version/auth ou exécution host, même avec profil propre et NoTools. La frontière de contexte implicite reste non vérifiée. Choisir Codex/Pi, supprimer ou remplacer les entrées `claude-code` du catalogue, sans supprimer les données utilisateur. L’exemple actif contient seulement Codex ; `init` conserve sa détection historique et nécessite cette adaptation. Login/doctor ne prouvent pas le support d’un stage.

I1 : les entrées trusted/secrets des adaptateurs utilisent le walker existant `trusted_path_target` sur leur plus proche ancêtre existant, avant canonicalisation. Chaque composant développé traversant cwd ou sandbox-tmp refuse. Les aliases externes stables et les feuilles secrètes absentes restent acceptés. Les consommateurs peuvent garder ces aliases parce qu’aucun composant de leur résolution acceptée n’appartient aux deux racines modifiables ; aucune stabilité face à un administrateur concurrent n’est revendiquée.

Rouges observés sur le code précédent : `cargo test -p agent-workers adapter_inputs -- --nocapture` refuse le test avec « mutable adapter alias accepted » ; `cargo test -p agent-workers --test workers trusted_adapter_aliases -- --nocapture` révèle une préparation Pi acceptée et son probe ; `cargo test -p agent-workers --test workers claude_all_profiles_refuse -- --nocapture` obtient une Command Claude avec profil propre. Après correction, les quatre nouvelles régressions passent dans la suite adaptateurs, **69 unitaires + 14 intégrations**. Les contrôles I1 couvrent trusted, secret présent et secret absent à plusieurs niveaux via chacune des deux racines ; le contrôle positif vérifie les règles canonicalisées. I2 couvre Full, ReadOnly, UntrustedReadOnly et NoTools, prepare/run, absence de marqueur version/auth/stage et de mutation du profil/session. Fichiers et exécutables factices locaux uniquement.

Checks finaux :

| Commande | Résultat | Journal |
|---|---|---|
| `cargo fmt -- --check` | exit 0 | sortie terminal après dernier format |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, 5,08 s, sans warning | `/private/tmp/provefab-final-fixes-clippy-final.log` |
| `cargo nextest run` | **804/804, 16 ignorés**, 53,519 s, sans LEAK/FAIL/WARN/ERROR | `/private/tmp/provefab-final-fixes-nextest-final.log` |
| `cargo test -p provefab --test native_sandbox -- --ignored --nocapture --test-threads=1` avec les prérequis ci-dessous | **3/3**, 20,89 s, exit 0 | `/private/tmp/provefab-final-fixes-native.log` |
| `cargo build --release --locked --offline -p provefab` | exit 0, 18,18 s | `/private/tmp/provefab-final-fixes-release.log` |
| `git diff --check` | exit 0 | sortie terminal |

Artefacts natifs relus : `/private/tmp/pf-core-ViIKhp/evidence.json`, `/private/tmp/pf-codex-Bwi8eu/evidence.json`, `/private/tmp/pf-pi-LZYzTC/{pi-output.txt,claude-refusal.txt}`. La suite existante conserve 17 résultats attendus core/Codex/Pi, HTTP0/ref absente, validateur exit1, patch ordinaire réellement terminé et appel protégé réellement refusé par hook. Le fichier Claude donne désormais le motif universel « implicit host context boundary is not verified » et oriente Codex/Pi. Aucun nouveau scénario offensif, probe Claude, accès à des secrets réels ni changement d’architecture runtime.

Le premier full nextest a échoué sur une prémisse de test périmée : `setup::tests::the_catalog_comes_from_the_worker_clis_on_path` comparait les constantes init à l’exemple maintenant Codex-only. Les attentes complètes init sont désormais indépendantes et littérales ; l’égalité Codex/jev avec l’exemple reste vérifiée. La logique init/routing est inchangée. Ciblé **1/1** vert. Ce premier run a aussi signalé LEAK sur le parser Codex `failed_commands_turn_failures_and_usage_limits`, dont le corps n’effectue aucun spawn. Diagnostic isolé **1/1** sans LEAK, puis full ci-dessus sans récidive. Cause du signal initial inconnue, aucune garantie générale de nettoyage déduite. Un fmt initial a signalé uniquement le format de la nouvelle assertion native ; corrigé puis revérifié.

Le total baisse de 816 à 804 : les tests des anciens helpers et modes acceptés Claude sont retirés avec ces helpers, et les régressions universelles les remplacent. Les preuves historiques restent historiques. Linux et la frontière native de course patch restent non vérifiés expérimentalement ; les limites Keychain, secret arbitraire et dépendances transitives ci-dessous restent applicables. Revue et rerun indépendants du contrôleur à suivre.

## Reproduction

Les trois tests natifs de [native_sandbox.rs](../../crates/provefab/tests/native_sandbox.rs) sont ignorés par défaut. Leur invocation explicite exige les prérequis, sans succès silencieux si un binaire, une authentification ou une preuve manque. Ils conservent leurs répertoires privés et affichent `native evidence: <chemin>` pour inspection. Nettoyer uniquement ces chemins précis après inspection.

Avant toute création de fixture ou mutation de confiance, la suite refuse toutes les variables Git héritées `GIT_*`, sauf `GIT_PAGER`. Cette exception est bornée : les commandes Git des fixtures et la découverte des cibles de confiance capturent leur sortie sans terminal et ne demandent jamais de pager. Un `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY` ou `GIT_CONFIG_*` hérité peut rediriger Git malgré `current_dir`, donc refuse même si le test pense déjà imposer une config locale. Les diagnostics donnent les noms, jamais les valeurs. Les tests focalisés ordinaires vérifient le refus avant callback/marqueur et la preuve JSON de réussite réelle des outils ; ils restent actifs sans prérequis natifs.

```bash
export PROVEFAB_NATIVE_SRT=/chemin/prive/node_modules/.bin/srt
export PROVEFAB_NATIVE_GIT=/opt/homebrew/bin/git
export PROVEFAB_NATIVE_PYTHON=/Library/Frameworks/Python.framework/Versions/3.14/bin/python3
export PROVEFAB_NATIVE_NODE="$HOME/.vite-plus/js_runtime/node/24.21.0/bin/node"
export PI_TEST_MODULE="$HOME/.vite-plus/packages/@earendil-works/pi-coding-agent/lib/node_modules/@earendil-works/pi-coding-agent/dist/index.js"
export PROVEFAB_NATIVE_CODEX=/opt/homebrew/bin/codex
export PROVEFAB_NATIVE_CODEX_HOME="$HOME/.provefab/codex"
export PROVEFAB_NATIVE_CLAUDE="$HOME/.local/bin/claude"
export PROVEFAB_NATIVE_CLAUDE_HOME="$HOME/.provefab/claude"
export PROVEFAB_TEST_CODEX_MODEL=gpt-5.5
cargo test -p provefab --test native_sandbox -- --ignored --nocapture --test-threads=1
cargo test -p provefab --test guard_cli
```

Utiliser un vrai exécutable Node. Le shim Vite+ `bin/node` se résout vers `vp` et change de comportement si lancé sous son nom canonique. Le profil Codex dédié doit être déjà connecté et avoir approuvé le hook Provefab. Le test appelle `codex_setup::pin_untrusted` pour son seul scratch neuf, puis `codex_setup::check` et `CodexWorker.run`, avec le binaire guard compilé par Cargo. Il vérifie après lancement que le scratch reste `untrusted` ; aucun login ou hook de confiance n'est changé. Le troisième test exige désormais le refus universel Claude avant version/auth/stage pour frontière de contexte implicite non vérifiée, indépendamment du profil et de l’état host. Ne pas adapter cette attente pour faire passer un lancement Claude.

Versions historiques observées : macOS 26.6.2, aarch64 ; SRT 0.0.79 ; Codex 0.162.1 ; modèle demandé gpt-5.5 ; Pi 1.0.3 ; Node 24.21.0 ; Python 3.14.0 ; Git Homebrew 2.51.0 ; Rust 1.96.0 ; nextest 0.9.132. Claude installé est **2.1.299**, différent de la version 2.1.296 inspectée alors par le code. Son refus intervient avant le probe de version à cause de `~/.claude/state`, pas après une exécution réussie de Claude.

## Preuves historiques observées

Le contrôle non sandboxé exécute les **17 opérations** et réussit chacune. Le serveur HTTP témoin reçoit deux requêtes, Python sans proxy et curl enfant. Le dépôt Git bare local reçoit réellement `refs/heads/main`. Les fixtures sont restaurées avant le run strict.

Le vrai [derive](../../crates/provefab/src/sandbox.rs) puis [run_gates](../../crates/provefab/src/gates.rs) produisent 17 résultats attendus : écriture ordinaire, enfant et temp privé réussis ; lectures directe/enfant/symlink du faux secret, HTTP Python/curl, push local avec environnement Git dépouillé et hooks retirés, écritures des validateurs interne/externe et de la politique, écriture externe, bind local, hardlinks secret/validateur et renommage du parent du validateur refusés. Témoins host : **zéro nouvelle requête HTTP, aucune ref Git**. Les trois validateurs restent byte-identiques ; leur script `exit 1` produit un `GateReport` échoué avec exit 1 après la tentative de remplacement. Runtime absent et option réseau local refusent avant création du marqueur.

Le vrai [CodexWorker.run](../../crates/agent-workers/src/codex.rs), profil Full et guard courant, exécute le même probe. Le transcript contient `command_execution`, exit 0 ; l'artefact indépendant contient les 17 résultats attendus, aucun accès au témoin HTTP et aucune ref Git. Une opération `file_change` réelle modifie `patch-target.txt` via `apply_patch`. Le rollout de cette session contient l'appel `apply_patch` sur le validateur protégé et sa sortie exacte : « Command blocked by PreToolUse hook: native sandbox policy refuses this tool path or cannot be verified. » Ce refus de hook prouve la tentative et la décision du guard. Il ne prouve pas une opération fichier native refusée par l'OS ni la résistance à une course de symlink. Le JSON exec omet l'appel refusé ; le test retrouve uniquement le rollout du thread courant par son nom et copie uniquement l'appel et sa sortie, sans lire d'autres sessions ni l'authentification.

Pi utilise la politique réellement produite par [PiWorker.prepare_command](../../crates/agent-workers/src/pi.rs), l'[extension](../../plugins/pi-provefab/extensions/provefab.ts), les fabriques installées, le vrai SRT et le guard Cargo. Son Bash exécute le même probe, 17 résultats attendus et témoins host inchangés. Les routes fichiers/builtin sont bloquées et seul `provefab_bash` est enregistré. Politique supprimée et runtime absent refusent avant le marqueur. Aucun appel modèle Pi n'est nécessaire.

Claude refuse dans `prepare_command`, avant lancement et sans fallback, avec « unsupported Claude managed/cached policy source » pour `~/.claude/state`. Aucun état n'est supprimé ou déplacé. Les [12 tests CLI guard](../../crates/provefab/tests/guard_cli.rs) passent, incluant Read/Edit/patch, parents, alias, symlink, politique absente/malformée et maintien du patch ordinaire.

Premiers artefacts indépendants : `/private/tmp/pf-core-JZablo/evidence.json`, `/private/tmp/pf-codex-kx9d5j/session/events.jsonl`, `/private/tmp/pf-codex-kx9d5j/worker-result.txt`, `/private/tmp/pf-pi-2s2ceq/pi-output.txt`, `/private/tmp/pf-pi-2s2ceq/claude-refusal.txt`. Les tests génèrent de nouveaux chemins à chaque invocation ; les résultats durables et les commandes ci-dessus restent suffisants après nettoyage des scratchs.

Run final avec pin de preflight normal : **3/3 tests natifs réussis**, 15,34 s. Artefacts : `/private/tmp/pf-core-KBLaB4/evidence.json`, `/private/tmp/pf-codex-EBJP6H/{evidence.json,worker-result.txt,session/events.jsonl}`, `/private/tmp/pf-pi-9xFvOE/{pi-output.txt,claude-refusal.txt}`. L'ancien champ `atomic_patch_os_boundary=unverified` de cet artefact historique désigne l'absence d'expérience dédiée ; le générateur actuel distingue `patch_os_boundary=source-verified Codex 0.162.1 fs-helper; no isolated native refusal experiment` et `patch_race_experiment=not performed`. Aucun nouvel appel modèle effectué uniquement pour cette clarification de libellé.

Vérifications workspace : `cargo fmt -- --check` et `cargo clippy --workspace --all-targets -- -D warnings` exit 0 ; `cargo nextest run` : **814/814 réussis, 16 ignorés**, 50,575 s, aucun signal LEAK dans le résumé final. Les 16 ignorés incluent les trois nouveaux tests natifs exécutés explicitement ci-dessus. Harness Pi installé : `PI_TEST_MODULE=... node --experimental-vm-modules --test plugins/pi-provefab/sandbox.test.mjs`, **5/5**, exit 0 ; seuls warnings Node pour API VM et stripping TypeScript expérimentales. `cargo test -p provefab --test guard_cli` : **12/12**, exit 0. Les sources/fixtures/documents édités ont été rouverts ; `git diff --check` exit 0. La revue et les tests indépendants du contrôleur restent une étape distincte.

Correctif de revue Task 3 : les contrôles Codex exigent désormais `item.completed`, `status=completed`, exit 0 pour la commande du probe courant et un `file_change` update achevé sur le chemin exact du patch. Un simple ToolStart, une mention textuelle, une opération échouée/incomplète ou un autre chemin refuse. `cargo test -p provefab --test native_sandbox` : **2/2 focalisés, 3 ignorés**, après rouges observés pour Git redirigé accepté et événement échoué accepté. Rerun natif corrigé : **3/3**, 15,58 s, artefacts `/private/tmp/pf-core-ArJ167/evidence.json`, `/private/tmp/pf-codex-RAHMal/evidence.json`, `/private/tmp/pf-pi-zFhRcD/{pi-output.txt,claude-refusal.txt}`. Format et Clippy workspace all-targets repassent. Le total workspace 814 ci-dessus décrit le commit initial et son contrôle indépendant ; les deux tests focalisés sont ajoutés ensuite, sans changement production ni répétition workspace demandée dans ce correctif.

## Risques contrôlés et limites

1. Faux succès par absence de tentative : contrôle positif complet, témoins HTTP/Git réels et assertions sur artefacts. Les textes du modèle seuls ne sont jamais une preuve.
2. Validateur remplacé : comparaison des bytes, parent rename et hardlink refusés, puis véritable gate échoué avec exit 1.
3. Fallback sans politique/runtime : marqueur absent après refus core/Pi ; Claude refuse sa source non vérifiable avant lancement.
4. Outils fichiers host Pi : fabriques absentes, routes stale bloquées, Bash soumis à SRT et au guard actuel. La preuve patch Codex distingue événements natifs réussis et refus du hook.
5. Chemin du socket SRT : le premier run avec le temp macOS long échoue avant la fixture, `listen EINVAL` sur `.../session/sandbox-tmp/srt-mux-79514-0.sock`. Le [layout core](../../crates/provefab/src/paths.rs) est `home/sessions/id/stage-attempt`; SRT 0.0.79 `dist/sandbox/mux-proxy.js:22` crée `join(tmpdir(), srt-mux-PID-SEQ.sock)`, avec commentaire « macOS sun_path is 104 bytes ». Le SDK macOS `sys/un.h` déclare `sun_path[104]`. Pour le home courant, task 7/attempt 2/pid 12345, les vrais stages occupent 87 à 92 bytes, `implement-r0` 89 et `post-merge-base` 92. Un home personnalisé long, des identifiants/compteurs plus longs ou un PID plus long peuvent dépasser cette limite et faire échouer les gates. Choisir un home court ; aucun fallback non sandboxé. Les fixtures choisissent un nouveau répertoire privé sous `/tmp` pour rester dans ce budget, décision explicite, sans nouveau service ou tmp production.

Linux reste **non vérifié** : le précédent conteneur disposable Node 22/SRT 0.0.79 n'autorisait pas les namespaces utilisateur bubblewrap. Aucun privilège Docker ajouté. Windows n'est pas exercé par cette suite Unix. L'option réseau local refuse toutes les plateformes : aucune garantie loopback-only n'est revendiquée. Les tests ne prouvent ni clôture transitive des dépendances de validation ni isolation de tous les secrets host, APIs Keychain par recherche par défaut, ou modification concurrente administrateur/compte/politique. `protected_paths` et `deny_read` restent des déclarations explicites.

Le choix de conserver le patch Codex s'appuie aussi sur la revue des sources exactes [handler](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/core/src/tools/handlers/apply_patch.rs) et [runtime](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/core/src/tools/runtimes/apply_patch.rs). C'est une justification par source, distincte de la preuve empirique ci-dessus ; aucune preuve native de course atomique n'est revendiquée.

La revue bornée suit handler 319-325/519-531, runtime 163-184, apply-patch/lib 628-635/730-770, exec-server/environment 729-744, local_file_system 78-108, file-system/lib 378-390, sandboxed_file_system 132-179 puis fs_sandbox 143-199 et spawn 375. La source applique les prédicats de lecture/écriture restreints et lance l'opération dans un subprocess fs-helper sandboxé ; `SandboxType::None` refuse. Cela ne constitue ni preuve empirique d'une course concurrente ni garantie de transaction multi-fichiers.

Le premier Worker lancé sans le pin de preflight avait auto-trusté son scratch, comportement déjà décrit dans [codex_setup.rs](../../crates/provefab/src/codex_setup.rs). Le rerun l'a détecté avec `ProjectTrusted`, sans lancement. Exception autorisée et bornée : l'API existante a repinné uniquement `/private/tmp/pf-codex-kx9d5j/work` à `untrusted`, puis le test a adopté le parcours normal pour chaque nouveau scratch. Aucune autre entrée/profil/authentification n'a été modifiée.
