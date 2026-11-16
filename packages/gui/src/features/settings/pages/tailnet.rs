//! Application-owned embedded Tailscale controls. Authentication URLs are consumed only
//! by the originating explicit login request, never retained in UI state or preferences.

use std::rc::Rc;

use serde_json::{Value, json};

use super::View;
use crate::features::settings::i18n::{t, ta, with_detail};
use crate::features::settings::model::forms::{FormSheet, SettingField};
use crate::features::settings::rpc::{Env, ReqError, handler};
use crate::features::settings::state::{Settings, SheetKind};
use crate::features::settings::ui::draw::Icon;
use crate::features::settings::ui::kit::{Ctl, Ui, Variant, button, icon_button, row, switch};

const DEFAULT_CONTROL: &str = "https://controlplane.tailscale.com";

#[derive(Default)]
pub struct TailnetState {
    loaded: bool,
    /// A status/configuration answer arrived since this page first asked.
    answered: bool,
    pending: bool,
    error: Option<String>,
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect()
}

fn busy(s: &Settings, value: &Value) -> bool {
    s.tailnet.pending || value.get("busy").and_then(Value::as_bool) == Some(true)
}

fn allowed_web_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let loopback = match url.host() {
        Some(url::Host::Domain(_)) => false,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return false,
    };
    url.username().is_empty()
        && url.password().is_none()
        && (url.scheme() == "https" || (url.scheme() == "http" && loopback))
}

fn allowed_auth_url(raw: &str, control: &str) -> bool {
    if !allowed_web_url(raw) {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        || (allowed_web_url(control)
            && url::Url::parse(control).is_ok_and(|control| {
                control.scheme() == "http" && control.origin() == url.origin()
            }))
}

fn validate_config(hostname: &str, control: &str) -> Result<(), ReqError> {
    let bytes = hostname.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    {
        return Err(ReqError::msg(t("tailnet.invalidHostname")));
    }
    if !control.is_empty() {
        let valid = allowed_web_url(control)
            && url::Url::parse(control)
                .is_ok_and(|url| url.query().is_none() && url.fragment().is_none());
        if !valid {
            return Err(ReqError::msg(t("tailnet.invalidControl")));
        }
    }
    Ok(())
}

fn configuration(value: &Value, enabled: bool) -> Value {
    json!({"action": "configure", "enabled": enabled,
        "hostname": value.get("hostname").and_then(Value::as_str).unwrap_or("passession"),
        "controlUrl": value.get("controlUrl").cloned().unwrap_or(Value::Null)})
}

fn request(s: &mut Settings, env: &mut Env, message: Value) {
    if busy(s, &env.ctx.state["tailnet"]) {
        return;
    }
    let login = message.get("action").and_then(Value::as_str) == Some("login");
    s.tailnet.pending = true;
    s.tailnet.error = None;
    s.tailnet_request(env, message, move |s, env, result| {
        s.tailnet.pending = false;
        s.tailnet.answered = true;
        match result {
            Ok(payload) if login => {
                if let Some(url) = payload.get("authUrl").and_then(Value::as_str) {
                    if allowed_auth_url(url, &text(&env.ctx.state["tailnet"], "controlUrl")) {
                        env.open_url(url);
                    } else {
                        s.tailnet.error = Some(t("tailnet.invalidAuthUrl"));
                    }
                }
            }
            Ok(_) => {}
            Err(error) => s.tailnet.error = Some(error_text(&error.text())),
        }
    });
}

/// Localized summary for a raw embedded-Tailscale failure; the raw runtime text
/// stays as the trailing detail.
fn error_text(raw: &str) -> String {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    let key = if lower.contains("timed out") || lower.contains("timeout") {
        "tailnet.errors.timedOut"
    } else if lower.contains("not connected") {
        "tailnet.errors.notConnected"
    } else if lower.contains("secret store") {
        "tailnet.errors.secretStore"
    } else if lower.contains("superseded") {
        "tailnet.errors.superseded"
    } else if lower.contains("unsupported")
        || lower.contains("not supported")
        || lower.contains("worker stopped")
    {
        "tailnet.errors.unavailable"
    } else {
        return raw.to_string();
    };
    with_detail(&t(key), raw)
}

/// Backend state label; an unrecognized state keeps its raw value as detail.
fn backend_label(backend: &str) -> String {
    match backend {
        "Running" => t("tailnet.states.running"),
        "NeedsLogin" => t("tailnet.states.needsLogin"),
        "NeedsMachineAuth" => t("tailnet.states.needsApproval"),
        "Starting" => t("tailnet.states.starting"),
        "Stopped" => t("tailnet.states.stopped"),
        "Disabled" => t("tailnet.states.disabled"),
        "Error" => t("tailnet.states.error"),
        "Paused" => t("tailnet.states.paused"),
        "NoState" | "" => t("tailnet.states.noState"),
        other => ta("tailnet.states.unknown", &[("state", other)]),
    }
}

/// The actions the Status row offers for a backend state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Primary {
    Login,
    Reconnect,
    /// Running or connecting: Pause, and Stop (turns Embedded Tailscale off).
    PauseStop,
    Resume,
}

