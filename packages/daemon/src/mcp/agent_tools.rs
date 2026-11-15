use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

mod model_catalog;

const PROMPT_NOTIFICATION: &str = "You will get notified when the prompted agent finishes, errors, or needs permission. Do not poll for status; continue with other work until the notification arrives.";
const DISPATCH_ACKNOWLEDGED: &str = "Dispatch acknowledged. Observe completion using get_agent_status/get_agent_activity or SDK/RPC subscriptions and wait APIs.";

pub(crate) fn definitions(agent_scope: bool) -> Vec<Value> {
    let features = json!({"type":"object","additionalProperties":true,"description":"Provider-specific feature values, for example { fast_mode: true } for Codex."});
    let create_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Session mode to configure before the first run."},
        "thinkingOptionId":{"type":"string","description":"Thinking option ID."},
        "features":features
    },"additionalProperties":false,"description":"Initial runtime settings for the new agent."});
    let update_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Session mode ID."},
        "model":{"type":["string","null"],"description":"Model ID. Pass null to clear."},
        "thinkingOptionId":{"type":["string","null"],"description":"Thinking option ID. Pass null to clear."},
        "features":features
    },"additionalProperties":false,"description":"Runtime settings to apply to the agent."});
    let inspect_settings = json!({"type":"object","properties":{
        "modeId":{"type":"string","description":"Draft session mode ID."},
        "model":{"type":"string","description":"Draft model ID."},
        "thinkingOptionId":{"type":"string","description":"Draft thinking option ID."},
        "features":{"type":"object","additionalProperties":true,"description":"Draft provider feature values."}
    },"additionalProperties":false,"description":"Draft provider settings used to compute available features."});
    let id = json!({"agentId":{"type":"string"}});
    let empty = json!({});
    vec![
        definition(
            "create_agent",
            "Create agent",
            "Create an agent in an existing local directory. Agent-scoped calls inherit your directory when cwd is omitted and create your subagent. Top-level calls require cwd. Requires provider/model and an initial prompt; call list_providers and list_models first if uncertain.",
            json!({
                "title":{"type":"string","x-trim":true,"minLength":1,"maxLength":60,"description":"Short descriptive title (<= 60 chars) summarizing the agent's focus."},
                "provider":{"type":"string","x-trim":true,"description":"Required provider/model pair, for example codex/gpt-5.4."},
                "labels":{"type":"object","additionalProperties":{"type":"string"},"description":"Labels to set on the agent"},
                "settings":create_settings,
                "initialPrompt":{"type":"string","x-trim":true,"minLength":1,"description":"Required first task to run immediately after creation."},
                "cwd":{"type":"string","minLength":1,"description":"Existing local directory on the destination host. Defaults to the caller's directory for local agent-scoped calls; required for top-level or remote calls."},
                "notifyOnFinish":{"type":"boolean","default":agent_scope,"description":if agent_scope { "Get notified when the created agent finishes, errors, or needs permission. Set false for explicit reporting or no completion callback." } else { "Agent-scoped only: get notified when the created agent finishes, errors, or needs permission." }}
            }),
            if agent_scope {
                &["title", "provider", "initialPrompt"]
            } else {
                &["title", "provider", "initialPrompt", "cwd"]
            },
        ),
        definition(
            "send_agent_prompt",
            "Send agent prompt",
            "Send a task. Agent callers receive an immediate queued receipt; prompts are batched per recipient. Completion reports include the target’s last response by default. Paseo supplies caller identity.",
            json!({
                "agentId":{"type":"string"}, "prompt":{"type":"string"},
                "sessionMode":{"type":"string","description":"Optional mode to set before running the prompt."},
                "activeTurnBehavior":{"type":"string","enum":["interrupt","steer"],"description":"How to send into a running turn: \"steer\" joins it, falling back to interrupt when unavailable; \"interrupt\" replaces it. Defaults to steer for agent callers and interrupt otherwise."},
                "notifyOnFinish":{"type":"boolean","default":agent_scope,"description":if agent_scope { "Get notified when the prompted agent finishes, errors, or needs permission. Set false for explicit reporting or no completion callback." } else { "Agent-scoped only: get notified when the prompted agent finishes, errors, or needs permission." }}
            }),
            &["agentId", "prompt"],
        ),
        definition(
            "get_agent_status",
            "Get agent status",
            "Return compact lifecycle, error and permission status. Full snapshot fields and every permission remain available via detail JSON-pointer reads in bounded chunks.",
            crate::activity::model::status_properties(),
            &["agentId"],
        ),
        definition(
            "list_agents",
            "List agents",
            "List recent agents with identity, lifecycle, location and attention metadata. Null fields and detailed model/timing settings are omitted; retrieve the full snapshot through get_agent_status detail.",
            json!({
                "includeBackground":{"type":"boolean","default":false}, "includeArchived":{"type":"boolean","default":false},
                "cwd":{"type":"string"}, "sinceHours":{"type":"integer","minimum":1,"maximum":720,"default":48},
                "statuses":{"type":"array","items":{"type":"string","enum":["initializing","idle","running","error","closed"]}},
                "limit":{"type":"integer","minimum":1,"maximum":200,"default":50}
            }),
            &[],
        ),
        definition(
            "cancel_agent",
            "Cancel agent run",
            "Abort the agent's current run but keep the agent alive for future tasks.",
            id.clone(),
            &["agentId"],
        ),
        definition(
            "archive_agent",
            "Archive agent",
            "Archive an agent (soft-delete). The agent is interrupted if running and removed from the active list.",
            id.clone(),
            &["agentId"],
        ),
        definition(
            "kill_agent",
            "Kill agent",
            "Close an agent's runtime while retaining its record and history. The agent can be resumed; this does not delete or archive it.",
            id,
            &["agentId"],
        ),
        definition(
            "update_agent",
            "Update agent",
            "Update an agent name, labels, and/or runtime settings.",
            json!({
                "agentId":{"type":"string"},"name":{"type":"string"},
                "labels":{"type":"object","additionalProperties":{"type":"string"},"description":"Labels to set on the agent"},
                "settings":update_settings
            }),
            &["agentId"],
        ),
        definition(
            "list_providers",
            "List providers",
            "List configured agent providers, availability, and their modes.",
            empty.clone(),
            &[],
        ),
        definition(
            "list_models",
            "List models",
            "Discover selectable models by ID/label with query and pagination. Use modelId for exact full details, or details:true for detailed pages. Detailed rows replace thinkingOptions with a zero-based thinkingOptionsRef into response-local thinkingOptionSets; absent options stay absent. Continue with nextCursor and the same provider/query; changed catalogs require restarting.",
            model_catalog::properties(),
            &["provider"],
        ),
        definition(
            "list_profiles",
            "List agent profiles",
            "List agent profiles: named provider/model/mode bundles a human configured for specific kinds of work. Read each profile's `notes` to pick the one that fits the task you're delegating, then copy its `provider`, `model`, `modeId`, `thinkingOptionId`, and `featureValues` into create_agent (there is no `profile` parameter). Returns an empty list if none are configured.",
            empty.clone(),
            &[],
        ),
        definition(
            "inspect_provider",
            "Inspect provider",
            "Inspect compact provider capabilities for orchestration, including modes and draft feature settings. Use list_models to discover models and inspect exact model details.",
            json!({
                "provider":{"type":"string","x-trim":true,"minLength":1,"description":"Provider ID, optionally with a model ID (for example codex or codex/gpt-5.4)."},
                "cwd":{"type":"string","description":"Working directory used to resolve provider feature availability."},"settings":inspect_settings
            }),
            &["provider"],
        ),
        definition(
            "get_agent_activity",
            "Get agent activity",
            "Return a bounded recent canonical timeline page and compact agent status, without reasoning text. Follow nextCursor for older pages, or view=failures for bounded failure discovery. For exact item JSON, set detail to {epoch: coverage.epoch, seqStart: entry.seqStart, seqEnd: entry.seqEnd}, preserving page agentId/serverId. agentStatus.detail belongs to get_agent_status. Unrelated tail growth is allowed; changed requested rows or epochs explicitly invalidate continuations.",
            crate::activity::model::activity_properties(),
            &["agentId"],
        ),
        definition(
            "set_agent_mode",
            "Set agent session mode",
            "Switch the agent's session mode (plan, bypassPermissions, read-only, auto, etc.).",
            json!({"agentId":{"type":"string"},"modeId":{"type":"string"}}),
            &["agentId", "modeId"],
        ),
        definition(
            "list_pending_permissions",
            "List pending permissions",
            "Return all pending permission requests across all agents with the normalized payloads.",
            empty,
            &[],
        ),
        definition(
            "respond_to_permission",
            "Respond to permission",
            "Approve or deny a pending permission request with an AgentManager-compatible response payload.",
            json!({
                "agentId":{"type":"string"},"requestId":{"type":"string"},
                "response":{"oneOf":[
                    {"type":"object","x-strip-unknown":true,"properties":{"behavior":{"const":"allow"},"selectedActionId":{"type":"string"},"updatedInput":{"type":"object","additionalProperties":true},"updatedPermissions":{"type":"array","items":{"type":"object","additionalProperties":true}}},"required":["behavior"],"additionalProperties":false},
                    {"type":"object","x-strip-unknown":true,"properties":{"behavior":{"const":"deny"},"selectedActionId":{"type":"string"},"message":{"type":"string"},"interrupt":{"type":"boolean"}},"required":["behavior"],"additionalProperties":false}
                ]}
            }),
            &["agentId", "requestId", "response"],
        ),
    ]
}

