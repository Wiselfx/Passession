---
title: Project scripts and services
description: Run named commands and development services in an existing session directory.
nav: Scripts and services
order: 11
category: Sessions
---

# Project scripts and services

Sessions use existing directories on the selected host. Paseo does not create or remove Git worktrees, run worktree setup/teardown hooks, or prepare branches for sessions. Install dependencies and prepare files explicitly in your normal project workflow.

## Scripts and services

Define named commands in the project's `paseo.json`:

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

Plain scripts run on demand. Services are supervised long-running processes with assigned ports and reverse-proxy URLs. The daemon owns their PTYs; clients show read-only logs. Open a **Terminal** session for an interactive shell or REPL instead.

Use the [CLI](/docs/cli#scripts) to select a directory or session:

```bash
paseo script ls --cwd ~/dev/my-app
paseo script start web --agent <agent-id>
paseo script stop web --agent <agent-id>
```

The [MCP script tools](/docs/mcp#scripts) are `list_scripts`, `start_script` and `stop_script`. They accept `agentId`, defaulting to the calling agent; a top-level caller must provide it. Start and stop also require `scriptName`.

## Service ports

Bind your service to `$PASEO_PORT`. An explicit script `port` wins; omit it to use dynamic allocation. By default the daemon asks the OS for an available ephemeral port.

Port policies retain their historical configuration keys. `worktrees.servicePorts` in daemon `config.json` supplies the default, and `worktree.servicePorts` in project `paseo.json` overrides it. These settings allocate service ports, not worktrees:

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

The range is inclusive. To use an external allocator, set `portScript` to an executable path in the same policy object. It runs without a shell, in the execution directory, and must print one TCP port. Its arguments are the script name, internal directory-owner ID, branch name and checkout path; a missing branch is empty. `portScript` takes precedence over `range`. The returned port can already be in use, so the allocator owns that decision.

## Service URLs and environment

Use the service URL reported by Paseo. The daemon's reverse proxy supports WebSocket upgrades. Local URLs have this shape:

```text
http://<script>--<branch>--<project>.localhost:<daemon-port>
```

The default branch label is omitted. `*.localhost` resolves to the loopback interface on modern systems; a URL on the host is not automatically reachable from another device. See [connectivity](/docs/connectivity).

Services receive:

- `$PASEO_PORT`: this service's port.
- `$PASEO_URL`: this service's proxy URL.
- `$PASEO_SERVICE_<NAME>_PORT` and `$PASEO_SERVICE_<NAME>_URL`: peer services in the same internal directory owner. Names are upper-cased and non-alphanumerics become `_`.
- `$HOST`: the bind address chosen for the daemon's local or network-facing service policy.

Stopping a script releases its running process. Closing its log view only stops observation. Archiving a session or scheduled run never deletes its directory or Git worktree.
