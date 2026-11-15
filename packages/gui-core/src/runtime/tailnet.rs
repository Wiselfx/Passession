//! Process-wide embedded-node ownership. UI messages are local operations, never daemon RPCs.
use super::{Core, directory, hosts, store};
use crate::{CoreError, RequestId, host_config};
use paseo_tailnet::{Config, KeyStore, Manager, Proxy, Status};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const SETTINGS: &str = "paseo.tailnet";

#[derive(Clone, Debug)]
struct Settings {
    enabled: bool,
    hostname: String,
    control_url: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            hostname: "passession".into(),
            control_url: None,
        }
    }
}
impl Settings {
    fn parse(value: &Value) -> Result<Self, CoreError> {
        let enabled = value["enabled"]
            .as_bool()
            .ok_or_else(|| invalid("enabled must be a boolean"))?;
        let hostname = value["hostname"]
            .as_str()
            .ok_or_else(|| invalid("A device hostname is required"))?
            .trim();
        if hostname.is_empty()
            || hostname.len() > 63
            || !hostname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !hostname.as_bytes()[0].is_ascii_alphanumeric()
            || !hostname.as_bytes()[hostname.len() - 1].is_ascii_alphanumeric()
        {
            return Err(invalid(
                "Device hostname must be a DNS label (1–63 letters, digits or hyphens, beginning and ending with a letter or digit).",
            ));
        }
        let control_url = match value.get("controlUrl") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.trim().is_empty() => None,
            Some(Value::String(value)) => {
                let url =
                    url::Url::parse(value.trim()).map_err(|_| invalid("Invalid control URL"))?;
                let loopback = url.host_str().is_some_and(|host| {
                    host.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                });
                if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(invalid(
                        "Control URL must use HTTPS (HTTP is allowed only for literal loopback IPs), without credentials, query or fragment.",
                    ));
                }
                Some(url.to_string())
            }
            _ => return Err(invalid("controlUrl must be a URL or null")),
        };
        Ok(Self {
            enabled,
            hostname: hostname.to_owned(),
            control_url,
        })
    }
    fn json(&self) -> Value {
        json!({"enabled":self.enabled,"hostname":self.hostname,"controlUrl":self.control_url})
    }
    fn runtime(&self, core: &Core) -> Config {
        Config {
            state_dir: core.config.state_dir.join("paseo-native").join("tailnet"),
            hostname: self.hostname.clone(),
            control_url: self.control_url.clone(),
        }
    }
}

#[derive(Default)]
struct OperationResult {
    status: Option<Status>,
    proxy: Option<Proxy>,
    auth_url: Option<String>,
}

pub(super) struct Tailnet {
    manager: Option<Manager>,
    operation: Mutex<()>,
    settings: RefCell<Settings>,
    status: RefCell<Option<Status>>,
    proxy: RefCell<Option<Proxy>>,
    error: RefCell<Option<String>>,
    busy: Cell<bool>,
    generation: Cell<u64>,
    sockets: RefCell<CancellationToken>,
}
impl Tailnet {
    pub fn new(key_store: Option<Arc<dyn KeyStore>>) -> Self {
        Self {
            manager: key_store.map(Manager::new),
            operation: Mutex::new(()),
            settings: RefCell::new(Settings::default()),
            status: RefCell::new(None),
            proxy: RefCell::new(None),
            error: RefCell::new(None),
            busy: Cell::new(false),
            generation: Cell::new(0),
            sockets: RefCell::new(CancellationToken::new()),
        }
    }
    pub fn state(&self) -> Value {
        let mut state = self.settings.borrow().json();
        let status = self.status.borrow();
        state["backendState"] = json!(if self.error.borrow().is_some() {
            "Error"
        } else if !self.settings.borrow().enabled {
            "Disabled"
        } else {
            status
                .as_ref()
                .map(|s| s.backend_state.as_str())
                .unwrap_or("Starting")
        });
        state["account"] = json!(status.as_ref().and_then(|s| s.account.as_ref()));
        state["tailnet"] = json!(status.as_ref().and_then(|s| s.tailnet.as_ref()));
        state["nodeName"] = json!(status.as_ref().and_then(|s| s.node_name.as_ref()));
        state["ips"] = json!(
            status
                .as_ref()
                .map(|s| s.ips.as_slice())
                .unwrap_or_default()
        );
        state["health"] = json!(
            status
                .as_ref()
                .map(|s| s.health.as_slice())
                .unwrap_or_default()
        );
        state["error"] = json!(*self.error.borrow());
        state["busy"] = json!(self.busy.get());
        state
    }
}

