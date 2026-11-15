use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::{state::AppState, terminals::CreateTerminal};

pub(crate) fn definitions(agent_scope: bool) -> Vec<Value> {
    let string = || json!({"type":"string"});
    let boolean = || json!({"type":"boolean"});
    let definition = |name: &str,
                      title: &str,
                      description: &str,
                      properties: Value,
                      required: &[&str]| {
        json!({"name":name,"title":title,"description":description,"inputSchema":{
            "type":"object","properties":properties,"required":required,"additionalProperties":true
        }})
    };
    vec![
        definition(
            "list_scripts",
            "List scripts",
            "List configured scripts for an agent's directory, including lifecycle, service port, proxy URL, health, and terminal ID.",
            json!({"agentId":{"type":"string","description":"Agent whose directory owns the scripts. Defaults to the calling agent."}}),
            if agent_scope { &[] } else { &["agentId"] },
        ),
        definition(
            "start_script",
            "Start script",
            "Start a configured paseo.json script in an agent's directory.",
            json!({"agentId":{"type":"string"},"scriptName":{"type":"string","minLength":1}}),
            if agent_scope {
                &["scriptName"]
            } else {
                &["agentId", "scriptName"]
            },
        ),
        definition(
            "stop_script",
            "Stop script",
            "Stop a configured script in an agent's directory.",
            json!({"agentId":{"type":"string"},"scriptName":{"type":"string","minLength":1}}),
            if agent_scope {
                &["scriptName"]
            } else {
                &["agentId", "scriptName"]
            },
        ),
        definition(
            "list_terminals",
            "List terminals",
            "List terminals for a working directory or across all working directories.",
            json!({"cwd":{"type":"string","description":"Optional working directory. Defaults to your current working directory."},"all":{"type":"boolean","description":"List terminals across all working directories."}}),
            &[],
        ),
        definition(
            "create_terminal",
            "Create terminal",
            "Create a terminal session for a working directory.",
            json!({"cwd":{"type":"string","description":"Optional working directory. Defaults to your current working directory."},"name":{"type":"string","description":"Optional terminal name."}}),
            &[],
        ),
        definition(
            "kill_terminal",
            "Kill terminal",
            "Kill an existing terminal session.",
            json!({"terminalId":string()}),
            &["terminalId"],
        ),
        definition(
            "capture_terminal",
            "Capture terminal",
            "Capture terminal output. Omit query for unchanged, unfiltered lines. An explicit query selects case-insensitive literal matches (including newline-separated physical rows), with bounded context. This is a filtered snapshot, not a complete error audit; row indices can shift or reset after scrolling, resize, or terminal reset.",
            json!({
                "terminalId":string(),"start":{"type":"number"},"end":{"type":"number"},
                "scrollback":boolean(),"stripAnsi":{"type":"boolean","default":true},
                "query":{"type":"string","minLength":1,"description":"Case-insensitive literal text, not a regex. Unicode lowercasing is used; whitespace is significant. Matching is restricted to the captured start/end window."},
                "contextLines":{"type":"integer","minimum":0,"maximum":20,"description":"Context rows on each side of selected matches, inside the captured window. Defaults to 2. Requires query."},
                "matchLimit":{"type":"integer","minimum":1,"maximum":100,"description":"Maximum selected matching start rows. Defaults to 20. Multiple occurrences on one row count once; context may contain additional matches. Requires query."}
            }),
            &["terminalId"],
        ),
        definition(
            "send_terminal_keys",
            "Send terminal keys",
            "Send text or a key token: Enter, Tab, Escape, Space, BSpace, Up, Down, Left, Right, Home, End, Insert, Delete, PageUp, PageDown, or F1-F12. Combine C- (Ctrl), M- (Alt), and S- (Shift) prefixes, e.g. C-r, S-Tab, or C-M-Left. Set literal=true to send the text unchanged.",
            json!({"terminalId":string(),"keys":string(),"literal":boolean()}),
            &["terminalId", "keys"],
        ),
    ]
}

