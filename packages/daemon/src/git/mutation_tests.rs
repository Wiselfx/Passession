use super::{command, facts, mutations};
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;

async fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    command::text(cwd, args, &BTreeMap::new(), CancellationToken::new()).await
}
async fn repository() -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), &["init", "--initial-branch=main"]).await?;
    git(dir.path(), &["config", "user.name", "Git fixture"]).await?;
    git(
        dir.path(),
        &["config", "user.email", "fixture@example.invalid"],
    )
    .await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"before\n").await?;
    git(dir.path(), &["add", "tracked.txt"]).await?;
    git(dir.path(), &["commit", "-m", "initial"]).await?;
    Ok(dir)
}

#[tokio::test]
async fn literal_discard_does_not_interpret_pathspec_magic() -> anyhow::Result<()> {
    let dir = repository().await?;
    // Read as a glob, `[ab].txt` would also discard the ordinary tracked `a.txt`.
    tokio::fs::write(dir.path().join("a.txt"), b"before\n").await?;
    git(dir.path(), &["add", "a.txt"]).await?;
    git(dir.path(), &["commit", "-m", "ordinary glob match"]).await?;
    // Windows file names cannot contain `:` or `*`; elsewhere also cover magic.
    let specials: Vec<&str> = if cfg!(windows) {
        vec!["[ab].txt"]
    } else {
        vec!["[ab].txt", ":(glob)*"]
    };
    for special in &specials {
        tokio::fs::write(dir.path().join(special), b"literal\n").await?;
    }
    tokio::fs::write(dir.path().join("a.txt"), b"keep a\n").await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"keep this change\n").await?;
    mutations::discard(
        dir.path(),
        &specials.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await?;
    for special in &specials {
        assert!(
            !dir.path().join(special).exists(),
            "{special} was not discarded"
        );
    }
    assert_eq!(
        tokio::fs::read(dir.path().join("a.txt")).await?,
        b"keep a\n"
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"keep this change\n"
    );
    Ok(())
}

#[tokio::test]
async fn push_uses_configured_remote_and_exact_head_ref() -> anyhow::Result<()> {
    let dir = repository().await?;
    let bare = tempfile::tempdir()?;
    git(bare.path(), &["init", "--bare"]).await?;
    git(
        dir.path(),
        &["remote", "add", "review", bare.path().to_str().unwrap()],
    )
    .await?;
    git(dir.path(), &["config", "branch.main.pushRemote", "review"]).await?;
    git(
        dir.path(),
        &[
            "config",
            "remote.review.push",
            "HEAD:refs/heads/review-target",
        ],
    )
    .await?;
    mutations::push(dir.path(), &BTreeMap::new(), CancellationToken::new()).await?;
    let expected = git(dir.path(), &["rev-parse", "HEAD"]).await?;
    assert_eq!(
        git(bare.path(), &["rev-parse", "refs/heads/review-target"]).await?,
        expected
    );
    assert!(
        !command::exists(
            bare.path(),
            "refs/heads/main",
            &BTreeMap::new(),
            CancellationToken::new()
        )
        .await?
    );
    Ok(())
}

#[tokio::test]
async fn merge_base_in_other_worktree_keeps_source_branch() -> anyhow::Result<()> {
    let dir = repository().await?;
    let worktrees = tempfile::tempdir()?;
    let feature = worktrees.path().join("feature");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            "feature",
            feature.to_str().unwrap(),
            "main",
        ],
    )
    .await?;
    tokio::fs::write(feature.join("tracked.txt"), b"feature\n").await?;
    git(&feature, &["commit", "-am", "feature"]).await?;
    let mutated = mutations::merge_to_base(
        &feature,
        Some("main"),
        false,
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await?;
    assert_eq!(mutated, dir.path());
    assert_eq!(
        facts::current_branch(&feature, &BTreeMap::new(), CancellationToken::new())
            .await?
            .as_deref(),
        Some("feature")
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"feature\n"
    );
    Ok(())
}

#[tokio::test]
async fn mutations_outside_a_repository_report_not_git_repo() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let error = mutations::pull(dir.path(), &BTreeMap::new(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<command::NotGitRepo>().is_some());
    assert_eq!(
        error.to_string(),
        format!("Not a git repository: {}", dir.path().display())
    );
    Ok(())
}