/// `actionable`: enabled and no operation in flight. A state whose action
/// would be disabled offers none (Enable is the switch above).
fn primary_action(actionable: bool, backend: &str) -> Option<Primary> {
    if !actionable {
        return None;
    }
    match backend {
        "NeedsLogin" | "NeedsMachineAuth" => Some(Primary::Login),
        "Stopped" | "Error" => Some(Primary::Reconnect),
        "Running" | "Starting" => Some(Primary::PauseStop),
        "Paused" => Some(Primary::Resume),
        _ => None,
    }
}

/// Log out applies only to a node that holds a login.
fn logged_in(account: &str, backend: &str) -> bool {
    !account.is_empty() || matches!(backend, "Running" | "Starting" | "Paused")
}

pub fn ensure(s: &mut Settings, env: &mut Env) {
    if !s.tailnet.loaded && !busy(s, &env.ctx.state["tailnet"]) {
        s.tailnet.loaded = true;
        request(s, env, json!({"action": "status"}));
    }
}

fn confirm_action(
    s: &mut Settings,
    action: &'static str,
    title: &'static str,
    description: &'static str,
) {
    s.confirm(
        t(title),
        t(description),
        t(title),
        action == "logout",
        handler(move |s, env| {
            let message = if action == "disable" {
                configuration(&env.ctx.state["tailnet"], false)
            } else {
                json!({"action": action})
            };
            request(s, env, message);
        }),
    );
}

fn edit_config(s: &mut Settings, env: &mut Env, advanced: bool) {
    let value = &env.ctx.state["tailnet"];
    let hostname = value["hostname"].as_str().unwrap_or("").to_string();
    let control = value["controlUrl"].as_str().unwrap_or("").to_string();
    let baseline = (hostname.clone(), control.clone());
    let fields = if advanced {
        vec![
            SettingField::text("controlUrl", t("tailnet.controlUrl"), control)
                .hint(t("tailnet.controlHint")),
        ]
    } else {
        vec![
            SettingField::text("hostname", t("tailnet.hostname"), hostname)
                .required()
                .hint(t("tailnet.hostnameHint")),
        ]
    };
    let id = s.next_id();
    let form = FormSheet::new(
        id,
        t(if advanced {
            "tailnet.advanced"
        } else {
            "tailnet.hostname"
        }),
        fields,
        Rc::new(move |s, env, id, fields| {
            let current = &env.ctx.state["tailnet"];
            if busy(s, current) {
                s.form_done(env, id, Err(ReqError::msg(t("tailnet.pending"))));
                return;
            }
            if baseline.0 != current["hostname"].as_str().unwrap_or("")
                || baseline.1 != current["controlUrl"].as_str().unwrap_or("")
            {
                s.form_done(env, id, Err(ReqError::msg(t("tailnet.configChanged"))));
                return;
            }
            let hostname = fields
                .get("hostname")
                .and_then(Value::as_str)
                .unwrap_or(&baseline.0)
                .trim();
            let control = fields
                .get("controlUrl")
                .and_then(Value::as_str)
                .unwrap_or(&baseline.1)
                .trim();
            if let Err(error) = validate_config(hostname, control) {
                s.form_done(env, id, Err(error));
                return;
            }
            let message = json!({"action": "configure", "enabled": current["enabled"].as_bool().unwrap_or(false),
            "hostname": hostname, "controlUrl": if control.is_empty() { Value::Null } else { Value::String(control.to_string()) }});
            s.tailnet.pending = true;
            s.tailnet.error = None;
            s.tailnet_request(env, message, move |s, env, result| {
                s.tailnet.pending = false;
                s.form_done(env, id, result.map(|_| ()));
            });
        }),
    );
    s.push_sheet(id, SheetKind::Form(form), 520.0);
}