fn definition(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    json!({"name":name,"title":title,"description":description,"inputSchema":{
        "type":"object","properties":properties,"required":required,"additionalProperties":true
    }})
}

fn result(structured: Value) -> Value {
    json!({"content":[],"structuredContent":structured,"isError":false})
}

fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn actor(caller: Option<&str>) -> Value {
    match caller {
        Some(id) => json!({"kind":"agent","agentId":id}),
        None => json!({"kind":"owner"}),
    }
}

fn provider_model(value: &str, required: bool) -> Result<(&str, Option<&str>)> {
    let value = value.trim();
    if let Some((provider, model)) = value.split_once('/') {
        let (provider, model) = (provider.trim(), model.trim());
        if !provider.is_empty() && !model.is_empty() {
            return Ok((provider, Some(model)));
        }
    } else if !required && !value.is_empty() {
        return Ok((value, None));
    }
    if required {
        bail!("provider must be provider/model, for example codex/gpt-5.4");
    }
    bail!("provider must be <provider> or <provider>/<model>")
}

pub(crate) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    mut input: Value,
    cancel: CancellationToken,
    _updates: Option<mpsc::Sender<Value>>,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("Tool execution cancelled");
    }
    let actor = actor(caller);
    let structured = match name {
        "create_agent" => {
            prepare_create(&mut input, caller)?;
            let input = resolve_create(app, caller, input).await?;
            let notifying = caller.is_some() && input["notifyOnFinish"] == true;
            let created = app.agents.create(&actor, input).await?;
            let snapshot = created
                .get("snapshot")
                .ok_or_else(|| anyhow!("Agent creation returned no snapshot"))?;
            let live = created.get("liveSnapshot").unwrap_or(snapshot);
            let guidance = if notifying && created["initialPromptStarted"] == true {
                "You will get notified when the created agent finishes, errors, or needs permission. Do not poll for status; continue with other work until the notification arrives."
            } else {
                "Creation acknowledged. Observe completion using get_agent_status/get_agent_activity or SDK/RPC subscriptions and wait APIs."
            };
            let response = json!({
                "agentId":live["id"], "type":snapshot["provider"],
                "status":live["status"], "cwd":live["cwd"],
                "currentModeId":live["currentModeId"], "availableModes":live["availableModes"],
                "guidance":guidance
            });
            response
        }
        "send_agent_prompt" => {
            let id = string(&input, "agentId")?.to_owned();
            let notifying = caller.is_some()
                && input
                    .get("notifyOnFinish")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
            // The source tool destructures these fields; MCP passthrough keys
            // must never become additional manager authority or run options.
            retain_fields(&mut input, &["prompt", "sessionMode", "activeTurnBehavior"]);
            input["notifyOnFinish"] = json!(notifying);
            if input.get("activeTurnBehavior").is_none() {
                input["activeTurnBehavior"] = json!(if caller.is_some() {
                    "steer"
                } else {
                    "interrupt"
                });
            }
            let receipt = app.agents.send(&actor, &id, input).await?;
            if caller.is_some() {
                json!({
                    "success":true, "deliveryStatus":"queued",
                    "messageId":receipt["messageId"], "status":receipt["status"],
                    "guidance":if notifying { format!("Queued for batched delivery (2s quiet window, 10s batching cap). {PROMPT_NOTIFICATION}") } else { "Queued for batched delivery (2s quiet window, 10s batching cap); provider dispatch has not occurred.".to_owned() }
                })
            } else {
                let snapshot = app.agents.observe_agent_snapshot(&id).await?;
                let status = snapshot
                    .as_ref()
                    .and_then(|value| value.get("status"))
                    .cloned()
                    .unwrap_or(json!("idle"));
                json!({"success":true,"disposition":receipt["disposition"],"status":status,"guidance":DISPATCH_ACKNOWLEDGED})
            }
        }
        "get_agent_status" => {
            let id = string(&input, "agentId")?;
            let snapshot = app
                .agents
                .observe_agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Agent {id} not found"))?;
            crate::activity::model::status(&snapshot, &input)?
        }
        "list_agents" => {
            retain_fields(
                &mut input,
                &[
                    "includeBackground",
                    "includeArchived",
                    "cwd",
                    "sinceHours",
                    "statuses",
                    "limit",
                ],
            );
            crate::activity::model::agent_list(app.agents.list_agents(&actor, input).await?)
        }
        "archive_agent" | "kill_agent" => {
            let id = string(&input, "agentId")?;
            app.agents
                .control(
                    &actor,
                    id,
                    if name == "kill_agent" {
                        "close"
                    } else {
                        "archive"
                    },
                    json!({}),
                )
                .await?;
            json!({"success":true})
        }
        "update_agent" => {
            let id = string(&input, "agentId")?;
            if let Some(settings) = input.get("settings") {
                for (key, action) in [
                    ("modeId", "set_mode"),
                    ("model", "set_model"),
                    ("thinkingOptionId", "set_thinking"),
                ] {
                    if let Some(value) = settings.get(key) {
                        let mut update = serde_json::Map::new();
                        update.insert(key.to_owned(), value.clone());
                        app.agents
                            .control(&actor, id, action, Value::Object(update))
                            .await?;
                    }
                }
                if let Some(features) = settings.get("features").and_then(Value::as_object) {
                    for (feature, value) in features {
                        app.agents
                            .control(
                                &actor,
                                id,
                                "set_feature",
                                json!({"featureId":feature,"value":value}),
                            )
                            .await?;
                    }
                }
            }
            let mut update = serde_json::Map::new();
            if let Some(title) = input
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                update.insert("name".to_owned(), json!(title));
            }
            if let Some(labels) = input
                .get("labels")
                .and_then(Value::as_object)
                .filter(|value| !value.is_empty())
            {
                update.insert("labels".to_owned(), Value::Object(labels.clone()));
            }
            if !update.is_empty() {
                app.agents
                    .control(&actor, id, "update", Value::Object(update))
                    .await?;
            }
            json!({"success":true})
        }
        "list_profiles" => {
            let mut config = app.config.mutable_snapshot();
            json!({"profiles":config.get_mut("agentProfiles").map(Value::take).unwrap_or_else(|| json!([]))})
        }
        "get_agent_activity" => {
            let id = string(&input, "agentId")?;
            let options = crate::activity::model::timeline_options(&input)?;
            let page = app.agents.observe_timeline(id, &options).await?;
            let snapshot = app
                .agents
                .observe_agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Agent {id} not found"))?;
            crate::activity::model::activity(&page, &snapshot, &input)?
        }
        "set_agent_mode" => {
            let id = string(&input, "agentId")?;
            let mode = string(&input, "modeId")?;
            app.agents
                .control(&actor, id, "set_mode", json!({"modeId":mode}))
                .await?;
            json!({"success":true,"newMode":mode})
        }
        "respond_to_permission" => {
            let id = string(&input, "agentId")?;
            app.agents
                .control(
                    &actor,
                    id,
                    "respond_permission",
                    json!({"requestId":string(&input,"requestId")?,"response":input["response"]}),
                )
                .await?;
            json!({"success":true})
        }
        _ => return execute_catalog_or_permissions(app, &actor, caller, name, input, cancel).await,
    };
    Ok(result(structured))
}