fn invalid(message: &str) -> CoreError {
    CoreError::new("TAILNET_CONFIG", message, false)
}
fn unavailable(message: &str) -> CoreError {
    CoreError::new("TAILNET_UNAVAILABLE", message, false)
}

/// The only authority for selecting a host's network. A missing node never means normal routing.
pub(super) fn route(
    core: &Core,
    config: &Value,
) -> Result<(Option<Proxy>, CancellationToken), CoreError> {
    if !host_config::uses_tailnet(config) {
        return Ok((None, core.cancel.clone()));
    }
    if !core.tailnet.settings.borrow().enabled {
        return Err(unavailable(
            "Embedded Tailscale is disabled or changing. This host will not use the normal network.",
        ));
    }
    let proxy = core.tailnet.proxy.borrow().clone().filter(Proxy::is_valid)
        .ok_or_else(|| unavailable("Embedded Tailscale is not connected. Enable it and sign in in Settings; this host will not use the normal network."))?;
    Ok((Some(proxy), core.tailnet.sockets.borrow().clone()))
}

fn invalidate(core: &Rc<Core>) -> u64 {
    let tailnet = &core.tailnet;
    let generation = tailnet.generation.get() + 1;
    tailnet.generation.set(generation);
    tailnet.sockets.borrow().cancel();
    *tailnet.sockets.borrow_mut() = core.cancel.child_token();
    tailnet.proxy.borrow_mut().take();
    let all = core.hosts.borrow().clone();
    let mut affected: HashSet<String> = all
        .iter()
        .filter(|host| host_config::uses_tailnet(&host.config.borrow()))
        .map(|host| host.server_id())
        .collect();
    // Connected-host routes are children of the selected transport, not independent fallback paths.
    loop {
        let old = affected.len();
        for host in &all {
            if host
                .origin
                .as_ref()
                .is_some_and(|origin| affected.contains(origin))
            {
                affected.insert(host.server_id());
            }
        }
        if affected.len() == old {
            break;
        }
    }
    for host in all {
        if affected.contains(&host.server_id()) {
            hosts::close_link(core, &host);
            let selector = host.selector.borrow().clone();
            if let Some(selector) = selector {
                selector.connections_changed(false);
            }
        }
    }
    generation
}

pub(super) fn initialize(core: &Rc<Core>) {
    let saved = core.store.borrow().prefs.values.get(SETTINGS).cloned();
    if let Some(saved) = saved {
        match Settings::parse(&saved) {
            Ok(settings) => *core.tailnet.settings.borrow_mut() = settings,
            Err(error) => *core.tailnet.error.borrow_mut() = Some(error.message),
        }
    }
    resume(core);
    let weak = Rc::downgrade(core);
    tokio::task::spawn_local(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(core) = weak.upgrade() else {
                break;
            };
            if core.closing.get() {
                break;
            }
            if core.tailnet.settings.borrow().enabled && !core.tailnet.busy.get() {
                launch(&core, None, "status".into(), None);
            }
        }
    });
}

pub(super) fn resume(core: &Rc<Core>) {
    if core.tailnet.settings.borrow().enabled && !core.tailnet.busy.get() {
        launch(core, None, "resume".into(), None);
    }
}

