---
title: 会话与目录
description: 在所选主机的现有目录中启动 Agent 会话。
nav: 会话与目录
---

# 会话与目录

侧边栏按 **主机 → 目录 → 会话** 组织。一个会话对应一段独立的 Agent 对话，拥有自己的历史和配置。会话直接作为标签页打开，没有中间的工作区或嵌套会话标签栏。

目录属于某个主机。不同主机上的相同路径保持独立。空的已注册目录仍然可见，搜索临时展开匹配的上级，不覆盖保存的折叠状态。

## 启动会话

选择 **New Session**，选择主机和项目，或浏览到主机上的现有目录，然后选择模型并输入任务。Chat 直接启动会话。需要交互式 shell 时，选择 **Launch → Terminal**。

多个会话可以使用同一目录，并共享其中的文件。并行运行 Agent 时应协调编辑范围。Paseo 不会为会话创建、切换、清理或删除 Git worktree。已有的 Git worktree 可以像其他本地目录一样使用；其分支和目录由你自己的 Git 工作流管理。

从终端启动：

```bash
paseo run --cwd ~/dev/my-app "implement authentication"
paseo run --cwd ~/dev/my-app "review the changes"
```

Agent 创建另一个 Agent 时，父子关系与目录选择相互独立。参见[并行任务](/docs/parallel-development)和 [MCP](/docs/mcp#mental-model)。

## 项目

项目是 daemon 主机上注册的根目录，可以是 Git 仓库或其他目录。**Add Project** 选择现有目录，也可以先通过克隆或创建目录流程准备目录。注册项目不会启动 Agent。

使用 [CLI 项目命令](/docs/cli#projects)注册、列出或重命名项目：

```bash
paseo project create ~/dev/my-app
paseo project ls
```

## 历史与文件

关闭标签页只关闭视图，不会归档会话、停止服务或删除文件。历史记录重新打开同一段对话。现有的会话 ID、草稿和历史继续保留，包括目录位于 Git worktree 中的会话。

使用 **Git Log** 查看提交，用 **Commit** 审查并提交选定改动。源码和 Markdown 文件可在会话旁打开。具名命令和开发服务配置在 [`paseo.json`](/docs/worktrees#scripts-and-services) 中，其输出为只读日志。网页项目使用外部浏览器；原生客户端没有内嵌浏览器。