pub fn draw(ui: &mut Ui, s: &Settings, v: &View) {
    let value = &v.tailnet;
    let pending = busy(s, value);
    let enabled = value["enabled"].as_bool().unwrap_or(false);
    let backend = text(value, "backendState");
    let account = text(value, "account");

    ui.section_title(&t("tailnet.title"), None, None);
    ui.card_begin();
    let toggle = (!pending).then(|| {
        handler(move |s, env| {
            if enabled {
                confirm_action(s, "disable", "tailnet.disable", "tailnet.disableConfirm");
            } else {
                let message = configuration(&env.ctx.state["tailnet"], true);
                request(s, env, message);
            }
        })
    });
    ui.row(
        row(t("tailnet.enable"))
            .hint(t("tailnet.scope"))
            .control(switch("tailnet.enable", enabled, toggle)),
    );
    ui.card_end();
    ui.gap(24.0);

    // Status: the state, the actions it calls for, and a refresh glyph.
    ui.section_title(&t("tailnet.status"), None, None);
    ui.card_begin();
    let checking = !s.tailnet.answered && (pending || !s.tailnet.loaded);
    let mut status = if checking {
        t("tailnet.checking")
    } else if pending {
        t("tailnet.pending")
    } else {
        backend_label(&backend)
    };
    let mut controls = Vec::new();
    match primary_action(enabled && !pending, &backend) {
        Some(Primary::Login) => {
            status = format!("{status}\n{}", t("tailnet.loginHint"));
            let login = handler(|s, env| request(s, env, json!({"action": "login"})));
            controls.push(button("tailnet.login", t("tailnet.login"), Some(login)).ctl());
        }
        Some(Primary::Reconnect) => {
            let reconnect = handler(|s, env| {
                let message = configuration(&env.ctx.state["tailnet"], true);
                request(s, env, message);
            });
            controls
                .push(button("tailnet.reconnect", t("tailnet.reconnect"), Some(reconnect)).ctl());
        }
        Some(Primary::PauseStop) => {
            let pause = handler(|s, env| request(s, env, json!({"action": "pause"})));
            controls.push(button("tailnet.pause", t("tailnet.pause"), Some(pause)).ctl());
            let stop = handler(|s, _| {
                confirm_action(s, "disconnect", "tailnet.stop", "tailnet.stopConfirm")
            });
            controls.push(button("tailnet.stop", t("tailnet.stop"), Some(stop)).ctl());
        }
        Some(Primary::Resume) => {
            status = format!("{status}\n{}", t("tailnet.pausedHint"));
            let resume = handler(|s, env| request(s, env, json!({"action": "resume"})));
            controls.push(
                button("tailnet.resume", t("tailnet.resume"), Some(resume))
                    .variant(Variant::Primary)
                    .ctl(),
            );
        }
        None => {}
    }
    controls.push(if pending {
        Ctl::Spinner(16.0)
    } else {
        let refresh = handler(|s, env| request(s, env, json!({"action": "status"})));
        icon_button("tailnet.refresh", Icon::RotateCw, Some(refresh)).tipped(t("tailnet.refresh"))
    });
    ui.row(
        row(t("tailnet.status"))
            .hint(status)
            .control(Ctl::Row(controls, 8.0)),
    );
    let network = text(value, "tailnet");
    if !network.is_empty() {
        ui.row(row(t("tailnet.network")).hint(network));
    }
    if let Some(health) = value["health"].as_array() {
        for item in health.iter().filter_map(Value::as_str) {
            ui.row(
                row(t("tailnet.health")).hint(
                    item.chars()
                        .filter(|c| !c.is_control())
                        .take(512)
                        .collect::<String>(),
                ),
            );
        }
    }
    let error = s
        .tailnet
        .error
        .clone()
        .unwrap_or_else(|| error_text(&text(value, "error")));
    if !error.is_empty() {
        ui.row(row(t("tailnet.requestFailed")).error(Some(error)));
    }
    ui.card_end();
    ui.gap(24.0);

    ui.section_title(&t("tailnet.device"), None, None);
    ui.card_begin();
    let node = text(value, "nodeName");
    if !node.is_empty() {
        ui.row(row(t("tailnet.deviceName")).hint(node));
    }
    if let Some(ips) = value["ips"].as_array() {
        let ips = ips
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|ip| ip.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if !ips.is_empty() {
            ui.row(row(t("tailnet.ips")).hint(ips));
        }
    }
    let edit = (!pending).then(|| handler(|s, env| edit_config(s, env, false)));
    ui.row(
        row(t("tailnet.hostname"))
            .hint(text(value, "hostname"))
            .control(button("tailnet.hostname", t("tailnet.edit"), edit).ctl()),
    );
    let control = text(value, "controlUrl");
    let edit = (!pending).then(|| handler(|s, env| edit_config(s, env, true)));
    ui.row(
        row(t("tailnet.advanced"))
            .hint(if control.is_empty() {
                DEFAULT_CONTROL.to_string()
            } else {
                control
            })
            .control(button("tailnet.advanced", t("tailnet.edit"), edit).ctl()),
    );
    ui.card_end();
    ui.gap(24.0);

    // Destructive log out last, and only for a node that is logged in.
    if logged_in(&account, &backend) {
        ui.section_title(&t("tailnet.account"), None, None);
        ui.card_begin();
        let logout = (!pending).then(|| {
            handler(|s, _| confirm_action(s, "logout", "tailnet.logout", "tailnet.logoutConfirm"))
        });
        let label = if account.is_empty() {
            t("tailnet.account")
        } else {
            account
        };
        ui.row(
            row(label).hint(t("tailnet.logoutHint")).control(
                button("tailnet.logout", t("tailnet.logout"), logout)
                    .danger_text()
                    .ctl(),
            ),
        );
        ui.card_end();
        ui.gap(24.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_offers_only_the_action_the_state_calls_for() {
        for state in ["NeedsLogin", "NeedsMachineAuth"] {
            assert_eq!(primary_action(true, state), Some(Primary::Login), "{state}");
        }
        for state in ["Stopped", "Error"] {
            assert_eq!(
                primary_action(true, state),
                Some(Primary::Reconnect),
                "{state}"
            );
        }
        for state in ["Running", "Starting"] {
            assert_eq!(
                primary_action(true, state),
                Some(Primary::PauseStop),
                "{state}"
            );
        }
        assert_eq!(primary_action(true, "Paused"), Some(Primary::Resume));
        for state in ["Disabled", "NoState", "", "InUseOtherUser"] {
            assert_eq!(primary_action(true, state), None, "{state}");
        }
        // Disabled Tailscale or an operation in flight shows no action at all.
        assert_eq!(primary_action(false, "Stopped"), None);
        assert_eq!(primary_action(false, "NeedsLogin"), None);

        assert!(logged_in("me@example.com", "Stopped"));
        assert!(logged_in("", "Running"));
        assert!(logged_in("", "Paused"));
        assert!(!logged_in("", "NeedsLogin"));
        assert!(!logged_in("", "Disabled"));
    }

    #[test]
    fn unknown_states_and_known_failures_are_localized_with_raw_detail() {
        let unknown = backend_label("InUseOtherUser");
        assert!(unknown.contains("InUseOtherUser"));
        assert_ne!(unknown, "InUseOtherUser");
        assert_eq!(backend_label("Running"), t("tailnet.states.running"));

        let raw = "Embedded Tailscale worker stopped";
        let mapped = error_text(raw);
        assert!(mapped.starts_with(&t("tailnet.errors.unavailable")));
        assert!(mapped.contains(raw));
        assert!(error_text("status request timed out").starts_with(&t("tailnet.errors.timedOut")));
        assert_eq!(error_text("  something new  "), "something new");
        assert_eq!(error_text(""), "");
    }

    #[test]
    fn configuration_rejects_unsafe_urls_and_invalid_device_names() {
        for name in ["", "-node", "node-", "node.name", "한글"] {
            assert!(validate_config(name, "").is_err());
        }
        for control in [
            "http://example.com",
            "https://user:password@example.com",
            "https://example.com?token=x",
            "https://example.com#secret",
            "file:///tmp/control",
        ] {
            assert!(validate_config("passession", control).is_err());
        }
        assert!(validate_config("my-phone-2", "https://controlplane.tailscale.com").is_ok());
        assert!(validate_config("test", "http://127.0.0.1:8080").is_ok());
        assert!(validate_config("test", "http://localhost:8080").is_err());
    }

    #[test]
    fn browser_login_allows_provider_https_but_not_credentials_or_other_schemes() {
        assert!(allowed_auth_url(
            "https://login.tailscale.com/a/one-time",
            ""
        ));
        assert!(allowed_auth_url(
            "https://identity.example.com/authorize?state=one-time",
            ""
        ));
        assert!(!allowed_auth_url("javascript:alert(1)", ""));
        assert!(!allowed_auth_url(
            "https://user:secret@login.tailscale.com",
            ""
        ));
        assert!(!allowed_auth_url("http://login.tailscale.com", ""));
        assert!(!allowed_auth_url("http://127.0.0.1/a/test", ""));
        assert!(!allowed_auth_url(
            "http://localhost/a/test",
            "http://127.0.0.1:8080"
        ));
        assert!(!allowed_auth_url(
            "http://127.0.0.1/a/test",
            "http://127.0.0.1:8080"
        ));
        assert!(allowed_auth_url(
            "http://127.0.0.1:8080/a/test",
            "http://127.0.0.1:8080"
        ));
    }
}
