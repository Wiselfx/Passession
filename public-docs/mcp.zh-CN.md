---
title: MCP 参考
description: Agent 用来管理会话、脚本、终端和定时任务的 Paseo 工具参考。
nav: MCP 参考
---

# MCP 参考 {#mcp-reference}

[启用 Paseo 工具](/docs/orchestration#get-started)，让 Agent 获得这套工具目录。用自然语言说明想要的结果，或者使用下面的工具接口。

## 配置 {#configuration}

| 设置                          | 默认值  | 用途                                     |
| ----------------------------- | ------- | ---------------------------------------- |
| `daemon.mcp.enabled`          | `true`  | 运行 MCP 服务器。                        |
| `daemon.mcp.injectIntoAgents` | `false` | 让 Paseo 启动的 Agent 可以使用它的工具。 |

根据 Provider 的不同，Paseo 通过其原生工具接口或 MCP 提供工具，能力是相同的。修改注入设置后，请启动新的 Agent 或重新加载已有的 Agent。

## 按 Provider 限制 Paseo 工具 {#limit-paseo-tools-by-provider}

当不同的 Agent 配置应获得不同的 Paseo 工具时，请使用 Provider 策略。先全局启用工具注入，然后在你启动所用的确切 Provider ID 上添加 `paseoTools`：

```json
{
  "$schema": "https://paseo.sh/schemas/paseo.config.v1.json",
  "version": 1,
  "daemon": {
    "mcp": {
      "enabled": true,
      "injectIntoAgents": true
    }
  },
  "agents": {
    "providers": {
      "codex-lead": {
        "extends": "codex",
        "label": "Codex Lead"
      },
      "codex-worker": {
        "extends": "codex",
        "label": "Codex Worker",
        "paseoTools": {
          "disabledTools": ["create_agent", "send_agent_prompt", "kill_agent"]
        }
      },
      "codex-isolated": {
        "extends": "codex",
        "label": "Codex Isolated",
        "paseoTools": {
          "enabled": false
        }
      }
    }
  }
}
```

编辑 `~/.paseo/config.json` 后运行 `paseo reload`，然后启动新的 Agent 或重新加载已有的 Agent。正在运行的会话保留它启动时得到的工具目录。

省略 `paseoTools` 会启用完整的工具目录。把 `enabled` 设为 `false` 会移除整个目录，在 `disabledTools` 中列出确切的工具 ID 则只移除所选的工具。自定义配置不会从 `extends` 继承这一策略；请为每个自定义 Provider ID 分别配置。

原生运行时没有内嵌浏览器自动化工具或浏览器主机。

这项设置限制的是呈现给 Agent 的工具目录。对于可以通过 shell 访问主机的 Agent，它不是安全边界。

## 心智模型 {#mental-model}

会话运行在现有目录中；Agent 的父子关系与目录选择相互独立。

- Agent 调用 `create_agent` 时可以省略 `cwd`，使用自己的目录。
- 传入 `cwd` 可选择目标主机上的另一个现有目录，不会让子 Agent 脱离父 Agent。
- 顶层 MCP 调用方必须提供 `cwd`。
- Paseo 不会为会话创建 Git worktree 或准备分支。

MCP 不提供解除 Agent 父子关系的工具。解除关系是用户在应用或 CLI 中手动执行的操作。

## 工具 {#tools}

### Agent {#agents}

| 工具                 | 功能                                                                                                 |
| -------------------- | ---------------------------------------------------------------------------------------------------- |
| `create_agent`       | 在 `cwd` 中创建会话；顶层调用方必须提供目录，Agent 调用默认使用自身目录。用 `initialPrompt` 开始工作，不等待完成即返回。 |
| `send_agent_prompt`  | 用 `agentId` 和 `prompt` 发送提示词；不等待完成就返回。                                              |
| `get_agent_status`   | 返回 Agent 的最新快照。                                                                              |
| `list_agents`        | 以精简元数据列出最近的 Agent；`includeBackground: true` 会包含后台工作。                             |
| `cancel_agent`       | 中止 Agent 当前的运行，但保留该 Agent。                                                              |
| `archive_agent`      | 软删除 Agent，并把它从活动列表中移除。                                                               |
| `kill_agent`         | 永久终止 Agent 会话。                                                                                |
| `update_agent`       | 更新 Agent 的名称、标签或运行时设置，例如模式、模型、思考选项和功能。                                |
| `get_agent_activity` | 以整理后的摘要返回 Agent 最近的时间线条目。                                                          |
| `set_agent_mode`     | 切换 Agent 的会话模式。                                                                              |

Paseo Agent 默认会收到完成、错误和权限通知；设置 `notifyOnFinish: false` 可以关闭它们。外部 MCP 客户端使用 `get_agent_status` 和 `get_agent_activity` 查看结果。

### 脚本 {#scripts}

这些工具管理 `paseo.json` 中的脚本。`agentId` 选择会话的目录归属，默认使用调用方 Agent；顶层调用方必须提供它。启动和停止还需要 `scriptName`。

| 工具                     | 功能                                                                  |
| ------------------------ | --------------------------------------------------------------------- |
| `list_scripts` | 列出已配置脚本及其生命周期、终端、端口、代理 URL 和健康状态元数据。 |
| `start_script` | 通过 Paseo 托管启动器启动已配置脚本。 |
| `stop_script` | 通过受监管的终端停止运行中的脚本。 |

配置见[项目脚本与服务](/docs/worktrees#scripts-and-services)。

### 终端 {#terminals}

这些是 daemon 端 PTY API。原生客户端以 **Terminal** 标签页打开交互式 shell，以只读日志显示脚本输出。`cwd` 默认为调用方 Agent 的目录；顶层调用默认为进程目录。

| 工具                 | 功能                                       |
| -------------------- | ------------------------------------------ |
| `list_terminals`     | 列出某个工作目录或所有工作目录的终端会话。 |
| `create_terminal`    | 为某个工作目录创建终端会话。               |
| `kill_terminal`      | 结束终端会话。                             |
| `capture_terminal`   | 捕获终端会话的纯文本输出。                 |
| `send_terminal_keys` | 向终端会话发送文本或特殊按键标记。         |

### 定时任务与 heartbeat {#schedules-and-heartbeats}

两者使用相同的 cron 引擎，但接口有意设计得不同。

| 工具                | 功能                                                      |
| ------------------- | --------------------------------------------------------- |
| `create_schedule`   | 创建一个 cron 定时任务，每次运行都启动一个新的 Agent。    |
| `list_schedules`    | 列出 daemon 管理的新建 Agent 定时任务。                   |
| `inspect_schedule`  | 查看定时任务及其运行历史。                                |
| `pause_schedule`    | 暂停一个活动的定时任务。                                  |
| `resume_schedule`   | 恢复一个已暂停的定时任务。                                |
| `update_schedule`   | 修改定时任务的 cron、提示词、Agent 设置、限制或其他设置。 |
| `schedule_logs`     | 返回定时任务最近的运行及输出。                            |
| `run_schedule_once` | 启动一次新建 Agent 的定时任务运行，不改变它的 cron。      |
| `delete_schedule`   | 永久删除一个新建 Agent 的定时任务。                       |
| `create_heartbeat`  | 向当前 Agent 发送一段由 cron 驱动的周期性提示词。         |
| `delete_heartbeat`  | 删除当前 Agent 的某个 heartbeat。                         |

MCP heartbeat 是临时的：只能创建或删除。要修改一个 heartbeat，请删除它并创建一个替代的。暂停、恢复、更新、查看、日志和单次运行只适用于新建 Agent 的定时任务。

默认情况下，新 Agent 定时运行结束后会归档其会话并停止所属终端，但不会删除文件或 Git worktree。`create_schedule` 和 `update_schedule` 不提供 **Archive on finish**（`archiveOnFinish`）；要保留之后的运行会话，请[在应用中修改该选项](/docs/schedules#keep-run-sessions)。

### Agent 配置 {#agent-profiles}

| 工具            | 功能                                                                          |
| --------------- | ----------------------------------------------------------------------------- |
| `list_profiles` | 返回主机上保存的 Agent 配置，包括它们的说明和启动设置。没有配置时返回空列表。 |

委派之前，请阅读每个配置的 `notes`，选择用户指定的配置或适合该任务的配置。设置方法和说明示例见 [Agent 配置](/docs/agent-profiles)。

`create_agent` 没有配置参数，并且需要 `provider/model` 组合。如果配置中没有模型，请先对其 Provider 调用 `list_models`，为任务选择一个可用的模型再启动。把所选配置的值应用到启动请求中：

| 配置字段                    | `create_agent` 字段                                            |
| --------------------------- | -------------------------------------------------------------- |
| `provider` 和可选的 `model` | `provider`，格式为 `provider/model`，使用保存的或发现的模型 ID |
| `modeId`                    | `settings.modeId`                                              |
| `thinkingOptionId`          | `settings.thinkingOptionId`                                    |
| `featureValues`             | `settings.features`                                            |
| `notes`                     | 供编排者选择时参考；任务放在 `initialPrompt` 中                |

省略不存在的可选设置。如果没有合适的配置，请用 Provider 发现来选择可用的设置。

### Provider {#providers}

| 工具               | 功能                                        |
| ------------------ | ------------------------------------------- |
| `list_providers`   | 列出已配置的 Agent Provider、可用性和模式。 |
| `list_models`      | 列出某个 Agent Provider 的模型。            |
| `inspect_provider` | 查看精简的 Provider 能力和草稿功能设置。    |

### 权限 {#permissions}

| 工具                       | 功能                                |
| -------------------------- | ----------------------------------- |
| `list_pending_permissions` | 返回所有 Agent 中待处理的权限请求。 |
| `respond_to_permission`    | 批准或拒绝一个待处理的权限请求。    |