fn prepare_create(input: &mut Value, caller: Option<&str>) -> Result<()> {
    strict_keys(
        input,
        &[
            "title",
            "provider",
            "labels",
            "settings",
            "initialPrompt",
            "notifyOnFinish",
            "cwd",
        ],
    )?;
    if input
        .get("cwd")
        .is_some_and(|value| value.as_str().is_none_or(|cwd| cwd.trim().is_empty()))
    {
        bail!("cwd must be a nonempty string");
    }
    if caller.is_none() && input.get("cwd").is_none() {
        bail!("cwd is required outside an agent-scoped session");
    }
    for key in ["title", "provider", "initialPrompt"] {
        let trimmed = string(input, key)?.trim().to_owned();
        if trimmed.is_empty() {
            bail!("{key} is required");
        }
        if key == "title" && trimmed.encode_utf16().count() > 60 {
            bail!("Title must be 60 characters or fewer");
        }
        input[key] = json!(trimmed);
    }
    provider_model(string(input, "provider")?, true)?;
    let notifying = caller.is_some()
        && input
            .get("notifyOnFinish")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    input["notifyOnFinish"] = json!(notifying);
    input["kind"] = json!("mcp");
    input["promptFailure"] = json!("throw");
    Ok(())
}

async fn execute_catalog_or_permissions(
    app: &Arc<AppState>,
    actor: &Value,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let structured = match name {
        "cancel_agent" => {
            let id = string(&input, "agentId")?;
            let outcome = app.agents.control(actor, id, "cancel", json!({})).await?;
            match outcome.get("status").and_then(Value::as_str) {
                Some("settled") => json!({"success":true}),
                Some("not_running") => json!({"success":false}),
                Some("refused") => bail!(
                    "Cannot stop agent {id} because its active run cancellation was not acknowledged"
                ),
                Some(status) => bail!("Agent cancellation did not settle: {status}"),
                None => bail!("Agent cancellation returned no outcome"),
            }
        }
        "list_pending_permissions" => {
            json!({"permissions":app.agents.pending_permissions().await?})
        }
        _ => return execute_provider(app, caller, name, input, cancel).await,
    };
    Ok(result(structured))
}

