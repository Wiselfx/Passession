# 在 SDK 中使用会话与目录

[English](workspaces.md) · [环境配置](quickstart.zh-CN.md)

在 daemon 主机上已有的本地目录中启动会话。Paseo 不创建或管理 Git worktree，也不自动运行初始化/清理钩子。已有目录、外部管理的 worktree、会话历史及归属记录均保留原位。

## 启动会话

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

保存返回的 `agent.id`，用于后续消息。SDK 通过 `config.cwd` 指定目录，与公开 MCP `create_agent` 工具的顶层 `cwd` 不同。底层 SDK 操作需要归属上下文时，从 Agent 快照读取实际目录和 `workspaceId`。

## 内部归属兼容性

`workspaceId` 仍是会话、终端及脚本的持久归属标识，作用域限于所属 daemon。它不是独立的产品工作区，也不要求创建检出目录。多个保留记录可能指向同一目录，不要根据路径替换或合并其 ID。

Rust SDK 和协议为兼容集成保留工作区方法与类型。`open_project({cwd})` 可解析已有目录的内部记录。`create_workspace` 仍接受目录来源，但明确不支持 worktree 来源。新的会话流程应直接使用 `create_agent`。保留的后台元数据和工作区观察属于内部注册表功能，不是侧栏位置控制。

显式 SDK `workspaceId` 必须与 `config.cwd` 匹配。保留旧安装的 ID 和历史。归档会话不会删除目录或 Git worktree，会话完成也不会自动归档其内部归属记录。

## 为会话启动终端

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

Capture 是某一时刻的快照，不等待进程退出。需要等待输出/退出时，使用 `Arc<DaemonClient>::observe_terminal` 接收 `TerminalEvent`，并释放订阅；参见[参考](reference.zh-CN.md#terminals-and-files)。传入会话的内部 `workspaceId`，以区分共享目录的归属记录。`list_terminals` 接受相同上下文。通过 `TerminalStream::input` 输入；可执行文件与参数分开传递，不要把不可信文本拼入 shell 命令。

原生应用以 **Terminal** 标签页打开交互式 shell，并以只读日志显示脚本输出。显式启动已配置脚本，参见[项目脚本](../worktrees.zh-CN.md#scripts-and-services)。公开 MCP 脚本工具使用 `agentId`；保留的 SDK 脚本方法使用会话快照中的 `workspaceId`。

## 观察与归档会话

通过 `fetch_agent`、`fetch_agents` 或[持有式 Agent 观察](events.zh-CN.md)跟踪会话。仅使用 `archive_agent` 归档自己拥有的临时 Agent。关闭 SDK 连接释放其观察，不影响 Agent 或目录。

维护内部注册表状态的集成仍可使用 `fetch_workspaces`、`CursorPages::workspaces` 和 `observe_workspaces`。`filter.includeBackground: true` 包含保留的后台记录。显式注册表归档/恢复方法保留协议兼容性；恢复要求目录存在，不会重新创建 Git worktree。不要把归档无关归属记录当作会话清理。