#[cfg(test)]
mod tool_contract_tests {
    use super::*;

    #[test]
    fn tool_catalog_exposes_session_scripts_without_workspace_lifecycle() {
        let tools = definitions(true);
        for retired in [
            "create_workspace",
            "list_workspaces",
            "rename_workspace",
            "archive_workspace",
            "list_workspace_scripts",
            "start_workspace_script",
            "stop_workspace_script",
        ] {
            assert!(!tools.iter().any(|tool| tool["name"] == retired));
        }
        for name in ["list_scripts", "start_script", "stop_script"] {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            let properties = &tool["inputSchema"]["properties"];
            assert!(properties.get("agentId").is_some());
            assert!(properties.get("workspaceId").is_none());
        }
    }
}

pub(crate) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
    cancel: CancellationToken,
    _updates: Option<Sender<Value>>,
) -> Result<Value> {
    // Once admitted, do not drop a mutating terminal operation on cancellation.
    if cancel.is_cancelled() {
        bail!("Tool call cancelled before execution");
    }
    let result = match name {
        "list_scripts" | "start_script" | "stop_script" => {
            if input.get("workspaceId").is_some() {
                return Err(super::input_error(
                    "Use agentId to select the script directory",
                ));
            }
            let id = input
                .get("agentId")
                .and_then(Value::as_str)
                .or(caller)
                .ok_or_else(|| {
                    super::input_error("agentId is required outside an agent-scoped session")
                })?;
            let agent = caller_agent(app, id).await?;
            let owner = required(&agent, "workspaceId")?;
            match name {
                "list_scripts" => json!({"scripts":app.terminals.script_list(owner).await?}),
                "start_script" => {
                    json!({"script":app.terminals.script_start(owner, required(&input,"scriptName")?).await?})
                }
                _ => {
                    json!({"script":app.terminals.script_stop(owner, required(&input,"scriptName")?).await?})
                }
            }
        }
        "list_terminals" => {
            let cwd = if flag(&input, "all", false) {
                None
            } else {
                Some(
                    resolve_scoped_cwd(app, caller, input.get("cwd").and_then(Value::as_str))
                        .await?,
                )
            };
            let cwd = cwd.as_ref().map(|path| path.to_string_lossy());
            let terminals = app
                .terminals
                .list(None, cwd.as_deref())
                .await
                .into_iter()
                .map(|terminal| terminal_summary(&terminal))
                .collect::<Vec<_>>();
            json!({"terminals":terminals})
        }
        "create_terminal" => {
            let cwd =
                resolve_scoped_cwd(app, caller, input.get("cwd").and_then(Value::as_str)).await?;
            let workspace_id = if let Some(caller) = caller {
                let agent = caller_agent(app, caller).await?;
                if agent["cwd"].as_str() == cwd.to_str() {
                    agent["workspaceId"].as_str().map(str::to_owned)
                } else {
                    None
                }
            } else {
                None
            };
            let workspace_id = match workspace_id {
                Some(id) => id,
                None => {
                    let workspace = app
                        .projects
                        .ensure_workspace_for_create(&cwd, &Value::Null, None)
                        .await?;
                    required(&workspace, "workspaceId")?.to_owned()
                }
            };
            let terminal = app
                .terminals
                .create(CreateTerminal {
                    cwd,
                    workspace_id,
                    name: trimmed(&input, "name").map(str::to_owned),
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    rows: 24,
                    cols: 80,
                })
                .await?;
            terminal_summary(&terminal.info())
        }
        "kill_terminal" => {
            let id = required(&input, "terminalId")?;
            existing_terminal(app, id).await?;
            app.terminals.kill(id).await?;
            json!({"success":true})
        }
        "capture_terminal" => {
            let id = required(&input, "terminalId")?;
            let filter = TerminalFilter::parse(&input)?;
            let terminal = existing_terminal(app, id).await?;
            let start = if flag(&input, "scrollback", false) {
                Some(0.0)
            } else {
                input.get("start").and_then(Value::as_f64)
            };
            let end = input.get("end").and_then(Value::as_f64);
            let (lines, total_lines) = terminal
                .capture(start, end, flag(&input, "stripAnsi", true))
                .await?;
            match filter {
                Some(filter) => filter.capture(id, lines, total_lines, start),
                None => json!({"terminalId":id,"lines":lines,"totalLines":total_lines}),
            }
        }
        "send_terminal_keys" => {
            let terminal = existing_terminal(app, required(&input, "terminalId")?).await?;
            let keys = required(&input, "keys")?;
            if flag(&input, "literal", false) {
                terminal.input(keys.as_bytes()).await?;
            } else {
                let preamble = terminal.replay_preamble().await?;
                let encoded = encode_key(keys, &preamble);
                terminal.input(encoded.as_bytes()).await?;
            }
            json!({"success":true})
        }
        _ => bail!("Unknown workspace tool: {name}"),
    };
    Ok(json!({"content":[],"structuredContent":result,"isError":false}))
}