async fn resolve_create(
    app: &Arc<AppState>,
    caller: Option<&str>,
    mut input: Value,
) -> Result<Value> {
    let parent = match caller {
        Some(id) => Some(
            app.agents
                .agent_snapshot(id)
                .await?
                .ok_or_else(|| anyhow!("Parent agent {id} not found"))?,
        ),
        None => None,
    };
    let cwd = super::workspace_tools::resolve_scoped_cwd(
        app,
        caller,
        input.get("cwd").and_then(Value::as_str),
    )
    .await?;
    let selected_provider = provider_model(string(&input, "provider")?, true)?.0;
    let inherited = parent
        .as_ref()
        .filter(|value| value.get("provider").and_then(Value::as_str) == Some(selected_provider))
        .and_then(|value| value.pointer("/config/providerOptions"))
        .cloned();
    let object = input
        .as_object_mut()
        .ok_or_else(|| anyhow!("Tool input must be an object"))?;
    object.insert("cwd".to_owned(), json!(cwd));
    if input_directory_inherited(&parent, &object["cwd"])
        && let Some(id) = parent.as_ref().and_then(|parent| parent.get("workspaceId"))
    {
        object.insert("workspaceId".to_owned(), id.clone());
    }
    if caller.is_some() {
        let identity = app.transport.identity();
        let server_id = string(&identity, "serverId")?;
        let labels = object.entry("labels").or_insert_with(|| json!({}));
        labels
            .as_object_mut()
            .ok_or_else(|| anyhow!("labels must be an object"))?
            .insert("paseo.parent-server-id".to_owned(), json!(server_id));
    }
    if let Some(options) = inherited {
        object.insert("config".to_owned(), json!({"providerOptions":options}));
    }
    if let Some(settings) = object.remove("settings") {
        for (key, target) in [
            ("modeId", "mode"),
            ("thinkingOptionId", "thinking"),
            ("features", "features"),
        ] {
            if !object.contains_key(target)
                && let Some(value) = settings.get(key)
            {
                object.insert(target.to_owned(), value.clone());
            }
        }
    }
    Ok(input)
}