#[tokio::test]
async fn merge_from_base_conflict_is_classified_and_aborted() -> anyhow::Result<()> {
    let dir = repository().await?;
    git(dir.path(), &["checkout", "-b", "feature"]).await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"feature\n").await?;
    git(dir.path(), &["commit", "-am", "feature"]).await?;
    git(dir.path(), &["checkout", "main"]).await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"main\n").await?;
    git(dir.path(), &["commit", "-am", "main"]).await?;
    git(dir.path(), &["checkout", "feature"]).await?;
    let error = mutations::merge_from_base(
        dir.path(),
        Some("main"),
        true,
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<mutations::MergeConflict>().is_some());
    assert_eq!(
        error.to_string(),
        "Merge conflict while merging main into feature. Please merge manually."
    );
    assert!(!dir.path().join(".git/MERGE_HEAD").exists());
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"feature\n"
    );
    Ok(())
}

fn selected_commit_cleaned_up(root: &Path) -> anyhow::Result<()> {
    assert!(!root.join(".git/index.lock").exists());
    for entry in std::fs::read_dir(root.join(".git"))? {
        assert!(!entry?.file_name().to_string_lossy().starts_with("paseo-selected-"));
    }
    Ok(())
}

#[tokio::test]
async fn selected_commit_preserves_unselected_index_and_worktree() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    for name in ["other.txt", "deleted.txt", "staged-delete.txt", "old name.txt", "a.txt"] {
        tokio::fs::write(root.join(name), format!("original {name}\n")).await?;
    }
    git(root, &["add", "-A"]).await?;
    git(root, &["commit", "-m", "fixture files"]).await?;
    for name in ["tracked.txt", "other.txt"] {
        tokio::fs::write(root.join(name), b"staged intermediate\n").await?;
        git(root, &["add", "--", name]).await?;
        tokio::fs::write(root.join(name), b"complete working contents\n").await?;
    }
    tokio::fs::remove_file(root.join("deleted.txt")).await?;
    git(root, &["rm", "--", "staged-delete.txt"]).await?;
    git(root, &["mv", "--", "old name.txt", "new name.txt"]).await?;
    tokio::fs::write(root.join("a.txt"), b"unselected glob match\n").await?;
    tokio::fs::write(root.join("unselected new.txt"), b"leave untracked\n").await?;
    let mut selected = vec![
        "tracked.txt".to_owned(), "deleted.txt".into(), "staged-delete.txt".into(),
        "new name.txt".into(), "[ab].txt".into(), "new file.txt".into(),
    ];
    if !cfg!(windows) {
        selected.push(":(glob)*".into());
    }
    for name in &selected[4..] {
        tokio::fs::write(root.join(name), b"literal new content\n").await?;
    }
    let other_index = git(root, &["ls-files", "--stage", "--", "other.txt"]).await?;
    let other_unstaged = git(root, &["diff", "--", "other.txt"]).await?;
    let sha = mutations::commit_selected(
        root, &selected, "Selected files\n\nFull contents, not staged hunks.",
        &BTreeMap::new(), CancellationToken::new(),
    ).await?;
    assert_eq!(sha, git(root, &["rev-parse", "HEAD"]).await?);
    assert_eq!(git(root, &["show", "HEAD:tracked.txt"]).await?, "complete working contents");
    assert_eq!(git(root, &["show", "HEAD:other.txt"]).await?, "original other.txt");
    assert_eq!(git(root, &["show", "HEAD:a.txt"]).await?, "original a.txt");
    assert_eq!(git(root, &["show", "HEAD:new name.txt"]).await?, "original old name.txt");
    for name in &selected[4..] {
        assert_eq!(git(root, &["show", &format!("HEAD:{name}")]).await?, "literal new content");
    }
    let tree = git(root, &["ls-tree", "--name-only", "HEAD"]).await?;
    assert!(!tree.lines().any(|name| ["deleted.txt", "staged-delete.txt", "old name.txt", "unselected new.txt"].contains(&name)));
    assert_eq!(other_index, git(root, &["ls-files", "--stage", "--", "other.txt"]).await?);
    assert_eq!(other_unstaged, git(root, &["diff", "--", "other.txt"]).await?);
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]).await?, "other.txt");
    assert_eq!(tokio::fs::read(root.join("a.txt")).await?, b"unselected glob match\n");
    assert_eq!(tokio::fs::read(root.join("other.txt")).await?, b"complete working contents\n");
    assert_eq!(tokio::fs::read(root.join("unselected new.txt")).await?, b"leave untracked\n");
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_supports_unborn_head_without_committing_unselected_staging() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init", "--initial-branch=main"]).await?;
    git(root, &["config", "user.name", "Git fixture"]).await?;
    git(root, &["config", "user.email", "fixture@example.invalid"]).await?;
    tokio::fs::write(root.join("selected.txt"), b"first commit\n").await?;
    tokio::fs::write(root.join("other.txt"), b"other staged\n").await?;
    git(root, &["add", "--", "other.txt"]).await?;
    tokio::fs::write(root.join("other.txt"), b"other unstaged\n").await?;
    let original = git(root, &["ls-files", "--stage", "--", "other.txt"]).await?;
    mutations::commit_selected(
        root, &["selected.txt".into()], "Initial selection",
        &BTreeMap::new(), CancellationToken::new(),
    ).await?;
    assert_eq!(git(root, &["ls-tree", "--name-only", "HEAD"]).await?, "selected.txt");
    assert_eq!(original, git(root, &["ls-files", "--stage", "--", "other.txt"]).await?);
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]).await?, "other.txt");
    assert_eq!(tokio::fs::read(root.join("other.txt")).await?, b"other unstaged\n");
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_rejects_invalid_empty_and_unchanged_selections() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    tokio::fs::create_dir(root.join("folder")).await?;
    tokio::fs::write(root.join("folder/new.txt"), b"new\n").await?;
    let before = tokio::fs::read(root.join(".git/index")).await?;
    let head = git(root, &["rev-parse", "HEAD"]).await?;
    for (paths, message) in [
        (vec![], "message"),
        (vec!["folder/new.txt"], " \n\t"),
        (vec!["folder/new.txt"], "bad\0message"),
        (vec![""], "message"),
        (vec!["../outside"], "message"),
        (vec!["/absolute"], "message"),
        (vec!["."], "message"),
        (vec!["folder"], "message"),
        (vec!["folder/"], "message"),
        (vec!["folder/../tracked.txt"], "message"),
        (vec!["tracked.txt"], "message"),
        (vec!["missing"], "message"),
        (vec!["folder/*"], "message"),
        (vec!["folder/new.txt", "missing"], "message"),
    ] {
        let paths = paths.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(mutations::commit_selected(
            root, &paths, message, &BTreeMap::new(), CancellationToken::new(),
        ).await.is_err(), "{paths:?}");
        assert_eq!(tokio::fs::read(root.join(".git/index")).await?, before);
        selected_commit_cleaned_up(root)?;
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(mutations::commit_selected(
        root, &["folder/new.txt".into()], "cancelled", &BTreeMap::new(), cancelled,
    ).await.is_err());
    assert_eq!(head, git(root, &["rev-parse", "HEAD"]).await?);
    assert_eq!(tokio::fs::read(root.join("folder/new.txt")).await?, b"new\n");
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_rejects_conflicts_without_touching_index() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    git(root, &["checkout", "-b", "feature"]).await?;
    tokio::fs::write(root.join("tracked.txt"), b"feature\n").await?;
    git(root, &["commit", "-am", "feature"]).await?;
    git(root, &["checkout", "main"]).await?;
    tokio::fs::write(root.join("tracked.txt"), b"main\n").await?;
    git(root, &["commit", "-am", "main"]).await?;
    assert!(git(root, &["merge", "feature"]).await.is_err());
    tokio::fs::write(root.join("unrelated.txt"), b"must not commit\n").await?;
    let before = tokio::fs::read(root.join(".git/index")).await?;
    assert!(mutations::commit_selected(
        root, &["unrelated.txt".into()], "not a merge",
        &BTreeMap::new(), CancellationToken::new(),
    ).await.is_err());
    // Also reject unmerged index entries without relying on MERGE_HEAD.
    tokio::fs::remove_file(root.join(".git/MERGE_HEAD")).await?;
    let error = mutations::commit_selected(
        root, &["unrelated.txt".into()], "not a conflict resolution",
        &BTreeMap::new(), CancellationToken::new(),
    ).await.unwrap_err();
    assert!(error.to_string().contains("conflicts"));
    assert_eq!(tokio::fs::read(root.join(".git/index")).await?, before);
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn selected_commit_failed_hook_restores_index_and_removes_temporary_state() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = repository().await?;
    let root = dir.path();
    tokio::fs::write(root.join("tracked.txt"), b"staged\n").await?;
    git(root, &["add", "tracked.txt"]).await?;
    tokio::fs::write(root.join("tracked.txt"), b"working\n").await?;
    let hook = root.join(".git/hooks/pre-commit");
    tokio::fs::write(&hook, b"#!/bin/sh\nexit 1\n").await?;
    tokio::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).await?;
    let index = tokio::fs::read(root.join(".git/index")).await?;
    let head = git(root, &["rev-parse", "HEAD"]).await?;
    assert!(mutations::commit_selected(
        root, &["tracked.txt".into()], "rejected",
        &BTreeMap::new(), CancellationToken::new(),
    ).await.is_err());
    assert_eq!(tokio::fs::read(root.join(".git/index")).await?, index);
    assert_eq!(head, git(root, &["rev-parse", "HEAD"]).await?);
    assert_eq!(tokio::fs::read(root.join("tracked.txt")).await?, b"working\n");
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn selected_commit_settles_index_after_cancellation_during_commit() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = repository().await?;
    let root = dir.path().to_owned();
    tokio::fs::write(root.join("tracked.txt"), b"committed\n").await?;
    let hook = root.join(".git/hooks/pre-commit");
    tokio::fs::write(
        &hook,
        b"#!/bin/sh\ntouch .git/hook-started\nwhile ! test -f .git/hook-release; do sleep 0.05; done\n",
    ).await?;
    tokio::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).await?;
    let cancel = CancellationToken::new();
    let task_root = root.clone();
    let task_cancel = cancel.clone();
    let commit = tokio::spawn(async move {
        mutations::commit_selected(
            &task_root, &["tracked.txt".into()], "finish admitted commit",
            &BTreeMap::new(), task_cancel,
        ).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !root.join(".git/hook-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await?;
    cancel.cancel();
    tokio::fs::write(root.join(".git/hook-release"), b"").await?;
    let sha = commit.await??;
    assert_eq!(sha, git(&root, &["rev-parse", "HEAD"]).await?);
    assert_eq!(git(&root, &["show", "HEAD:tracked.txt"]).await?, "committed");
    assert!(git(&root, &["diff", "--cached", "--name-only"]).await?.is_empty());
    selected_commit_cleaned_up(&root)?;
    Ok(())
}

#[test]
fn selected_commit_requires_workspace_write_for_request_and_response() {
    use crate::transport::permissions;
    assert_eq!(
        permissions::inbound("checkout.commit_selected.request"),
        Some(["workspace.write"].as_slice()),
    );
    assert_eq!(
        permissions::outbound("checkout.commit_selected.response"),
        Some(["workspace.write"].as_slice()),
    );
}

#[tokio::test]
async fn selected_commit_unborn_without_existing_index() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init", "--initial-branch=main"]).await?;
    git(root, &["config", "user.name", "Git fixture"]).await?;
    git(root, &["config", "user.email", "fixture@example.invalid"]).await?;
    tokio::fs::write(root.join("new.txt"), b"first\n").await?;
    assert!(!root.join(".git/index").exists());
    let sha = mutations::commit_selected(
        root, &["new.txt".into()], "first commit", &BTreeMap::new(), CancellationToken::new(),
    ).await?;
    assert_eq!(sha, git(root, &["rev-parse", "HEAD"]).await?);
    assert_eq!(git(root, &["show", "HEAD:new.txt"]).await?, "first");
    assert!(git(root, &["status", "--porcelain"]).await?.is_empty());
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_empty_tree_change_keeps_staged_version() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    tokio::fs::write(root.join("tracked.txt"), b"staged but not selected contents\n").await?;
    git(root, &["add", "tracked.txt"]).await?;
    tokio::fs::write(root.join("tracked.txt"), b"before\n").await?;
    let index = tokio::fs::read(root.join(".git/index")).await?;
    let head = git(root, &["rev-parse", "HEAD"]).await?;
    assert!(mutations::commit_selected(
        root, &["tracked.txt".into()], "no working changes",
        &BTreeMap::new(), CancellationToken::new(),
    ).await.is_err());
    assert_eq!(index, tokio::fs::read(root.join(".git/index")).await?);
    assert_eq!(head, git(root, &["rev-parse", "HEAD"]).await?);
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_rename_can_select_source() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    git(root, &["mv", "tracked.txt", "renamed.txt"]).await?;
    mutations::commit_selected(
        root, &["tracked.txt".into()], "rename",
        &BTreeMap::new(), CancellationToken::new(),
    ).await?;
    assert_eq!(git(root, &["ls-tree", "--name-only", "HEAD"]).await?, "renamed.txt");
    assert!(git(root, &["status", "--porcelain"]).await?.is_empty());
    selected_commit_cleaned_up(root)?;
    Ok(())
}

#[tokio::test]
async fn selected_commit_does_not_remove_another_writers_lock() -> anyhow::Result<()> {
    let dir = repository().await?;
    let root = dir.path();
    tokio::fs::write(root.join("tracked.txt"), b"changed\n").await?;
    tokio::fs::write(root.join(".git/index.lock"), b"owned elsewhere\n").await?;
    let index = tokio::fs::read(root.join(".git/index")).await?;
    assert!(mutations::commit_selected(
        root, &["tracked.txt".into()], "must wait",
        &BTreeMap::new(), CancellationToken::new(),
    ).await.is_err());
    assert_eq!(index, tokio::fs::read(root.join(".git/index")).await?);
    assert_eq!(tokio::fs::read(root.join(".git/index.lock")).await?, b"owned elsewhere\n");
    Ok(())
}