pub(super) fn request(core: &Rc<Core>, id: RequestId, message: Value) {
    let action = message["action"].as_str().unwrap_or_default();
    let settings = match action {
        "configure" => match Settings::parse(&message) {
            Ok(settings) => Some(settings),
            Err(error) => {
                respond(core, &id, action, Err(error), None);
                return;
            }
        },
        "status" | "login" | "disconnect" | "logout" => None,
        _ => {
            respond(
                core,
                &id,
                action,
                Err(invalid("Unknown embedded Tailscale action")),
                None,
            );
            return;
        }
    };
    launch(core, Some(id), action.to_owned(), settings);
}

fn respond(
    core: &Core,
    id: &str,
    action: &str,
    result: Result<(), CoreError>,
    auth_url: Option<String>,
) {
    let mut payload = json!({"requestId":id,"action":action,"success":result.is_ok(),"tailnet":core.tailnet.state()});
    if let Err(error) = result {
        payload["error"] = json!(error.message);
    }
    if let Some(url) = auth_url {
        payload["authUrl"] = json!(url);
    }
    core.respond(
        "",
        id,
        Ok(json!({"type":"tailnet.response","payload":payload})),
    );
}

fn launch(core: &Rc<Core>, id: Option<RequestId>, action: String, settings: Option<Settings>) {
    if action == "status" && core.tailnet.busy.get() {
        if let Some(id) = id {
            respond(core, &id, &action, Ok(()), None);
        }
        return;
    }
    let mutation = action != "status";
    let generation = if mutation {
        invalidate(core)
    } else {
        core.tailnet.generation.get()
    };
    if let Some(settings) = settings {
        *core.tailnet.settings.borrow_mut() = settings;
    }
    if matches!(action.as_str(), "disconnect" | "logout") {
        core.tailnet.settings.borrow_mut().enabled = false;
    }
    core.tailnet.busy.set(true);
    if mutation {
        *core.tailnet.error.borrow_mut() = None;
    }
    core.touch();
    let core = core.clone();
    tokio::task::spawn_local(async move {
        let _guard = core.tailnet.operation.lock().await;
        if core.closing.get() || generation != core.tailnet.generation.get() {
            if let Some(id) = id {
                respond(
                    &core,
                    &id,
                    &action,
                    Err(invalid("Operation superseded by a newer Tailscale setting")),
                    None,
                );
            }
            return;
        }
        let settings = core.tailnet.settings.borrow().clone();
        let enabled = settings.enabled;
        let config = settings.runtime(&core);
        let result: Result<OperationResult, CoreError> = async {
            let mut persistence_error =
                if matches!(action.as_str(), "configure" | "disconnect" | "logout") {
                    core.store.borrow_mut().prefs.values.insert(SETTINGS.into(), settings.json());
                    store::write_prefs(&core).await.err()
                } else {
                    None
                };
            if action == "status" && !enabled {
                return Ok(OperationResult::default());
            }
            let manager = core.tailnet.manager.clone().ok_or_else(|| {
                unavailable("Embedded Tailscale requires an available platform secret store. Normal host connections are unaffected.")
            })?;
            let operation = action.clone();
            // A storage failure must not prevent disconnect/revocation. Never start a newly
            // enabled node whose intent could not be persisted.
            if action == "configure" && enabled
                && let Some(error) = persistence_error.take()
            {
                let _ = tokio::task::spawn_blocking(move || manager.disconnect()).await;
                return Err(error);
            }
            let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                match operation.as_str() {
                    "configure" | "resume" => {
                        manager.disconnect()?;
                        if !enabled {
                            return Ok(OperationResult::default());
                        }
                        let status = manager.start(config)?;
                        let proxy = if status.backend_state == "Running" {
                            Some(manager.proxy()?)
                        } else {
                            None
                        };
                        Ok(OperationResult { status: Some(status), proxy, auth_url: None })
                    }
                    "disconnect" => {
                        manager.disconnect()?;
                        Ok(OperationResult::default())
                    }
                    "logout" => {
                        manager.start(config)?;
                        manager.logout()?;
                        Ok(OperationResult::default())
                    }
                    "login" => {
                        anyhow::ensure!(enabled, "Enable embedded Tailscale before signing in");
                        manager.start(config)?;
                        let auth_url = manager.login()?;
                        let status = manager.status()?;
                        let proxy = if status.backend_state == "Running" {
                            Some(manager.proxy()?)
                        } else {
                            None
                        };
                        Ok(OperationResult { status: Some(status), proxy, auth_url: Some(auth_url) })
                    }
                    "status" => {
                        let status = manager.status()?;
                        let proxy = if status.backend_state == "Running" {
                            Some(manager.proxy()?)
                        } else {
                            None
                        };
                        Ok(OperationResult { status: Some(status), proxy, auth_url: None })
                    }
                    _ => unreachable!(),
                }
            })
            .await
            .map_err(|_| unavailable("Embedded Tailscale worker stopped"))?
            .map_err(|error| unavailable(&error.to_string()))?;
            if let Some(error) = persistence_error {
                return Err(error);
            }
            Ok(outcome)
        }.await;
        if core.closing.get() || generation != core.tailnet.generation.get() {
            if let Some(id) = id {
                respond(
                    &core,
                    &id,
                    &action,
                    Err(invalid("Operation superseded by a newer Tailscale setting")),
                    None,
                );
            }
            return;
        }
        core.tailnet.busy.set(false);
        let mut auth_url = None;
        let result = match result {
            Ok(OperationResult {
                status,
                proxy,
                auth_url: auth,
            }) => {
                if mutation || enabled {
                    *core.tailnet.error.borrow_mut() = None;
                }
                let reconnect = proxy.is_some() && core.tailnet.proxy.borrow().is_none();
                // A failed backend cannot retain a previously usable route.
                if proxy.is_none() && core.tailnet.proxy.borrow().is_some() {
                    invalidate(&core);
                }
                *core.tailnet.status.borrow_mut() = status;
                *core.tailnet.proxy.borrow_mut() = proxy;
                auth_url = auth;
                if reconnect {
                    let hosts = core.hosts.borrow().clone();
                    for host in hosts {
                        if host.reconnect_enabled()
                            && host_config::uses_tailnet(&host.config.borrow())
                        {
                            directory::refresh_host(&core, &host);
                        }
                    }
                }
                Ok(())
            }
            Err(error) => {
                if core.tailnet.proxy.borrow().is_some() {
                    invalidate(&core);
                }
                *core.tailnet.error.borrow_mut() = Some(error.message.clone());
                Err(error)
            }
        };
        core.touch();
        if let Some(id) = id {
            respond(&core, &id, &action, result, auth_url);
        }
    });
}

