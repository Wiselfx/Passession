# Architecture

Paseo is a native client/server system. **Rust owns the daemon, the CLI and the Linux, macOS, Windows, Android and iOS clients** (Makepad 1.0.0 widgets). Desktop can run a local daemon. Mobile clients connect to a desktop/server daemon and do not ship or start one.

```text
Rust desktop client ── native desktop-host IPC ── Rust lifecycle/supervisor
       │                                            │
Rust mobile client ── WebSocket / encrypted relay ── Rust daemon
Rust CLI / external Rust SDK ─────────────────────┤
                                                  ├─ provider processes
                                                  ├─ persistent state / files / Git
                                                  ├─ terminal and script PTYs
                                                  ├─ MCP / schedules / connected hosts
                                                  └─ optional native plugin executables
```

Paseo's runtime, SDKs and build tools require neither Node, Electron, nor Chromium. Optional backend plugins run as native executables with bounded JSONL IPC. External provider programs retain their own installation and runtime requirements.

## Ownership

| Component                   | Source                                                                                 | Responsibility                                                                            |
| --------------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Native executable           | `packages/daemon/src/main.rs`, `cli.rs`, `cli/`                                        | CLI dispatch, daemon commands, client operations                                          |
| Transport                   | `packages/daemon/src/transport/`, `packages/client/src`, `packages/protocol/src/crypto` | HTTP/WebSocket, authentication, relay encryption, local/remote SDK and CLI connections |
| State and configuration     | `state.rs`, `config.rs`, `config/`                                                     | Config resolution, durable state, mutation transactions                                   |
| Agents                      | `agent.rs`, `agent/`, `providers/`                                                     | Provider processes, permissions, lifecycle, snapshots, timeline persistence and streaming |
| Projects and directory ownership | `projects.rs`, `projects/`, `directory_sync/` | Existing-directory identity, retained ownership registries and reconciliation |
| Files and Git               | `files/`, `git/`                                                                       | Remote picker, reads/writes, observation, Git facts and lazy diff bodies                  |
| Terminals and scripts       | `terminals.rs`, `terminals/` | PTY lifecycle, interactive shells, service scripts, screen state and replayable output |
| Automation and integrations | `automation/`, `mcp/`, `mcp_client/`, `hosts/`, `hub/`                                 | Schedules, MCP client/server, connected-host and Hub behavior                             |
| Desktop lifecycle           | `desktop_host.rs`, `desktop_host/`, `daemon_control/`                                  | Structured IPC, process identity, ownership, supervisor and worker lifecycle              |
| Client runtime              | `packages/gui-core/src`                                                                | Host connections, directory synchronization, persisted client state, request/event boundary |
| Client UI                   | `packages/gui/src` (`shell/`, `common/`, `features/*`)                                 | Makepad shell, navigation, tabs/windows, feature views and controllers                    |
| Platform integration        | `packages/gui-platform/src`, `packages/gui-platform/{android,ios}`                     | Desktop host, updater, tray, notifications, pickers, audio, mobile bridges                |
| Terminal emulation          | `packages/terminal`                                                                    | Ghostty VT engine shared by daemon screens, terminal tabs and logs                        |
| Packaging                   | `xtask/src`, `packaging/`                                                              | Native runtime assets, installers, provenance, updates, attributions                      |

Paths in the daemon rows are relative to `packages/daemon/src` unless otherwise shown. The client (`paseo-client`), shared protocol (`paseo-protocol`), relay (`paseo-relay`), plugin SDK (`paseo-plugin`), highlighting (`paseo-highlight`) and terminal emulation (`paseo-terminal`, wrapping Ghostty's `libghostty-vt`) are Rust workspace crates. `paseo-xtask` owns development and release commands. The unused website source is removed; Markdown documentation remains in the repository.

## Protocol and persistence

The existing JSON request/response and event contracts, binary stream framing, request IDs, session IDs, provider-native session identity, and configuration formats are preserved. `packages/protocol` supplies Rust wire types, validation and shared cryptography for the daemon and, through `paseo-client`, for the native client. Compatibility work must still exercise the wire boundary against older daemons and clients, not just compile one side.

The default daemon home is `~/.paseo`. `PASEO_HOME` and CLI `--home` select another instance. State remains in that home: `config.json`, agent/session records and history, project/workspace records, schedules, plugin configuration, credentials and runtime bookkeeping. A UI cutover is not permission to delete, reset, or relocate a user's state. Checkout development defaults to `.dev/paseo-home` and `127.0.0.1:6768`, separate from the ordinary `6767` endpoint.

A client disconnect does not end a provider turn or archive a session. Daemon-owned operations must survive socket replacement. Request failures, persistence failures, and unsupported capabilities are visible; the client must not acknowledge a durable mutation before it succeeds.

## Client state and navigation

`packages/gui-core` owns client protocol behavior and shared application state behind `GuiCore`/`CoreHandle`; network and disk IO never run on the GUI thread. `packages/gui/src/shell` composes the native shell and routes; feature widgets under `packages/gui/src/features` own their paged data.

The primary navigation tree is **host → directory → session**. Keys include host identity: equal paths on two machines must not merge. Empty directories remain visible. Search temporarily reveals ancestors without overwriting saved collapse state. Sessions open directly as tabs without an intermediate workspace or nested session tab bar. Internal `workspaceId` records remain durable directory owners for protocol, history and runtime state; they are not exposed as a product creation or selection concept.

File operations use the selected host. New Session and Add Project browse host-side directories through the daemon, never client-device paths. New work reuses an existing directory, including a user-managed Git worktree; Paseo does not create, restore or remove worktrees. Markdown and source editing are native views. Git Log reads commit history, while Commit reviews and submits selected changes; patch bodies load on demand.

## Desktop ownership

The client talks to the Rust `desktop-host` process through structured IPC (`paseo-gui-platform`). Desktop lifecycle tracks the daemon's process-start identity and whether this application owns its supervisor. A pre-existing daemon is not treated as a child to kill on quit. Keep-running policy and owned-stop behavior are distinct from hiding or closing a window. A supervisor restart must not create a second competing daemon.

All desktop windows live in one process and share one set of host connections; each window or pane keeps its own route and selection. A second launch forwards its arguments to the running primary process and exits. Closing a window or tab neither archives sessions nor stops a pre-existing daemon; owned-daemon cleanup runs only at application quit, after every document close guard has resolved.

Platform tray/menu/window behavior is native integration, not an Electron renderer feature. Runtime verification is platform-specific; Linux lifecycle evidence does not establish Windows or macOS execution.

## Terminals and excluded surfaces

Daemon PTYs back interactive terminals and project scripts. The client opens interactive shells as Terminal tabs on every platform and presents script output as read-only logs; see [terminal activity](terminal-activity.md). Native CLI chat remains available outside the application.

The client has no embedded browser, browser automation, web target, Chromium capture surface, or executable React UI-plugin host. Links open externally. Backend plugin RPCs/providers/lifecycle/usage contributions remain an optional extension boundary, described in [plugins](plugins.md); a server extension does not imply support for its former React client entry.

## Runtime assets and release boundary

Desktop packaging carries the native executable, skills, native runtime assets and desktop editor assets. It does not carry a Node executable, JavaScript plugin compiler or speech runtime. Speech recognition, voice conversation and TTS are excluded; generic effect-sound playback remains.

Cargo and bundled-asset notices are generated by the Rust tooling in `xtask/src/notices.rs`. Preserve them with the packaged artifacts. See [release requirements](release.md#distribution-requirements).
