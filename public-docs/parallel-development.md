---
title: Run parallel tasks
description: Run agent sessions in existing directories, review summary-first diffs, and inspect script output in the native client.
nav: Run parallel tasks
order: 12
category: Sessions
---

# Run parallel tasks

Use the native client to give several coding agents independent tasks, review their changes, and run project scripts. Each task runs as a session in an existing directory, alongside conversations, source/Markdown views, diffs and read-only script logs.

Start with the [desktop app](/docs#desktop-app-recommended), existing local directories for your tasks, and at least one [coding agent installed and authenticated](/docs/supported-providers). Prepare separate checkouts yourself if the tasks need independent files or branches. Existing Git worktrees are usable as local directories; Paseo does not create or manage them.

## Start two tasks

1. Open **New session** and choose the host and an existing directory.
2. Use the first task's directory, for example `/projects/list-filter`.
3. Choose an agent and model, then submit a task, for example: “Add filtering to the project list. Run the relevant tests.”
4. Create a second session in another existing directory, such as `/projects/settings-keyboard`. Give its agent another task: “Add keyboard navigation to the settings page. Run the relevant tests.”

Both agents run concurrently. Select their sessions under the host and directory in the sidebar to read progress and send follow-ups. Choose the same provider for both tasks or a different provider for each.

Separate checkouts keep file edits apart. Sessions in the same directory share files and are not isolated. For two alternative implementations of the same feature, use separate checkouts, give both agents the same task, and compare their results before choosing one.

## Review each change

Select a session and open **Changes** to inspect its directory's diff. Open **Files** to browse and edit the source. Use split panes to keep the agent conversation beside the code you're reviewing.

Send corrections to that session's agent. To get another opinion, create a new session in the same directory and ask it to review the changes without editing files. Sessions in the same directory share its files.

## Run and test the app

Run the project's test or development command through a configured project script or ask the agent to run it. Inspect the script's read-only output in Paseo. For manual interactive shell work, choose **New session → Launch → Terminal** to open a shell on the daemon host, or use your own external terminal.

Separate directories have their own files, but processes still share the machine's network ports. Assign a different dev-server port to each running copy. Configure [project scripts and services](/docs/worktrees#scripts-and-services) for repeatable commands, and run any dependency setup explicitly.

For a web project, open the service URL in an external browser. Ensure the URL is reachable from that browser's machine; `localhost` on another device is not the daemon host.

Automated browser checks use your project's separately installed test tools or an explicitly configured external MCP service. Paseo does not embed a browser or inject its former browser-automation tools. Give any external tool concrete verification steps and review its actual results rather than treating an unexecuted test plan as success.

## Choose what to merge

Review each directory's diff and actual test results. Ask for remaining changes before committing or opening a pull request, using the agent, native Git actions or your usual external host terminal.

For independent tasks, review and merge each branch. For competing implementations, choose the version you want to keep. Archive a session when you no longer need it. Archiving preserves the directory and its files; manage checkout cleanup yourself after saving any work you need.

## Let an agent coordinate the tasks

When you want an agent to launch workers in existing directories and collect their results, use [orchestration](/docs/orchestration). The workers appear in Paseo so you can inspect their conversations and changes as they work.