struct TerminalFilter<'a> {
    query: &'a str,
    context_lines: usize,
    match_limit: usize,
}

impl<'a> TerminalFilter<'a> {
    fn parse(input: &'a Value) -> Result<Option<Self>> {
        let Some(query) = input.get("query") else {
            if input.get("contextLines").is_some() || input.get("matchLimit").is_some() {
                return Err(super::input_error(
                    "contextLines and matchLimit require query",
                ));
            }
            return Ok(None);
        };
        let query = query
            .as_str()
            .filter(|query| !query.is_empty())
            .ok_or_else(|| super::input_error("query must be a nonempty string"))?;
        let bounded = |key: &str, default: usize, min: usize, max: usize| -> Result<usize> {
            let Some(value) = input.get(key) else {
                return Ok(default);
            };
            let number = value.as_f64().filter(|number| {
                number.fract() == 0.0 && *number >= min as f64 && *number <= max as f64
            });
            number.map(|number| number as usize).ok_or_else(|| {
                super::input_error(format!("{key} must be an integer between {min} and {max}"))
            })
        };
        Ok(Some(Self {
            query,
            context_lines: bounded("contextLines", 2, 0, 20)?,
            match_limit: bounded("matchLimit", 20, 1, 100)?,
        }))
    }

    fn capture(
        &self,
        terminal_id: &str,
        lines: Vec<String>,
        total_lines: usize,
        start: Option<f64>,
    ) -> Value {
        // Screen::capture resolves negatives and clamps before truncating the
        // fractional index. Derive the origin from that same snapshot's total,
        // never from a second capture which could observe scrolling or resize.
        let start = start.unwrap_or(0.0);
        let start = if start < 0.0 {
            total_lines as f64 + start
        } else {
            start
        };
        let origin = start.clamp(0.0, total_lines.saturating_sub(1) as f64) as usize;
        let inspected_range = if lines.is_empty() {
            Value::Null
        } else {
            json!({"start":origin,"end":origin + lines.len() - 1})
        };
        let query = self.query.to_lowercase();
        let parts: Vec<_> = query.split('\n').collect();
        let folded: Vec<_> = lines.iter().map(|line| line.to_lowercase()).collect();
        let mut matches = Vec::new();
        let mut matched = vec![false; lines.len()];
        for (row, window) in folded.windows(parts.len()).enumerate() {
            let is_match = if parts.len() == 1 {
                window[0].contains(parts[0])
            } else {
                window[0].ends_with(parts[0])
                    && window[parts.len() - 1].starts_with(parts[parts.len() - 1])
                    && window[1..parts.len() - 1]
                        .iter()
                        .zip(&parts[1..parts.len() - 1])
                        .all(|(line, part)| line.as_str() == *part)
            };
            if is_match {
                matches.push(row);
                matched[row..row + parts.len()].fill(true);
            }
        }
        let total_matches = matches.len();
        let returned_matches = total_matches.min(self.match_limit);
        let truncated = total_matches > returned_matches;
        let next_range = matches
            .get(self.match_limit)
            .map(|row| json!({"start":origin + row,"end":origin + lines.len() - 1}));
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for row in matches.into_iter().take(self.match_limit) {
            let start = row.saturating_sub(self.context_lines);
            let end = (row + parts.len() + self.context_lines).min(lines.len());
            if let Some(previous) = ranges.last_mut()
                && start <= previous.end
            {
                previous.end = previous.end.max(end);
            } else {
                ranges.push(start..end);
            }
        }
        let mut ranges = ranges.into_iter().peekable();
        let selected: Vec<_> = lines
            .into_iter()
            .enumerate()
            .filter_map(|(row, text)| {
                while ranges.peek().is_some_and(|range| row >= range.end) {
                    ranges.next();
                }
                ranges
                    .peek()
                    .filter(|range| range.contains(&row))
                    .map(|_| json!({"index":origin + row,"text":text,"matched":matched[row]}))
            })
            .collect();
        json!({
            "terminalId":terminal_id,"lines":selected,"totalLines":total_lines,
            "query":self.query,"contextLines":self.context_lines,"matchLimit":self.match_limit,
            "inspectedRange":inspected_range,"totalMatches":total_matches,
            "returnedMatches":returned_matches,"noMatches":total_matches == 0,
            "truncated":truncated,"nextRange":next_range,
            "indexScope":"snapshot",
            "notice":"Filtered snapshot, not a complete error audit. Indices address physical rows in this snapshot only and may shift or reset after scrolling, resize, or terminal reset. nextRange is an inclusive range for another capture with the same query and scrollback omitted (scrollback=true overrides start); it is not a stable cursor."
        })
    }
}

