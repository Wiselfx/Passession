//! Read-only checkout identification for existing Git repositories and legacy placement records.
use crate::git::command;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::{Component, Path, PathBuf}};
use tokio_util::sync::CancellationToken;

fn lexical(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => { result.pop(); }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

pub async fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut ancestor = lexical(path)?;
    let mut suffix = Vec::new();
    loop {
        match tokio::fs::canonicalize(&ancestor).await {
            Ok(mut root) => {
                for component in suffix.iter().rev() { root.push(component); }
                return Ok(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(ancestor.file_name().ok_or(error)?.to_owned());
                if !ancestor.pop() { bail!("Unable to resolve path {}", path.display()); }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn base_root(home: &Path, config: &Value) -> Result<PathBuf> {
    let Some(root) = config["worktreesRoot"].as_str().filter(|root| !root.is_empty()) else {
        return lexical(&home.join("worktrees"));
    };
    let expanded = if root == "~" || root.starts_with("~/") {
        dirs::home_dir().ok_or_else(|| anyhow!("User home directory is unavailable"))?
            .join(root.strip_prefix("~/").unwrap_or(""))
    } else { PathBuf::from(root) };
    lexical(&if expanded.is_absolute() { expanded } else { home.join(expanded) })
}

pub async fn repo_root(cwd: &Path, cancel: CancellationToken) -> Result<PathBuf> {
    let raw = command::checked(cwd,
        &["rev-parse".into(), "--path-format=absolute".into(), "--git-common-dir".into()],
        &BTreeMap::new(), cancel, command::READ_LIMIT, command::MUTATION_TIMEOUT).await?;
    let raw = String::from_utf8(raw)?;
    let common = normalize_path(Path::new(raw.trim_end_matches(['\r', '\n']))).await?;
    Ok(if common.file_name().is_some_and(|name| name == ".git") {
        common.parent().unwrap_or(&common).to_owned()
    } else { common })
}

fn placement<'a>(resolved: &'a Path, base: &Path) -> Option<(PathBuf, &'a std::ffi::OsStr)> {
    let mut parts = resolved.strip_prefix(base).ok()?.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(hash)), Some(Component::Normal(slug))) => Some((base.join(hash), slug)),
        _ => None,
    }
}

/// Identifies historical placement only; this never grants mutation or deletion rights.
pub async fn in_worktrees_root(home: &Path, config: &Value, cwd: &Path) -> Result<bool> {
    let resolved = normalize_path(cwd).await?;
    let base = normalize_path(&base_root(home, config)?).await?;
    Ok(placement(&resolved, &base).is_some())
}

pub async fn ownership(home: &Path, config: &Value, cwd: &Path, cancel: CancellationToken) -> Result<Value> {
    let resolved = normalize_path(cwd).await?;
    let base = normalize_path(&base_root(home, config)?).await?;
    let mut result = json!({"allowed":false,"worktreePath":resolved});
    if let Ok(repo) = repo_root(cwd, cancel.clone()).await { result["repoRoot"] = json!(repo); }
    if cancel.is_cancelled() { bail!("Checkout inspection cancelled"); }
    if let Some((root, slug)) = placement(&resolved, &base) {
        result["allowed"] = json!(true);
        result["worktreePath"] = json!(root.join(slug));
    }
    Ok(result)
}

pub async fn inspect_recovery(id: &str, workspace: Option<&Value>, project: Option<&Value>) -> Result<Value> {
    let unavailable = |reason: &str, message: &str| json!({"kind":"unavailable","workspaceId":id,"reason":reason,"message":message});
    let Some(workspace) = workspace else {
        return Ok(unavailable("workspace_not_found", "This directory record is no longer known to the host."));
    };
    if workspace["archivedAt"].is_null() {
        return Ok(unavailable("workspace_not_archived", "This directory record is not archived."));
    }
    if project.is_none() {
        return Ok(unavailable("project_not_found", "The project for this directory no longer exists."));
    }
    let cwd = workspace["cwd"].as_str().ok_or_else(|| anyhow!("Missing cwd"))?;
    match tokio::fs::metadata(cwd).await {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(unavailable("workspace_directory_missing", "The directory no longer exists and cannot be recreated.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(unavailable("workspace_directory_missing", "The directory no longer exists and cannot be recreated.")),
        Err(error) => return Err(error.into()),
    }
    let name = workspace["title"].as_str().or_else(|| workspace["displayName"].as_str()).unwrap_or(id);
    Ok(json!({"kind":"recoverable","workspaceId":id,"workspaceName":name,"action":"unarchive","branch":workspace.get("branch").unwrap_or(&Value::Null)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archived_worktree_recovery_never_recreates_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("missing");
        let workspace = json!({"cwd":cwd,"kind":"worktree","archivedAt":"old","isPaseoOwnedWorktree":true});
        let state = inspect_recovery("w", Some(&workspace), Some(&json!({"rootPath":root.path()}))).await.unwrap();
        assert_eq!(state["kind"], "unavailable");
        assert_eq!(state["reason"], "workspace_directory_missing");
        assert!(!cwd.exists());
    }

    #[tokio::test]
    async fn archived_external_worktree_can_be_unarchived_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let workspace = json!({"cwd":root.path(),"kind":"worktree","archivedAt":"old"});
        let state = inspect_recovery("w", Some(&workspace), Some(&json!({}))).await.unwrap();
        assert_eq!(state["action"], "unarchive");
        assert!(root.path().exists());
    }
}
