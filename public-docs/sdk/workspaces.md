# Sessions and directories with the SDK

[中文](workspaces.zh-CN.md) · [Setup](quickstart.md)

Start sessions in an existing local directory on the daemon host. Paseo does not create or manage Git worktrees or run automatic setup/teardown hooks. Existing directories, externally managed worktrees, session history, and ownership records stay in place.

## Start a session

```rust
use anyhow::Result;
use paseo_client::{DaemonClient, RequestOptions};
use serde_json::{Value, json};

async fn create_work(client: &DaemonClient, cwd: &str, model: &str) -> Result<Value> {
    client.create_agent(json!({
        "config": {
            "provider": "codex", "model": model, "cwd": cwd,
            "title": "Checkout review"
        },
        "initialPrompt": "Map the checkout flow before changing anything."
    }), RequestOptions::default()).await
}
```

Persist the returned `agent.id` for follow-ups. The SDK takes placement in `config.cwd`; this differs from the public MCP `create_agent` tool's top-level `cwd`. Read the agent snapshot for its actual directory and `workspaceId` when a lower-level SDK operation needs ownership context.

## Internal ownership compatibility

`workspaceId` remains a durable, daemon-scoped ownership identity for sessions, terminals, and scripts. It is not a separate product workspace or a request to provision a checkout. Several retained records can refer to the same directory; do not replace or merge their IDs based on the path.

The Rust SDK and wire protocol retain workspace methods and types for compatible integrations. `open_project({cwd})` can resolve an existing directory's internal record. `create_workspace` still accepts a directory source, but worktree sources are explicitly unsupported. New session workflows should use `create_agent` directly. Retained background metadata and workspace observations are internal registry features, not sidebar placement controls.

An explicit SDK `workspaceId` must match `config.cwd`. Preserve IDs and history from older installations. Archiving a session does not delete its directory or Git worktree, and finishing a session does not automatically archive its internal owner.

## Start a terminal for a session

```rust
use anyhow::{Context, Result};
use paseo_client::DaemonClient;
use serde_json::json;

async fn run_log(client: &DaemonClient, agent_id: &str) -> Result<()> {
    let fetched = client.fetch_agent(json!({"agentId": agent_id})).await?;
    let agent = &fetched["agent"];
    let workspace_id = agent["workspaceId"].as_str().context("missing ownership ID")?;
    let cwd = agent["cwd"].as_str().context("missing directory")?;
    let created = client.create_terminal(json!({
        "workspaceId": workspace_id, "cwd": cwd, "name": "Repository status",
        "command": "git", "args": ["status", "--short"],
        "size": {"rows": 24, "cols": 100}
    })).await?;
    let id = created["terminal"]["id"].as_str().context("missing terminal ID")?;
    let capture = client.capture_terminal(json!({"terminalId": id, "stripAnsi": true})).await;
    let cleanup = client.kill_terminal(json!({"terminalId": id})).await;
    println!("{}", capture?);
    cleanup?;
    Ok(())
}
```

Capture is a point-in-time snapshot, not a wait for process exit. To wait for exit/output use `Arc<DaemonClient>::observe_terminal`, receive `TerminalEvent`, and release its subscription; see [reference](reference.md#terminals-and-files). Pass the session's internal `workspaceId` to disambiguate owners sharing a directory. `list_terminals` accepts the same context. Input is available through `TerminalStream::input`; command arguments are separate from the executable. Never interpolate untrusted text into shell commands.

The native app opens interactive shells as **Terminal** tabs and shows script output as read-only logs. Start configured scripts explicitly; see [project scripts](../worktrees.md#scripts-and-services). Public MCP script tools use `agentId`; retained SDK script methods use `workspaceId` from the session snapshot.

## Observe and archive sessions

Use `fetch_agent`, `fetch_agents`, or an [owned agent observation](events.md) to follow sessions. Archive only your own temporary agents with `archive_agent`. Closing an SDK connection releases its observations, not the agents or their directories.

For integrations that maintain internal registry state, `fetch_workspaces`, `CursorPages::workspaces`, and `observe_workspaces` remain available. `filter.includeBackground: true` includes retained background records. Explicit registry archival/restoration methods remain wire-compatible; restoration requires the directory to exist and does not recreate a Git worktree. Do not archive unrelated ownership records as session cleanup.
