#![cfg(feature = "testkit")]
//! Post-merge verification (spec revision 2), against real git repos.

use std::path::Path;

use provefab::testkit::{fixture, git};

fn commit_and_push(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-qm", msg]);
    git(repo, &["push", "-q", "origin", "main"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn git_helpers_revert_exactly_and_never_reuse_residue() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git {
        program: "git".into(),
    };
    let sha = commit_and_push(&repo, "README.md", "broken\n", "change");
    assert_eq!(g.parent_count(&repo, &sha).await.unwrap(), 1);

    // Residue at the path is discarded, never reused.
    let wt = f.home.join("post-merge").join("1-verify");
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    std::fs::write(wt.join("README.md"), "residue\n").unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "broken\n"
    );
    assert!(g.clean(&wt).await.unwrap());

    // A plain directory (not a registered worktree) is discarded too.
    g.worktree_discard(&repo, &wt).await.unwrap();
    std::fs::create_dir_all(wt.join("junk")).unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert!(!wt.join("junk").exists());

    g.revert(&wt, &sha, None).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "hello\n"
    );
    let revert = g.head(&wt).await.unwrap();

    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0")
            .await
            .unwrap(),
        None
    );
    g.branch_force(&repo, "provefab/revert-1-0", &revert)
        .await
        .unwrap();
    g.push_sha(&repo, &revert, "provefab/revert-1-0")
        .await
        .unwrap();
    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0")
            .await
            .unwrap()
            .as_deref(),
        Some(revert.as_str())
    );
    g.worktree_discard(&repo, &wt).await.unwrap();
    assert!(!wt.exists());
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap();
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap(); // missing is fine
}

#[tokio::test]
async fn a_merge_commit_reverts_with_mainline_one() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git {
        program: "git".into(),
    };
    git(&repo, &["switch", "-qc", "feature"]);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["commit", "-qam", "feature"]);
    git(&repo, &["switch", "-q", "main"]);
    git(
        &repo,
        &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(g.parent_count(&repo, &merge).await.unwrap(), 2);
    let wt = f.home.join("post-merge").join("2-revert");
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    assert!(g.revert(&wt, &merge, None).await.is_err());
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    g.revert(&wt, &merge, Some(1)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(wt.join("README.md")).unwrap(),
        "hello\n"
    );
}
