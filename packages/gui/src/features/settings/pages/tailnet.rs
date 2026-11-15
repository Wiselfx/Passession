//! Application-owned embedded Tailscale controls. Authentication URLs are consumed only
//! by the originating explicit login request, never retained in UI state or preferences.

use std::rc::Rc;

use serde_json::{Value, json};

use super::View;
use crate::features::settings::i18n::t;
use crate::features::settings::model::forms::{FormSheet, SettingField};
use crate::features::settings::rpc::{Env, ReqError, handler};
use crate::features::settings::state::{Settings, SheetKind};
use crate::features::settings::ui::kit::{Ui, button, row, switch};

const DEFAULT_CONTROL: &str = "https://controlplane.tailscale.com";

#[derive(Default)]
pub struct TailnetState {
    loaded: bool,
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
            Err(error) => s.tailnet.error = Some(error.text()),
        }
    });
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
    ui.card_begin();
    ui.row(row(t("tailnet.title")).hint(t("tailnet.scope")));
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
    ui.row(row(t("tailnet.enable")).control(switch("tailnet.enable", enabled, toggle)));
    let status = match backend.as_str() {
        "Running" => t("tailnet.states.running"),
        "NeedsLogin" => t("tailnet.states.needsLogin"),
        "NeedsMachineAuth" => t("tailnet.states.needsApproval"),
        "Starting" => t("tailnet.states.starting"),
        "Stopped" => t("tailnet.states.stopped"),
        "Disabled" => t("tailnet.states.disabled"),
        "Error" => t("tailnet.states.error"),
        "NoState" | "" => t("tailnet.states.noState"),
        _ => backend.clone(),
    };
    ui.row(row(t("tailnet.status")).hint(if pending {
        t("tailnet.pending")
    } else {
        status
    }));
    for (key, label) in [
        ("account", "tailnet.account"),
        ("tailnet", "tailnet.network"),
        ("nodeName", "tailnet.device"),
    ] {
        let data = text(value, key);
        if !data.is_empty() {
            ui.row(row(t(label)).hint(data));
        }
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
        .unwrap_or_else(|| text(value, "error"));
    if !error.is_empty() {
        ui.row(row(t("tailnet.requestFailed")).error(Some(error)));
    }
    ui.card_end();
    ui.gap(24.0);
    ui.card_begin();
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
    ui.card_begin();
    let login = (enabled && !pending)
        .then(|| handler(|s, env| request(s, env, json!({"action": "login"}))));
    ui.row(
        row(t("tailnet.login"))
            .hint(t("tailnet.loginHint"))
            .control(button("tailnet.login", t("tailnet.login"), login).ctl()),
    );
    let refresh =
        (!pending).then(|| handler(|s, env| request(s, env, json!({"action": "status"}))));
    ui.row(
        row(t("tailnet.refresh"))
            .control(button("tailnet.refresh", t("tailnet.refresh"), refresh).ctl()),
    );
    let disconnect = (enabled && !pending && backend != "Stopped").then(|| {
        handler(|s, _| {
            confirm_action(
                s,
                "disconnect",
                "tailnet.disconnect",
                "tailnet.disconnectConfirm",
            )
        })
    });
    ui.row(
        row(t("tailnet.disconnect"))
            .hint(t("tailnet.disconnectHint"))
            .control(button("tailnet.disconnect", t("tailnet.disconnect"), disconnect).ctl()),
    );
    let reconnect =
        (!pending && (!enabled || backend == "Stopped" || backend == "Error")).then(|| {
            handler(|s, env| {
                let message = configuration(&env.ctx.state["tailnet"], true);
                request(s, env, message);
            })
        });
    ui.row(
        row(t("tailnet.reconnect"))
            .control(button("tailnet.reconnect", t("tailnet.reconnect"), reconnect).ctl()),
    );
    let logout = (!pending).then(|| {
        handler(|s, _| confirm_action(s, "logout", "tailnet.logout", "tailnet.logoutConfirm"))
    });
    ui.row(
        row(t("tailnet.logout"))
            .hint(t("tailnet.logoutHint"))
            .control(
                button("tailnet.logout", t("tailnet.logout"), logout)
                    .danger_text()
                    .ctl(),
            ),
    );
    ui.card_end();
}

#[cfg(test)]
mod tests {
    use super::*;

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