async fn existing_terminal(
    app: &Arc<AppState>,
    id: &str,
) -> Result<Arc<crate::terminals::Terminal>> {
    app.terminals
        .get(id)
        .await
        .map_err(|_| super::input_error(format!("Terminal {id} not found")))
}

pub(crate) async fn resolve_scoped_cwd(
    app: &Arc<AppState>,
    caller: Option<&str>,
    requested: Option<&str>,
) -> Result<PathBuf> {
    if let Some(id) = caller {
        let agent = caller_agent(app, id).await?;
        let parent = Path::new(required(&agent, "cwd")?);
        let requested = requested.map(str::trim).filter(|value| !value.is_empty());
        if requested.is_none() {
            return Ok(parent.to_path_buf());
        }
        return expand_path(app, requested.unwrap(), Some(parent));
    }
    let requested = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| super::input_error("cwd is required"))?;
    expand_path(app, requested, None)
}

pub(super) fn expand_path(app: &AppState, value: &str, base: Option<&Path>) -> Result<PathBuf> {
    let expanded = if value == "~" {
        app.user_home.clone()
    } else if let Some(suffix) = value.strip_prefix("~/") {
        app.user_home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        match base {
            Some(base) => base.join(expanded),
            None => std::env::current_dir()?.join(expanded),
        }
    };
    // Match path.resolve, not canonicalize: do not impose existence or symlink resolution.
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

async fn caller_agent(app: &Arc<AppState>, id: &str) -> Result<Value> {
    app.agents
        .agent_snapshot(id)
        .await?
        .ok_or_else(|| super::input_error(format!("Parent agent {id} not found")))
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| super::input_error(format!("{key} is required")))
}

