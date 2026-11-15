---
title: 项目脚本与服务
description: 在现有会话目录中运行具名命令和开发服务。
nav: 脚本与服务
---

# 项目脚本与服务

会话使用所选主机上的现有目录。Paseo 不会创建或删除 Git worktree，不运行 worktree setup/teardown 钩子，也不为会话准备分支。请在正常的项目工作流中显式安装依赖和准备文件。

## 脚本与服务 {#scripts-and-services}

在项目的 `paseo.json` 中定义具名命令：

```json
{
  "scripts": {
    "test": { "command": "npm test" },
    "lint": { "command": "npm run lint" },
    "web": {
      "type": "service",
      "command": "npm run dev -- --port $PASEO_PORT",
      "port": 3000
    }
  }
}
```

普通脚本按需运行。服务是受监管的长时间运行进程，具有分配的端口和反向代理 URL。daemon 拥有它们的 PTY；客户端显示只读日志。需要交互式 shell 或 REPL 时，请打开 **Terminal** 会话。

通过 [CLI](/docs/cli#scripts) 选择目录或会话：

```bash
paseo script ls --cwd ~/dev/my-app
paseo script start web --agent <agent-id>
paseo script stop web --agent <agent-id>
```

[MCP 脚本工具](/docs/mcp#scripts)为 `list_scripts`、`start_script` 和 `stop_script`。它们接受 `agentId`，默认为调用方 Agent；顶层调用方必须提供它。启动和停止还需要 `scriptName`。

## 服务端口 {#service-ports}

让服务绑定 `$PASEO_PORT`。脚本中显式设置的 `port` 优先；省略时动态分配，默认向操作系统请求可用的临时端口。

端口策略保留历史配置键。daemon 的 `config.json` 中，`worktrees.servicePorts` 提供默认值；项目 `paseo.json` 中的 `worktree.servicePorts` 覆盖它。这些设置只分配服务端口，不创建 worktree：

```json
{
  "worktree": {
    "servicePorts": { "range": "3000-4000" }
  },
  "scripts": {
    "web": { "type": "service", "command": "npm run dev -- --port $PASEO_PORT" }
  }
}
```

范围包含两端。使用外部分配器时，在同一策略对象中把 `portScript` 设为可执行文件路径。它在执行目录中直接运行，不经过 shell，并必须输出一个 TCP 端口。参数依次为脚本名、内部目录归属 ID、分支名和检出路径；没有分支时传入空字符串。`portScript` 优先于 `range`。返回的端口可能已被占用，这由分配器负责判断。

## 服务 URL 与环境 {#service-urls-and-environment}

使用 Paseo 返回的服务 URL。daemon 的反向代理支持 WebSocket 升级。本地 URL 形如：

```text
http://<script>--<branch>--<project>.localhost:<daemon-port>
```

默认分支的标签会省略。在现代系统中，`*.localhost` 解析为回环地址；主机上的 URL 不代表其他设备可以直接访问。参见[连接](/docs/connectivity)。

服务获得：

- `$PASEO_PORT`：该服务的端口。
- `$PASEO_URL`：该服务的代理 URL。
- `$PASEO_SERVICE_<NAME>_PORT` 和 `$PASEO_SERVICE_<NAME>_URL`：同一内部目录归属下其他服务的信息。名称转为大写，非字母数字字符变为 `_`。
- `$HOST`：daemon 本地或网络服务策略选择的绑定地址。

停止脚本会释放其运行进程。关闭日志视图只停止观察。归档会话或定时运行不会删除目录或 Git worktree。