pub(super) async fn shutdown(core: &Rc<Core>) {
    invalidate(core);
    let _guard = core.tailnet.operation.lock().await;
    if let Some(manager) = core.tailnet.manager.clone() {
        let _ = tokio::task::spawn_blocking(move || manager.disconnect()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tailnet_settings_reject_unsafe_control_urls() {
        for control in [
            "http://example.com",
            "http://localhost:2345",
            "https://user:secret@example.com",
            "file:///tmp/control",
            "https://example.com/#secret",
            "https://example.com/?key=secret",
        ] {
            assert!(
                Settings::parse(&json!({"enabled":true,"hostname":"phone","controlUrl":control}))
                    .is_err()
            );
        }
        for control in [
            "https://control.example.com",
            "http://127.0.0.1:2345",
            "http://[::1]:2345",
        ] {
            assert!(
                Settings::parse(&json!({"enabled":true,"hostname":"phone","controlUrl":control}))
                    .is_ok()
            );
        }
    }
    #[test]
    fn tailnet_settings_and_public_state_never_persist_authentication() {
        let settings = Settings::parse(
            &json!({"enabled":true,"hostname":"phone","authUrl":"secret","password":"secret"}),
        )
        .unwrap();
        assert!(!settings.json().to_string().contains("secret"));
        let node = Tailnet::new(None);
        assert_eq!(node.state()["backendState"], "Disabled");
        assert!(!node.state().to_string().contains("authUrl"));
        *node.status.borrow_mut() = Some(Status {
            backend_state: "Running".into(),
            ..Status::default()
        });
        *node.error.borrow_mut() = Some("Logout was not confirmed; identity retained".into());
        assert_eq!(node.state()["backendState"], "Error");
        assert_eq!(node.state()["enabled"], false);
    }

    fn core(path: &std::path::Path) -> Rc<Core> {
        Core::new(
            crate::CoreConfig {
                state_dir: path.to_owned(),
                daemon_home: None,
                default_endpoint: None,
                tailnet_key_store: None,
            },
            Arc::new(crate::events::EventQueue::new(Arc::new(|| {}))),
        )
    }

    #[tokio::test]
    async fn tailnet_invalidation_closes_only_selected_hosts_and_fences_old_routes() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let core = core(dir.path());
                let selected =
                    json!({"id":"tail","endpoint":"ws://peer:6767/ws","networkMode":"tailnet"});
                let normal = json!({"id":"normal","endpoint":"ws://localhost:6767/ws"});
                let tail = hosts::Host::new(
                    1,
                    selected.clone(),
                    "tail".into(),
                    None,
                    vec![selected.clone()],
                    None,
                );
                let direct = hosts::Host::new(
                    2,
                    normal.clone(),
                    "normal".into(),
                    None,
                    vec![normal.clone()],
                    None,
                );
                core.hosts
                    .borrow_mut()
                    .extend([tail.clone(), direct.clone()]);
                assert!(route(&core, &selected).is_err());
                assert!(route(&core, &normal).unwrap().0.is_none());
                let old = core.tailnet.sockets.borrow().clone();
                let direct_cancel = direct.resource_cancellation();
                let selected_cancel = tail.resource_cancellation();
                let generation = invalidate(&core);
                assert_eq!(generation, 1);
                assert!(old.is_cancelled());
                assert!(selected_cancel.is_cancelled());
                assert!(!direct_cancel.is_cancelled());
                assert!(
                    !tail.resource_cancellation().is_cancelled(),
                    "new unrelated public requests remain possible after disconnect"
                );
                assert!(!core.tailnet.sockets.borrow().is_cancelled());
                core.tailnet.settings.borrow_mut().enabled = true;
                assert!(
                    route(&core, &selected).is_err(),
                    "enabled without a live node must not imply a normal route"
                );
                assert!(
                    route(&core, &normal).unwrap().0.is_none(),
                    "ordinary hosts remain direct while the embedded node is enabled"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn tailnet_newer_request_supersedes_pending_enable_and_missing_keystore_is_explicit() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let core = core(dir.path());
                request(
                    &core,
                    "old".into(),
                    json!({"action":"configure","enabled":true,"hostname":"phone"}),
                );
                request(&core, "new".into(), json!({"action":"disconnect"}));
                let mut responses = Vec::new();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        core.queue.drain_into(&mut responses, 100);
                        if responses
                            .iter()
                            .filter(|event| matches!(event, crate::CoreEvent::Response { .. }))
                            .count()
                            == 2
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let old = responses
                    .iter()
                    .find_map(|event| match event {
                        crate::CoreEvent::Response {
                            request_id,
                            result: Ok(value),
                            ..
                        } if request_id == "old" => Some(value),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(old["payload"]["success"], false);
                assert!(
                    old["payload"]["error"]
                        .as_str()
                        .unwrap()
                        .contains("superseded")
                );
                assert!(!core.tailnet.settings.borrow().enabled);
                assert!(!core.tailnet.busy.get());
                assert!(
                    core.tailnet
                        .error
                        .borrow()
                        .as_deref()
                        .unwrap()
                        .contains("secret store")
                );
            })
            .await;
    }
}