fn trimmed<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn flag(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn terminal_summary(terminal: &Value) -> Value {
    json!({"id":terminal["id"],"name":terminal["name"],"cwd":terminal["cwd"]})
}

fn encode_key<'a>(original: &'a str, preamble: &str) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    // JavaScript's source token regex is not dot-all: multiline text is literal.
    if original.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return Cow::Borrowed(original);
    }
    let mut token = original;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    loop {
        if let Some(rest) = token.strip_prefix("C-") {
            ctrl = true;
            token = rest;
        } else if let Some(rest) = token.strip_prefix("M-") {
            alt = true;
            token = rest;
        } else if let Some(rest) = token.strip_prefix("S-") {
            shift = true;
            token = rest;
        } else {
            break;
        }
    }
    if token.is_empty() {
        return Cow::Borrowed(original);
    }
    let token = match token {
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "BSpace" => "Backspace",
        "Space" => " ",
        other => other,
    };
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let prefix = |value: &str| {
        if alt {
            format!("\x1b{value}")
        } else {
            value.to_owned()
        }
    };
    let csi = |final_byte: char| {
        if modifier == 1 {
            format!("\x1b[{final_byte}")
        } else {
            format!("\x1b[1;{modifier}{final_byte}")
        }
    };
    let tilde = |number: u8| {
        if modifier == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{modifier}~")
        }
    };
    if token
        .chars()
        .next()
        .is_some_and(|character| character.len_utf16() == 1 && character.len_utf8() == token.len())
    {
        if !ctrl && !alt && !shift {
            return Cow::Borrowed(token);
        }
        let character = if shift {
            token.to_uppercase()
        } else {
            token.to_owned()
        };
        let encoded = if ctrl {
            let upper = character.to_uppercase();
            let control = if upper.len() == 1 && upper.as_bytes()[0].is_ascii_uppercase() {
                Some(upper.as_bytes()[0] - 64)
            } else {
                match character.as_str() {
                    " " | "@" | "2" => Some(0),
                    "[" | "3" => Some(27),
                    "\\" | "4" => Some(28),
                    "]" | "5" => Some(29),
                    "^" | "6" => Some(30),
                    "_" | "/" | "7" => Some(31),
                    "8" | "?" => Some(127),
                    _ if character.encode_utf16().count() == 1 => {
                        Some((character.encode_utf16().next().unwrap() & 31) as u8)
                    }
                    _ => None,
                }
            };
            match control {
                Some(code) => prefix(&char::from(code).to_string()),
                None => prefix(&character),
            }
        } else {
            prefix(&character)
        };
        return Cow::Owned(encoded);
    }
    let encoded = match token {
        "Enter" if modifier > 1 && preamble.contains("\x1b[?9001h") => {
            let state = 16 * u8::from(shift) + 2 * u8::from(alt) + 8 * u8::from(ctrl);
            format!("\x1b[13;28;13;1;{state};1_")
        }
        "Enter"
            if modifier > 1
                && preamble.split("\x1b[=").skip(1).any(|part| {
                    part.split(';')
                        .next()
                        .and_then(|number| number.parse::<u64>().ok())
                        .is_some_and(|number| number > 0)
                }) =>
        {
            format!("\x1b[13;{modifier}u")
        }
        "Enter" => "\r".to_owned(),
        "Tab" if shift && !ctrl && !alt => "\x1b[Z".to_owned(),
        "Tab" => prefix("\t"),
        "Backspace" => prefix("\x7f"),
        "Escape" => "\x1b".to_owned(),
        "ArrowUp" | "ArrowDown" | "ArrowRight" | "ArrowLeft" => {
            let final_byte = match token {
                "ArrowUp" => 'A',
                "ArrowDown" => 'B',
                "ArrowRight" => 'C',
                _ => 'D',
            };
            if modifier == 1 && preamble.contains("\x1b[?1h") {
                format!("\x1bO{final_byte}")
            } else {
                csi(final_byte)
            }
        }
        "Home" => csi('H'),
        "End" => csi('F'),
        "Insert" => tilde(2),
        "Delete" => tilde(3),
        "PageUp" => tilde(5),
        "PageDown" => tilde(6),
        "F1" | "F2" | "F3" | "F4" => {
            let final_byte = match token {
                "F1" => 'P',
                "F2" => 'Q',
                "F3" => 'R',
                _ => 'S',
            };
            if modifier == 1 {
                format!("\x1bO{final_byte}")
            } else {
                csi(final_byte)
            }
        }
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => return Cow::Borrowed(original),
    };
    Cow::Owned(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(input: Value, lines: &[&str], total: usize, start: Option<f64>) -> Value {
        TerminalFilter::parse(&input).unwrap().unwrap().capture(
            "terminal",
            lines.iter().map(|line| (*line).into()).collect(),
            total,
            start,
        )
    }

    #[test]
    fn capture_filter_preserves_unfiltered_requests_and_validates_inputs() {
        assert!(
            TerminalFilter::parse(&json!({
                "terminalId":"terminal","start":-2.5,"end":3.9,"stripAnsi":false
            }))
            .unwrap()
            .is_none()
        );
        let defaults = json!({"query":" "});
        let defaults = TerminalFilter::parse(&defaults).unwrap().unwrap();
        assert_eq!(defaults.query, " ");
        assert_eq!(defaults.context_lines, 2);
        assert_eq!(defaults.match_limit, 20);
        for input in [
            json!({"query":""}),
            json!({"query":null}),
            json!({"query":12}),
            json!({"contextLines":1}),
            json!({"matchLimit":1}),
            json!({"query":"x","contextLines":-1}),
            json!({"query":"x","contextLines":21}),
            json!({"query":"x","contextLines":1.5}),
            json!({"query":"x","contextLines":"2"}),
            json!({"query":"x","contextLines":null}),
            json!({"query":"x","matchLimit":0}),
            json!({"query":"x","matchLimit":101}),
            json!({"query":"x","matchLimit":1.5}),
            json!({"query":"x","matchLimit":true}),
        ] {
            let error = TerminalFilter::parse(&input).err().unwrap();
            assert!(
                error
                    .downcast_ref::<crate::providers::api::NativeToolInputError>()
                    .is_some(),
                "{input}"
            );
        }
        assert!(
            TerminalFilter::parse(&json!({
                "query":"x","contextLines":20.0,"matchLimit":100
            }))
            .is_ok()
        );
        let definition = definitions(false)
            .into_iter()
            .find(|tool| tool["name"] == "capture_terminal")
            .unwrap();
        for mut input in [
            json!({"terminalId":"t","query":""}),
            json!({"terminalId":"t","query":"x","contextLines":21}),
            json!({"terminalId":"t","query":"x","matchLimit":0}),
            json!({"terminalId":"t","query":"x","matchLimit":2.5}),
        ] {
            assert!(super::super::validate_input(&definition["inputSchema"], &mut input).is_err());
        }
        let mut original = json!({"terminalId":"t","start":-2.5,"end":3.9});
        super::super::validate_input(&definition["inputSchema"], &mut original).unwrap();
        assert!(TerminalFilter::parse(&original).unwrap().is_none());
    }

    #[test]
    fn capture_filter_uses_snapshot_negative_and_fractional_row_origins() {
        for (start, lines, expected) in [
            (Some(-2.0), vec!["two", "three"], vec![2, 3]),
            (Some(-2.9), vec!["one", "two", "three"], vec![1, 2, 3]),
            (Some(1.1), vec!["one"], vec![1]),
            (Some(-999.0), vec!["zero"], vec![0]),
            (Some(999.0), vec!["three"], vec![3]),
            (Some(-0.5), vec![], vec![]),
            (Some(1.9), vec![], vec![]),
        ] {
            // Captured windows follow screen_tests' inclusive bounds, including
            // reversed fractional ranges which are empty before truncation.
            let result = filtered(json!({"query":"e","contextLines":2}), &lines, 4, start);
            let indices: Vec<_> = result["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line["index"].as_u64().unwrap())
                .collect();
            assert_eq!(indices, expected, "{start:?}");
            assert_eq!(result["totalLines"], 4);
            if let (Some(first), Some(last)) = (expected.first(), expected.last()) {
                assert_eq!(result["inspectedRange"], json!({"start":first,"end":last}));
            } else {
                assert!(result["inspectedRange"].is_null());
            }
        }
    }

    #[test]
    fn capture_filter_merges_context_and_keeps_unicode_and_literal_text() {
        let result = filtered(
            json!({"query":"ÉRROR [x]","contextLines":1}),
            &[
                "before",
                "érror [x] 日本語",
                "between",
                "ÉRROR [x] again",
                "after",
            ],
            20,
            Some(10.8),
        );
        assert_eq!(result["totalMatches"], 2);
        assert_eq!(
            result["lines"],
            json!([
                {"index":10,"text":"before","matched":false},
                {"index":11,"text":"érror [x] 日本語","matched":true},
                {"index":12,"text":"between","matched":false},
                {"index":13,"text":"ÉRROR [x] again","matched":true},
                {"index":14,"text":"after","matched":false}
            ])
        );
        assert_eq!(result["inspectedRange"], json!({"start":10,"end":14}));
        assert_eq!(result["truncated"], false);
        assert!(result["nextRange"].is_null());
        let literal = filtered(
            json!({"query":".*","contextLines":0}),
            &["error", ".*"],
            2,
            None,
        );
        assert_eq!(literal["totalMatches"], 1);
        assert_eq!(literal["lines"][0]["index"], 1);
    }

    #[test]
    fn capture_filter_matches_multiline_literals_and_overlapping_spans() {
        let result = filtered(
            json!({"query":"ÉRROR\n日本語\ntrace","contextLines":0}),
            &["prefix érror", "日本語", "TRACE suffix", "not included"],
            10,
            Some(-4.0),
        );
        assert_eq!(result["totalMatches"], 1);
        assert_eq!(result["lines"].as_array().unwrap().len(), 3);
        assert!(
            result["lines"]
                .as_array()
                .unwrap()
                .iter()
                .all(|line| line["matched"] == true)
        );
        assert_eq!(result["lines"][0]["index"], 6);
        let overlapping = filtered(
            json!({"query":"x\nx","contextLines":0}),
            &["x", "x", "x"],
            3,
            None,
        );
        assert_eq!(overlapping["totalMatches"], 2);
        assert_eq!(overlapping["lines"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn capture_filter_reports_no_matches_and_bounded_continuation() {
        let empty = filtered(json!({"query":"missing"}), &["error"], 10, Some(3.0));
        assert_eq!(empty["lines"], json!([]));
        assert_eq!(empty["totalMatches"], 0);
        assert_eq!(empty["noMatches"], true);
        assert_eq!(empty["truncated"], false);
        assert_eq!(empty["inspectedRange"], json!({"start":3,"end":3}));
        let result = filtered(
            json!({"query":"ERROR","contextLines":1,"matchLimit":1}),
            &["before", "error error", "context", "ERROR", "after"],
            10,
            Some(-5.0),
        );
        assert_eq!(result["totalMatches"], 2);
        assert_eq!(result["returnedMatches"], 1);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["nextRange"], json!({"start":8,"end":9}));
        assert_eq!(result["lines"].as_array().unwrap().len(), 3);
        assert_eq!(result["indexScope"], "snapshot");
        assert!(
            result["notice"]
                .as_str()
                .unwrap()
                .contains("not a complete error audit")
        );
        let bounded = filtered(
            json!({"query":"x","contextLines":20,"matchLimit":1}),
            &["x", "x"],
            2,
            None,
        );
        assert_eq!(bounded["lines"].as_array().unwrap().len(), 2);
        assert_eq!(bounded["lines"][1]["matched"], true);
        assert_eq!(bounded["truncated"], true);
    }
}
