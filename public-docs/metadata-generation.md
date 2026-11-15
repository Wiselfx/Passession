---
title: Metadata generation
description: How Paseo uses providers to generate commit messages and pull request text, and how to configure them.
nav: Metadata generation
order: 42
category: Configuration
---

# Metadata generation

Paseo asks a language model to write short pieces of text for you so you don't have to. This is separate from the agent you're talking to: it's a small, one-shot call made in the background.

Paseo can draft Git metadata for your existing checkout:

- **Commit messages** — a concise message for the changes you're committing.
- **Pull request title and body** — drafted from the diff when you open a PR.

## How a model is chosen

Paseo has two modes. **Automatic** is the default and needs no configuration. **Manual** uses only the models you choose.

In Automatic mode, Paseo builds an ordered list of candidates and tries each one until a generation succeeds, so a slow or unavailable model falls through to the next:

1. **Built-in defaults**, matched against the models of the providers you have enabled:
   1. a `haiku` model
   2. `gpt-5.4-mini` (low reasoning)
   3. `minimax-m3`
   4. `nemotron-3-super`
2. **The model currently selected** for that agent or draft, as a last resort.

Each default is a match on the model id or name, so the first enabled provider that ships a matching model wins. Defaults with no match are skipped. Duplicates are removed, then generation tries the list top to bottom. The default order prefers small, fast, cheap models for these short tasks before falling back to whatever you have selected.

In Manual mode, Paseo tries only your configured models, in your order. It never adds the built-in defaults or the selected model, so your prompts and diffs only go to providers you chose. If every configured model fails, commits and pull requests get placeholder text. Pick a model that works for these tasks and review the generated text before submitting.

Every failure is written to `$PASEO_HOME/daemon.log` (`~/.paseo/daemon.log` by default) with the provider, the model and the error. Look there when a manual model doesn't produce commit messages or pull request text.

## Choose a model

Open **Settings → Host → Metadata** and select **Automatic** or **Manual**. In Manual mode, pick the model Paseo uses. Choosing a model replaces the first entry in `agents.metadataGeneration.providers` and preserves the rest of the list. Switching back to Automatic clears the list.

## Configure a custom fallback order

To use more than one model in Manual mode, list them in `agents.metadataGeneration.providers` in `~/.paseo/config.json`. Paseo tries your entries in order and nothing else. An empty list means Automatic.

```json
{
  "agents": {
    "metadataGeneration": {
      "providers": [
        { "provider": "claude", "model": "claude-haiku-4-5-20251001", "thinkingOptionId": "low" },
        { "provider": "opencode" }
      ]
    }
  }
}
```

Each entry accepts:

- `provider` (required) — the provider id. Built-in ids are `claude`, `codex`, `copilot`, `opencode`, and `pi`; custom providers use the id you gave them.
- `model` (optional) — a specific model id. Omit it to use that provider's default model.
- `thinkingOptionId` (optional) — a reasoning/thinking level for models that support one. Falls back to the model's default if the value isn't valid for that model.

The Settings screen replaces only the first entry and preserves the rest of a custom list. Restart the daemon after editing the file directly.

## Per-project instructions

You can steer Git metadata wording per repository with an on-disk `paseo.json` at the checkout root reported by `git rev-parse --show-toplevel`. Paseo does not read this configuration from a committed base-branch version. An externally created linked Git worktree uses its own checkout's file.

```json
{
  "metadataGeneration": {
    "commitMessage": { "instructions": "Follow Conventional Commits." },
    "pullRequest": { "instructions": "Include a Testing section in the body." }
  }
}
```

Each key is optional; only the ones you set are affected. Your instructions **replace** the default style for that metadata type — they are not appended to it — so your wording never competes with Paseo's defaults. The functional requirements (what to produce and the output format) always apply and cannot be overridden.