fn input_directory_inherited(parent: &Option<Value>, cwd: &Value) -> bool {
    parent.as_ref().is_some_and(|parent| parent["cwd"] == *cwd)
}

fn strict_keys(value: &Value, allowed: &[&str]) -> Result<()> {
    for key in value
        .as_object()
        .ok_or_else(|| anyhow!("Expected an object"))?
        .keys()
    {
        if !allowed.contains(&key.as_str()) {
            bail!("Unrecognized argument: {key}");
        }
    }
    Ok(())
}

async fn execute_provider(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let structured = match name {
        "list_providers" => {
            let entries = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entries = app.providers.list(None, true) => entries?,
            };
            let providers = entries
                .iter()
                .map(provider_summary)
                .collect::<Result<Vec<_>>>()?;
            json!({"providers":providers})
        }
        "list_models" => {
            let request = model_catalog::Request::parse(&input)?;
            let provider = request.provider;
            let entry = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entry = app.providers.catalog(provider, None, true) => entry?,
            };
            if entry.get("enabled").and_then(Value::as_bool) != Some(true) {
                bail!("Provider '{provider}' is disabled");
            }
            match entry.get("status").and_then(Value::as_str) {
                Some("ready") => {}
                Some("error") => bail!(
                    "{}",
                    entry
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Failed to load provider '{provider}'"))
                ),
                _ => bail!("Provider '{provider}' is not available"),
            }
            request.project(&entry, &cancel)?
        }
        "inspect_provider" => {
            let (provider, model) = provider_model(string(&input, "provider")?, false)?;
            let cwd = super::workspace_tools::resolve_scoped_cwd(
                app,
                caller,
                input.get("cwd").and_then(Value::as_str),
            )
            .await?;
            let entry = tokio::select! {
                _ = cancel.cancelled() => bail!("Tool execution cancelled"),
                entry = app.providers.catalog(provider, Some(&cwd), true) => entry?,
            };
            if entry.get("enabled").and_then(Value::as_bool) != Some(true) {
                bail!("Provider '{provider}' is disabled");
            }
            if entry.get("status").and_then(Value::as_str) != Some("ready") {
                bail!(
                    "{}",
                    entry
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Provider '{provider}' is unavailable"))
                );
            }
            let settings = input.get("settings");
            let selected_model = settings
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .or(model);
            let mut config = json!({"provider":provider,"cwd":cwd});
            if let Some(model) = selected_model.filter(|value| !value.is_empty()) {
                config["model"] = json!(model);
            }
            if let Some(settings) = settings {
                for (key, target) in [
                    ("modeId", "modeId"),
                    ("thinkingOptionId", "thinkingOptionId"),
                    ("features", "featureValues"),
                ] {
                    if let Some(value) = settings
                        .get(key)
                        .filter(|value| value.as_str().is_none_or(|value| !value.is_empty()))
                    {
                        config[target] = value.clone();
                    }
                }
            }
            let features = app.agents.list_draft_features(&config).await?;
            let summary = provider_summary(&entry)?;
            json!({
                "provider":provider, "label":summary["label"], "description":summary["description"],
                "enabled":summary["enabled"], "status":summary["status"], "modes":summary["modes"],
                "selectedModel":selected_model, "features":features
            })
        }
        _ => bail!("Unknown agent tool: {name}"),
    };
    Ok(result(structured))
}

