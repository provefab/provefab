//! Risk-aware policy: classify a change by its changed paths into risk
//! categories (spec 2026-09-30-risk-policy §3-§6). Pure; no I/O.

use std::collections::BTreeMap;

use crate::config::ModelEntry;
use crate::task::Tier;

/// Reserved category: the changed paths could not be computed (fail closed).
pub const UNKNOWN: &str = "unknown";

/// `[repos.risk]`.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    #[serde(default)]
    pub disable: Vec<String>,
    #[serde(default)]
    pub categories: BTreeMap<String, CategoryConfig>,
}

/// `[repos.risk.categories.<name>]`.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryConfig {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub reviewer_tier: Option<String>,
}

/// A resolved category.
#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    pub name: String,
    pub paths: Vec<String>,
    pub checks: Vec<String>,
    /// Reviewer tier is frontier (default) rather than standard.
    pub frontier: bool,
}

/// Resolved categories: built-ins in table order, then repository ones by name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Policy {
    pub categories: Vec<Category>,
}

/// A category found in a change, with the paths that matched.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Detected {
    pub name: String,
    pub paths: Vec<String>,
}

/// Spec §5: `/`-separated, `**` spans zero or more segments, `*` any run of
/// characters inside one segment, everything else literal and case-sensitive,
/// anchored at the repository root.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs(&p, &s)
}

fn segs(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => (0..=s.len()).any(|i| segs(&p[1..], &s[i..])),
        Some(seg) => {
            !s.is_empty() && seg_match(seg.as_bytes(), s[0].as_bytes()) && segs(&p[1..], &s[1..])
        }
    }
}

// Byte-wise: `*` and `/` are ASCII, so UTF-8 sequences are matched literally.
fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => (0..=s.len()).any(|i| seg_match(&p[1..], &s[i..])),
        Some(c) => s.first() == Some(c) && seg_match(&p[1..], &s[1..]),
    }
}

/// The five built-in categories (spec §3).
pub fn builtins() -> Vec<Category> {
    let cat = |name: &str, paths: &[&str]| Category {
        name: name.into(),
        paths: paths.iter().map(|s| s.to_string()).collect(),
        checks: vec![],
        frontier: true,
    };
    vec![
        cat("ci", &[".github/**", ".gitlab-ci.yml", ".circleci/**"]),
        cat(
            "dependencies",
            &[
                "**/Cargo.toml",
                "**/Cargo.lock",
                "**/package.json",
                "**/*lock*.json",
                "**/pnpm-lock.yaml",
                "**/yarn.lock",
                "**/go.mod",
                "**/go.sum",
                "**/requirements*.txt",
                "**/pyproject.toml",
                "**/Gemfile*",
            ],
        ),
        cat("migrations", &["**/migrations/**", "**/*.sql"]),
        cat(
            "infrastructure",
            &[
                "**/Dockerfile*",
                "**/*.tf",
                "k8s/**",
                "helm/**",
                "**/docker-compose*.yml",
            ],
        ),
        cat(
            "secrets-config",
            &["**/.env*", "**/*.pem", "**/*.key", "**/*secret*"],
        ),
    ]
}

// `none` is Pro's calibration bucket for "no risk detected"; a category of
// that name would merge into it, so it is reserved like `unknown`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != UNKNOWN
        && name != "none"
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Built-ins, minus `disable`, extended by `[repos.risk.categories]` (spec §4).
pub fn resolve(cfg: Option<&RiskConfig>) -> Result<Policy, String> {
    let mut cats = builtins();
    let Some(cfg) = cfg else {
        return Ok(Policy { categories: cats });
    };
    let all = builtins();
    for name in &cfg.disable {
        if !all.iter().any(|c| &c.name == name) {
            return Err(format!("unknown category in disable: {name}"));
        }
        cats.retain(|c| &c.name != name);
    }
    for (name, cc) in &cfg.categories {
        if !valid_name(name) {
            return Err(format!("invalid category name: {name}"));
        }
        if cc.paths.iter().any(|p| p.trim().is_empty()) {
            return Err(format!("{name}: empty path"));
        }
        // Changed paths are relative and normalised, so these can never match.
        if let Some(p) = cc.paths.iter().find(|p| {
            p.starts_with('/') || p.starts_with("./") || p.ends_with('/') || p.contains("//")
        }) {
            return Err(format!(
                "{name}: path \"{p}\" must be relative, without \"./\", a trailing \"/\" or empty segments"
            ));
        }
        if cc.checks.iter().any(|c| c.trim().is_empty()) {
            return Err(format!("{name}: empty check"));
        }
        let frontier = match cc.reviewer_tier.as_deref() {
            None => None,
            Some("frontier") => Some(true),
            Some("standard") => Some(false),
            Some(_) => {
                return Err(format!(
                    "{name}: reviewer_tier must be standard or frontier"
                ));
            }
        };
        if let Some(existing) = cats.iter_mut().find(|c| &c.name == name) {
            existing.paths.extend(cc.paths.iter().cloned());
            existing.checks = cc.checks.clone();
            if let Some(f) = frontier {
                existing.frontier = f;
            }
        } else if all.iter().any(|c| &c.name == name) {
            return Err(format!("{name} is disabled"));
        } else {
            if cc.paths.is_empty() {
                return Err(format!("{name}: no paths"));
            }
            cats.push(Category {
                name: name.clone(),
                paths: cc.paths.clone(),
                checks: cc.checks.clone(),
                frontier: frontier.unwrap_or(true),
            });
        }
    }
    Ok(Policy { categories: cats })
}

