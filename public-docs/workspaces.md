---
title: Sessions and directories
description: Start agent sessions in existing directories on your selected host.
nav: Sessions and directories
order: 10
category: Sessions
---

# Sessions and directories

The sidebar is **host → directory → session**. A session is one agent conversation with its own history and configuration. Sessions open directly as tabs; there is no intermediate workspace or nested session tab bar.

A directory belongs to a host. Equal paths on different hosts stay separate. Empty registered directories remain visible, and search reveals matching ancestors without replacing your saved collapse state.

## Start a session

Choose **New Session**, select a host and project or browse to an existing host-side directory, then choose your model and enter a task. Chat starts a session directly. Choose **Launch → Terminal** for an interactive shell instead.

Several sessions can use the same directory. They share its files, so coordinate edits when running agents in parallel. Paseo does not create, switch, clean up or delete Git worktrees for sessions. An existing Git worktree is usable like any other local directory; manage its branches and directory with your own Git workflow.

From a terminal:

```bash
paseo run --cwd ~/dev/my-app "implement authentication"
paseo run --cwd ~/dev/my-app "review the changes"
```

When an agent creates another agent, parentage is independent of directory selection. See [parallel tasks](/docs/parallel-development) and [MCP](/docs/mcp#mental-model).

## Projects

A project is a registered root on a daemon host. It can be a Git repository or another directory. **Add Project** selects an existing directory; clone and create-directory flows can prepare one first. Registering a project does not start an agent.

Use [CLI project commands](/docs/cli#projects) to register, list or rename projects:

```bash
paseo project create ~/dev/my-app
paseo project ls
```

## History and files

Closing a tab only closes the view. It does not archive the session, stop a service or delete files. History reopens the same conversation. Existing session IDs, drafts and stored histories remain available, including sessions whose directories are Git worktrees.

Use **Git Log** to inspect commits and **Commit** to review and commit selected changes. Open source and Markdown files beside your session. Named commands and development services live in [`paseo.json`](/docs/worktrees#scripts-and-services), with read-only output logs. For a web project's UI, use an external browser; the native client has no embedded browser.
