use crate::cli::{Context, flag, option, output::Output, positional, required, string, switch};
use anyhow::{Result, anyhow, bail, ensure};
use clap::{ArgMatches, Command};
use paseo_client::{ClientError, DaemonClient, DaemonTarget};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
const RPC: Duration = Duration::from_secs(30);
const PROJECT: &[(&str, &str)] = &[
    ("PROJECT ID", "projectId"),
    ("NAME", "name"),
    ("KIND", "kind"),
    ("PATH", "path"),
];
pub fn commands() -> Vec<Command> {
    let project = Command::new("project")
        .about("Manage projects")
        .subcommand(Command::new("create").arg(positional("path", false)))
        .subcommand(Command::new("ls"))
        .subcommand(
            Command::new("rename")
                .arg(positional("project-id", true))
                .arg(positional("name", false))
                .arg(switch("reset", "reset")),
        )
        .subcommand(Command::new("delete").arg(positional("project-id", true)));
    let clone = Command::new("clone")
        .about("Clone a repository and register its project")
        .arg(positional("repo", true))
        .arg(option("dir", "dir").required(true))
        .arg(option("protocol", "protocol").value_parser(["https", "ssh"]));
    vec![project, clone]
}
pub async fn execute(
    path: &[&str],
    matches: &ArgMatches,
    context: &Context,
) -> Result<Option<Output>> {
    if !matches!(path.first(), Some(&"project" | &"clone")) {
        return Ok(None);
    }
    if path.len() == 1 && path[0] != "clone" {
        return Ok(None);
    }
    let client = context.connect().await?;
    let result = match path[0] {
        "project" => project(path[1], matches, &client, context).await,
        "clone" => clone_repo(matches, &client).await,
        _ => unreachable!(),
    };
    client.close().await;
    result.map(Some)
}
async fn rpc(
    client: &DaemonClient,
    mut message: Value,
    response: &str,
    context: Option<&Context>,
) -> Result<Value> {
    if message.get("requestId").is_none() {
        message["requestId"] = uuid::Uuid::new_v4().to_string().into();
    }
    let result = if let Some(context) = context {
        client
            .request_cancellable(message, response, RPC, context.cancel.clone())
            .await?
    } else {
        client.request(message, response, RPC).await?
    };
    if let Some(error) = result.get("error").filter(|e| !e.is_null())
        && let Some(message) = error["message"].as_str()
    {
        return Err(ClientError::new(
            error["code"].as_str().unwrap_or("DAEMON_RPC_ERROR"),
            message,
        )
        .into());
    }
    Ok(result)
}
fn project_row(value: &Value) -> Value {
    json!({"projectId":value["projectId"],"name":value["projectDisplayName"],"kind":value["projectKind"],"path":value["projectRootPath"]})
}
fn rename_value(matches: &ArgMatches, key: &str) -> Result<Value> {
    let value = string(matches, key);
    if flag(matches, "reset") {
        ensure!(value.is_none(), "--reset cannot be combined with a {key}");
        Ok(Value::Null)
    } else {
        let value = value.unwrap_or("").trim();
        ensure!(!value.is_empty(), "{key} cannot be empty");
        Ok(value.into())
    }
}
async fn project(
    action: &str,
    m: &ArgMatches,
    client: &DaemonClient,
    context: &Context,
) -> Result<Output> {
    match action {
        "ls" => {
            let payload = rpc(
                client,
                json!({"type":"project.list.request"}),
                "project.list.response",
                Some(context),
            )
            .await?;
            let rows = payload["projects"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid project list"))?
                .iter()
                .map(project_row)
                .collect::<Vec<_>>();
            Ok(Output::new(json!(rows))
                .id("projectId")
                .columns(PROJECT)
                .widths(&[20, 24, 10, 42]))
        }
        "create" => {
            let raw = string(m, "path");
            let cwd = match &context.target {
                DaemonTarget::Endpoint { .. } => raw
                    .ok_or_else(|| {
                        ClientError::new(
                            "MISSING_PATH",
                            "Project path is required when targeting a daemon explicitly",
                        )
                    })?
                    .to_owned(),
                _ => absolute(raw.unwrap_or("."))?,
            };
            let payload = rpc(
                client,
                json!({"type":"project.add.request","cwd":cwd}),
                "project.add.response",
                Some(context),
            )
            .await?;
            ensure!(payload["project"].is_object(), "Project creation failed");
            Ok(Output::new(project_row(&payload["project"]))
                .id("projectId")
                .columns(PROJECT)
                .widths(&[20, 24, 10, 42]))
        }
        "rename" => {
            let id = required(m, "project-id")?;
            let payload=rpc(client,json!({"type":"project.rename.request","projectId":id,"customName":rename_value(m,"name")?}),"project.rename.response",Some(context)).await?;
            ensure!(payload["accepted"] == true, "renameProject rejected");
            Ok(
                Output::new(json!({"projectId":id,"name":payload["customName"]}))
                    .id("projectId")
                    .columns(&[("PROJECT ID", "projectId"), ("NAME", "name")]),
            )
        }
        "delete" => {
            let id = required(m, "project-id")?;
            let payload = rpc(
                client,
                json!({"type":"project.remove.request","projectId":id}),
                "project.remove.response",
                Some(context),
            )
            .await?;
            ensure!(payload["accepted"] == true, "removeProject rejected");
            let count = payload["removedWorkspaceIds"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            Ok(Output::new(
                json!({"projectId":id,"removedWorkspaceIds":payload["removedWorkspaceIds"]}),
            )
            .table_data(json!({"projectId":id,"removedWorkspaceCount":count}))
            .id("projectId")
            .columns(&[
                ("PROJECT ID", "projectId"),
                ("REMOVED WORKSPACES", "removedWorkspaceCount"),
            ])
            .widths(&[20, 18]))
        }
        _ => bail!("Unknown project command {action}"),
    }
}
fn copy(m: &ArgMatches, target: &mut Value, keys: &[(&str, &str)]) {
    for (from, to) in keys {
        if let Some(value) = string(m, from) {
            target[*to] = value.into();
        }
    }
}
async fn clone_repo(m: &ArgMatches, client: &DaemonClient) -> Result<Output> {
    let repo = required(m, "repo")?;
    let dir = required(m, "dir")?.trim();
    ensure!(!dir.is_empty(), "--dir is required");
    let complete = repo.contains("://")
        || repo
            .split_once(':')
            .is_some_and(|(host, path)| host.contains('@') && !path.is_empty());
    ensure!(
        complete || string(m, "protocol").is_some(),
        "--protocol is required for owner/repo repository names"
    );
    client.require_feature("projectGithubClone","This daemon does not support cloning GitHub repos. Update the host to a newer Paseo version.")?;
    let mut request =
        json!({"type":"project.github.clone.request","repo":repo,"targetDirectory":dir});
    if !complete {
        copy(m, &mut request, &[("protocol", "cloneProtocol")]);
    }
    let response = client
        .request(
            request,
            "project.github.clone.response",
            Duration::from_secs(600),
        )
        .await?;
    ensure!(
        response["project"].is_object() && response["checkoutPath"].is_string(),
        "Failed to clone GitHub repo: no project returned"
    );
    Ok(Output::new(json!({"repo":response["repo"],"checkoutPath":response["checkoutPath"],"projectId":response["project"]["projectId"],"projectName":response["project"]["projectDisplayName"]})).id("projectId").columns(&[("REPO","repo"),("PROJECT","projectName"),("PATH","checkoutPath")]))
}
pub(super) fn absolute(path: &str) -> Result<String> {
    let path = PathBuf::from(path);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    }
    .components()
    .fold(PathBuf::new(), |mut out, part| {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
        out
    })
    .to_string_lossy()
    .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_catalog_has_no_workspace_or_worktree_commands() {
        let commands = commands();
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command.get_name(), "workspace" | "worktree"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.get_name() == "project")
        );
        assert!(commands.iter().any(|command| command.get_name() == "clone"));
    }
}