/// Per category in policy order, the input paths (input order, deduped) that
/// match any of its patterns; categories with no match are omitted.
pub fn classify(policy: &Policy, paths: &[String]) -> Vec<Detected> {
    policy
        .categories
        .iter()
        .filter_map(|c| {
            let mut hit: Vec<String> = Vec::new();
            for p in paths {
                if !hit.contains(p) && c.paths.iter().any(|pat| glob_match(pat, p)) {
                    hit.push(p.clone());
                }
            }
            (!hit.is_empty()).then(|| Detected {
                name: c.name.clone(),
                paths: hit,
            })
        })
        .collect()
}

/// Fail-closed result when the changed paths cannot be computed.
pub fn unknown() -> Vec<Detected> {
    vec![Detected {
        name: UNKNOWN.into(),
        paths: vec![],
    }]
}

impl Policy {
    /// Union of the detected categories' checks, category order, deduped.
    pub fn checks(&self, detected: &[Detected]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.categories {
            if detected.iter().any(|d| d.name == c.name) {
                for check in &c.checks {
                    if !out.contains(check) {
                        out.push(check.clone());
                    }
                }
            }
        }
        out
    }

    /// Any detected category is frontier, or the result is `unknown`.
    pub fn needs_frontier(&self, detected: &[Detected]) -> bool {
        detected.iter().any(|d| {
            d.name == UNKNOWN
                || self
                    .categories
                    .iter()
                    .any(|c| c.name == d.name && c.frontier)
        })
    }
}

/// The review tier the risk policy asks for (spec §7, amended 2026-09-30):
/// frontier when a detected category needs it and the catalog has a
/// frontier model from a provider other than the implementer's; `None`
/// otherwise, so the usual tier and cross-provider choice apply. The PR's
/// Risk section reads the same answer, so it states what ran.
pub fn risky_review_tier(
    catalog: &[ModelEntry],
    implementer: Option<&str>,
    needs_frontier: bool,
) -> Option<Tier> {
    (needs_frontier
        && catalog
            .iter()
            .any(|m| m.tier == Tier::Frontier && Some(m.provider_key().as_str()) != implementer))
    .then_some(Tier::Frontier)
}