fn provider_summary(entry: &Value) -> Result<Value> {
    let provider = string(entry, "provider")?;
    let status = string(entry, "status")?;
    let mut summary = json!({
        "id":provider,
        "label":entry.get("label").filter(|value| !value.is_null()).unwrap_or(&entry["provider"]),
        "description":entry.get("description").and_then(Value::as_str).unwrap_or(""),
        "enabled":entry["enabled"],
        "modes":entry.get("modes").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!([])),
        "status":if status == "ready" { "available" } else { status }
    });
    if let Some(error) = entry
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        summary["error"] = json!(error);
    }
    Ok(summary)
}

fn retain_fields(input: &mut Value, fields: &[&str]) {
    if let Some(object) = input.as_object_mut() {
        object.retain(|key, _| fields.contains(&key.as_str()));
    }
}
#[cfg(test)]
mod creation_tests {
    use super::*;

    fn input() -> Value {
        json!({"title":"Task","provider":"codex/model","initialPrompt":"Do the task"})
    }

    #[test]
    fn creation_uses_directory_or_inherits_calling_session() {
        assert!(prepare_create(&mut input(), Some("parent")).is_ok());
        assert!(prepare_create(&mut input(), None).is_err());
        let mut local = input();
        local["cwd"] = json!("/existing");
        prepare_create(&mut local, None).unwrap();
        assert_eq!(local["cwd"], "/existing");
        for key in ["workspaceId", "workspace", "relationship", "worktree"] {
            let mut retired = input();
            retired[key] = json!("old");
            assert!(prepare_create(&mut retired, Some("parent")).is_err());
        }
    }

    #[test]
    fn directory_inheritance_never_rehomes_an_explicit_directory() {
        let parent = Some(json!({"cwd":"/parent","workspaceId":"owner"}));
        assert!(input_directory_inherited(&parent, &json!("/parent")));
        assert!(!input_directory_inherited(&parent, &json!("/other")));
    }
}