/// Some frontier model differs in provider from some standard model: a
/// risky change implemented on standard can get a frontier reviewer from
/// another provider (`provefab doctor`).
pub fn cross_provider_frontier(catalog: &[ModelEntry]) -> bool {
    let of = |t: Tier| catalog.iter().filter(move |m| m.tier == t);
    of(Tier::Frontier).any(|f| of(Tier::Standard).any(|s| s.provider_key() != f.provider_key()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_like_the_docs_say() {
        for (p, s) in [
            ("**/migrations/**", "db/migrations/0005.sql"),
            ("**/migrations/**", "migrations/0005.sql"),
            ("**/*.sql", "schema.sql"),
            ("**/.env*", "web/.env.local"),
            ("**/*lock*.json", "web/package-lock.json"),
            ("src/auth/**", "src/auth/mod.rs"),
            (".github/**", ".github/workflows/ci.yml"),
            ("**/*.sql", "docs/réunion notes.sql"),
        ] {
            assert!(glob_match(p, s), "{p} should match {s}");
        }
        for (p, s) in [
            ("src/auth/**", "src/authz.rs"),
            (".gitlab-ci.yml", "ci/.gitlab-ci.yml"),
            ("**/*.sql", "schema.sqlx"),
            ("k8s/**", "deploy/k8s/app.yaml"),
        ] {
            assert!(!glob_match(p, s), "{p} should not match {s}");
        }
    }

    #[test]
    fn resolution_extends_disables_and_validates() {
        let cfg: RiskConfig = toml::from_str(
            r#"
            disable = ["dependencies"]
            [categories.auth]
            paths = ["src/auth/**"]
            checks = ["cargo test auth"]
            [categories.migrations]
            paths = ["db/schema/**"]
            checks = ["./check.sh"]
            reviewer_tier = "standard"
        "#,
        )
        .unwrap();
        let p = resolve(Some(&cfg)).unwrap();
        let names: Vec<_> = p.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ci",
                "migrations",
                "infrastructure",
                "secrets-config",
                "auth"
            ]
        );
        let m = &p.categories[1];
        assert!(
            m.paths.contains(&"db/schema/**".to_string())
                && m.paths.contains(&"**/*.sql".to_string())
        );
        assert_eq!(
            (m.checks.clone(), m.frontier),
            (vec!["./check.sh".to_string()], false)
        );
        for bad in [
            r#"disable = ["nope"]"#,
            "[categories.unknown]\npaths = [\"x\"]",
            "[categories.Auth]\npaths = [\"x\"]",
            "[categories.auth]\npaths = []",
            "[categories.auth]\npaths = [\"\"]",
            "[categories.auth]\npaths = [\"x\"]\nchecks = [\" \"]",
            "[categories.auth]\npaths = [\"x\"]\nreviewer_tier = \"fast\"",
            "disable = [\"ci\"]\n[categories.ci]\npaths = [\"x\"]",
        ] {
            let c: RiskConfig = toml::from_str(bad).unwrap();
            assert!(resolve(Some(&c)).is_err(), "{bad}");
        }
        assert!(toml::from_str::<RiskConfig>("typo = 1").is_err());
        assert_eq!(resolve(None).unwrap().categories, builtins());
    }

    #[test]
    fn classification_lists_the_matching_paths_per_category() {
        let p = resolve(None).unwrap();
        let paths: Vec<String> = [
            "src/lib.rs",
            "migrations/0005.sql",
            "Cargo.toml",
            ".github/workflows/ci.yml",
            "migrations/0005.sql",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let d = classify(&p, &paths);
        assert_eq!(
            d.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["ci", "dependencies", "migrations"]
        );
        assert_eq!(d[2].paths, ["migrations/0005.sql"]);
        assert!(classify(&p, &["src/lib.rs".to_string()]).is_empty());
        assert!(p.needs_frontier(&d));
        assert!(p.needs_frontier(&unknown()));
        assert!(!p.needs_frontier(&[]));
    }

    #[test]
    fn none_is_reserved_like_unknown() {
        let c: RiskConfig = toml::from_str("[categories.none]\npaths = [\"x\"]").unwrap();
        assert_eq!(
            resolve(Some(&c)).unwrap_err(),
            "invalid category name: none"
        );
    }

    #[test]
    fn patterns_that_can_never_match_are_rejected() {
        for bad in ["/src/auth/**", "./src/auth/**", "src/auth/", "src//auth/**"] {
            let c = RiskConfig {
                categories: BTreeMap::from([(
                    "auth".to_string(),
                    CategoryConfig {
                        paths: vec![bad.into()],
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            };
            assert_eq!(
                resolve(Some(&c)).unwrap_err(),
                format!(
                    "auth: path \"{bad}\" must be relative, without \"./\", a trailing \"/\" or empty segments"
                ),
                "{bad}"
            );
        }
        // Extending a built-in is validated the same way.
        let c: RiskConfig = toml::from_str("[categories.ci]\npaths = [\"ci/\"]").unwrap();
        assert!(resolve(Some(&c)).is_err());
    }

    fn model(id: &str, worker: &str, tier: &str) -> ModelEntry {
        toml::from_str(&format!(
            "id = \"{id}\"\nworker = \"{worker}\"\nmodel = \"m\"\ntier = \"{tier}\""
        ))
        .unwrap()
    }

    #[test]
    fn a_risky_review_is_frontier_only_from_another_provider() {
        let std_claude = model("s", "claude-code", "standard");
        let std_codex = model("c", "codex", "standard");
        let top_claude = model("t", "claude-code", "frontier");
        let top_codex = model("x", "codex", "frontier");
        let same = [std_claude.clone(), std_codex.clone(), top_claude.clone()];
        let other = [std_claude.clone(), std_codex.clone(), top_codex.clone()];
        let none = [std_claude.clone(), std_codex.clone()];
        let claude = Some("claude-code");
        assert_eq!(risky_review_tier(&same, claude, true), None);
        assert_eq!(
            risky_review_tier(&other, claude, true),
            Some(Tier::Frontier)
        );
        assert_eq!(risky_review_tier(&none, claude, true), None);
        assert_eq!(risky_review_tier(&other, claude, false), None);
        // No implement run recorded: any frontier model differs from it.
        assert_eq!(risky_review_tier(&same, None, true), Some(Tier::Frontier));
        assert!(!cross_provider_frontier(&same[..2]));
        assert!(!cross_provider_frontier(&[
            std_claude.clone(),
            top_claude.clone()
        ]));
        assert!(cross_provider_frontier(&same));
        assert!(cross_provider_frontier(&other));
    }

    #[test]
    fn checks_are_the_deduped_union_in_category_order() {
        let cfg: RiskConfig = toml::from_str("[categories.migrations]\nchecks = [\"a\", \"b\"]\n[categories.auth]\npaths = [\"src/auth/**\"]\nchecks = [\"b\", \"c\"]").unwrap();
        let p = resolve(Some(&cfg)).unwrap();
        let d = classify(
            &p,
            &["migrations/1.sql".to_string(), "src/auth/x.rs".to_string()],
        );
        assert_eq!(p.checks(&d), ["a", "b", "c"]);
    }
}
