//! Desktop updates remain owned by the GUI. This module never installs artifacts.
use super::{SubmitRequest, update_operation, update_operation_id};
use crate::daemon_control::managed::desktop_protocol_path;
use crate::{daemon_control, state::AppState};
use anyhow::{Context, Result, bail, ensure};
use paseo_protocol::components::{ComponentId, ComponentIdentity, RestartGroup, validate_identity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const MAX_FRAME: u64 = 64 * 1024;
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Owner {
    schema_version: u32,
    address: SocketAddr,
    pub(crate) token: String,
    server_id: String,
    daemon_home: PathBuf,
    daemon_pid: u32,
    daemon_started_at: String,
    gui_pid: u32,
    gui_birth_token: String,
    gui_executable: PathBuf,
    components: BTreeMap<ComponentId, ComponentIdentity>,
    capabilities: Vec<String>,
}

pub(crate) async fn desktop_managed(home: &Path) -> Result<bool> {
    Ok(daemon_control::read_daemon_instance(home)
        .await?
        .context("Cannot establish current daemon owner")?
        .desktop_managed)
}

pub(crate) fn drain_policy(args: &Value) -> Result<(bool, u64)> {
    let policy = super::remote_submit_request(args)?;
    Ok((policy.force_running, policy.idle_timeout_ms))
}

// Never chmod an untrusted file into looking private. Reads are bounded before parsing.
async fn private_json(home: &Path, path: &Path) -> Result<Value> {
    let canonical_home = tokio::fs::canonicalize(home).await?;
    let home = desktop_protocol_path(&canonical_home);
    let normalized_path = desktop_protocol_path(path);
    ensure!(
        normalized_path.starts_with(&home),
        "Desktop update record belongs to another home"
    );
    let mut ancestor = path.to_path_buf();
    loop {
        let metadata = tokio::fs::symlink_metadata(&ancestor).await?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Desktop update path contains a symlink"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() },
                "Desktop update record belongs to another user"
            );
            ensure!(
                metadata.mode() & 0o077 == 0,
                "Desktop update record is not private"
            );
        }
        #[cfg(windows)]
        if desktop_protocol_path(&ancestor).as_ref() != home.as_ref() {
            windows_private_acl(&ancestor).await?;
        }
        if desktop_protocol_path(&ancestor).as_ref() == home.as_ref() {
            break;
        }
        ensure!(ancestor.pop(), "Invalid desktop update record path");
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).await?;
    ensure!(
        file.metadata().await?.is_file(),
        "Desktop update record is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_FRAME + 1).read_to_end(&mut bytes).await?;
    ensure!(
        bytes.len() as u64 <= MAX_FRAME,
        "Desktop update record exceeds limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) async fn owner(home: &Path, server: &str) -> Result<Owner> {
    let home = tokio::fs::canonicalize(home).await?;
    let registry = home.join("desktop-app-update/owner.json");
    let owner: Owner = serde_json::from_value(private_json(&home, &registry).await
        .context("DESKTOP_OWNER_UNAVAILABLE: open the owning desktop app; older apps require a packaged desktop upgrade")?)?;
    ensure!(
        owner.schema_version == 2
            && owner.server_id == server
            && desktop_protocol_path(&owner.daemon_home) == desktop_protocol_path(&home),
        "Desktop owner home/server identity mismatch"
    );
    ensure!(
        owner
            .capabilities
            .iter()
            .any(|capability| capability == "desktop-components-v1")
            && owner.capabilities.len() <= 16
            && owner.components.get(&ComponentId::Daemon) == Some(&crate::component_identity()),
        "DESKTOP_OWNER_UNAVAILABLE: a compatible component-aware desktop owner is required"
    );
    validate_components(&owner.components)?;
    ensure!(
        owner.address.ip() == std::net::Ipv4Addr::LOCALHOST && owner.address.port() != 0,
        "Desktop owner endpoint must be IPv4 loopback"
    );
    ensure!(
        owner.token.len() == 64 && owner.token.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid desktop owner authentication"
    );
    let daemon = daemon_control::read_daemon_instance(&home)
        .await?
        .context("Desktop daemon stopped")?;
    ensure!(
        daemon.desktop_managed
            && daemon.pid == owner.daemon_pid
            && daemon.started_at == owner.daemon_started_at
            && daemon.server_id.as_deref() == Some(server),
        "Desktop daemon owner changed"
    );
    daemon_control::managed::verify_desktop_process(
        owner.gui_pid,
        &owner.gui_birth_token,
        &owner.gui_executable,
    )?;
    Ok(owner)
}

async fn call(home: &Path, server: &str, command: &str, args: Value) -> Result<Value> {
    let owner = owner(home, server).await?;
    let request = json!({"schemaVersion":2,"token":owner.token,"serverId":owner.server_id,
        "daemonHome":owner.daemon_home,"daemonPid":owner.daemon_pid,"daemonStartedAt":owner.daemon_started_at,
        "guiPid":owner.gui_pid,"guiBirthToken":owner.gui_birth_token,"guiExecutable":owner.gui_executable,
        "command":command,"args":args});
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let mut stream = tokio::net::TcpStream::connect(owner.address).await?;
        let mut request = serde_json::to_vec(&request)?;
        ensure!(
            request.len() < MAX_FRAME as usize,
            "Desktop owner request exceeds limit"
        );
        request.push(b'\n');
        stream.write_all(&request).await?;
        let mut bytes = Vec::new();
        tokio::io::BufReader::new(stream)
            .take(MAX_FRAME + 1)
            .read_until(b'\n', &mut bytes)
            .await?;
        ensure!(
            bytes.len() <= MAX_FRAME as usize && bytes.last() == Some(&b'\n'),
            "Invalid desktop owner response frame"
        );
        let reply: Value = serde_json::from_slice(&bytes)?;
        if reply["ok"] != true {
            let message: String = reply["message"]
                .as_str()
                .unwrap_or("Desktop owner rejected update")
                .chars()
                .take(1024)
                .collect();
            bail!("{message}");
        }
        Ok::<_, anyhow::Error>(reply["result"].clone())
    })
    .await
    .context("Desktop owner request timed out; retry the same operation ID")??;
    Ok(result)
}

pub(crate) async fn check(home: &Path, server: &str) -> Result<Value> {
    let result = call(home, server, "check", json!({})).await?;
    ensure!(
        result["serverId"] == server && result["targetKind"] == "app",
        "Desktop check host identity mismatch"
    );
    let components = component_map(&result["components"])?;
    ensure!(
        components.get(&ComponentId::Daemon) == Some(&crate::component_identity()),
        "Desktop check reports another daemon component"
    );
    let changes: Vec<ComponentChange> =
        serde_json::from_value(result["availableComponents"].clone())?;
    ensure!(changes.len() <= 4, "Too many component changes");
    let mut changed = BTreeSet::new();
    for change in &changes {
        validate_identity(&change.target)?;
        paseo_protocol::components::validate_sha256(&change.artifact_sha256)?;
        ensure!(
            changed.insert(change.id)
                && components.get(&change.id) == Some(&change.current)
                && change.download_size > 0
                && change.download_size <= paseo_protocol::components::MAX_ARTIFACT_BYTES
                && change.activation_only == (change.current == change.target)
                && change.requires.values().all(|abi| *abi > 0),
            "Invalid available component change"
        );
    }
    let requirements: BTreeMap<ComponentId, BTreeMap<ComponentId, u32>> =
        serde_json::from_value(result["componentRequirements"].clone())?;
    ensure!(
        requirements.keys().eq(components.keys())
            && requirements.values().all(|requires| requires
                .iter()
                .all(|(id, abi)| *abi > 0 && components.contains_key(id))),
        "Invalid current component requirements"
    );
    let _: RestartGroup = serde_json::from_value(result["restartGroup"].clone())?;
    let groups: Vec<Vec<ComponentId>> = serde_json::from_value(result["atomicGroups"].clone())?;
    ensure!(
        groups.len() <= 4 && groups.iter().all(|group| group.len() <= 4),
        "Invalid component atomic groups"
    );
    // Allowlist prevents owner implementation details from becoming a remote API.
    let mut view = json!({});
    for key in [
        "serverId",
        "targetKind",
        "current",
        "latest",
        "supported",
        "updateAvailable",
        "owner",
        "supportError",
        "checkError",
        "components",
        "availableComponents",
        "componentRequirements",
        "restartGroup",
        "atomicGroups",
    ] {
        view[key] = result[key].clone();
    }
    view["current"] = json!({"version":components[&ComponentId::App].version,
        "sourceSha":components[&ComponentId::App].source_sha});
    view["owner"] = json!({"kind":"desktopApp"});
    view["components"] = json!(components);
    view["availableComponents"] = json!(changes);
    if !result["latest"].is_null() {
        let latest: super::updater::ReleaseIdentity =
            serde_json::from_value(result["latest"].clone())?;
        latest.validate()?;
        view["latest"] = json!(latest);
    }
    Ok(view)
}

pub(crate) async fn submit(
    home: &Path,
    server: &str,
    request: SubmitRequest,
    components: Option<BTreeSet<ComponentId>>,
) -> Result<Value> {
    ensure!(
        matches!(request.source, super::UpdateSource::Release) && request.resume.is_none(),
        "Desktop updates accept official releases only"
    );
    let expected = request.expected_release.as_ref().context(
        "Check the desktop release before submitting its exact expectedRelease identity",
    )?;
    expected.validate()?;
    {
        let _admission = update_operation::AdmissionLease::acquire(home, "admission")?;
        update_operation::ensure_app_cutover_allowed(home).await?;
    }
    let mut args = json!({"operationId":request.operation_id,
        "expectedRelease":expected,"idleTimeoutMs":request.idle_timeout_ms,"forceRunning":request.force_running});
    if let Some(components) = &components {
        args["selectedComponents"] = json!(components);
    }
    let existing = operation(home, server, &request.operation_id).await;
    match existing {
        Ok(record) => require_request(&record, &request, components.as_ref())?,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    let result = call(home, server, "submit", args).await?;
    ensure!(
        result["operationId"] == request.operation_id && result["serverId"] == server,
        "Desktop submit identity mismatch"
    );
    require_request(
        &operation(home, server, &request.operation_id).await?,
        &request,
        components.as_ref(),
    )?;
    status_local(home, server, &request.operation_id).await
}

fn require_request(
    record: &Value,
    request: &SubmitRequest,
    components: Option<&BTreeSet<ComponentId>>,
) -> Result<()> {
    let admitted = super::remote_submit_request(&record["request"])?;
    ensure!(
        admitted.operation_id == request.operation_id
            && admitted.expected_release == request.expected_release
            && admitted.idle_timeout_ms == request.idle_timeout_ms
            && admitted.force_running == request.force_running
            && super::selected_components(&record["request"])?.as_ref() == components,
        "Operation ID already belongs to a different complete update request"
    );
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComponentChange {
    id: ComponentId,
    current: ComponentIdentity,
    target: ComponentIdentity,
    download_size: u64,
    artifact_sha256: String,
    activation_only: bool,
    requires: BTreeMap<ComponentId, u32>,
}

fn validate_components(components: &BTreeMap<ComponentId, ComponentIdentity>) -> Result<()> {
    ensure!(components.len() == 4, "Desktop component set is incomplete");
    for identity in components.values() {
        validate_identity(identity)?;
    }
    Ok(())
}

fn component_map(value: &Value) -> Result<BTreeMap<ComponentId, ComponentIdentity>> {
    let components = serde_json::from_value(value.clone())?;
    validate_components(&components)?;
    Ok(components)
}

fn operation_path(home: &Path, id: &str) -> PathBuf {
    home.join("desktop-app-update/operations")
        .join(id)
        .join("operation.json")
}

pub(crate) async fn operation(home: &Path, server: &str, id: &str) -> Result<Value> {
    ensure!(
        update_operation_id(id)? == id,
        "Noncanonical desktop operation ID"
    );
    let home = tokio::fs::canonicalize(home).await?;
    let record = private_json(&home, &operation_path(&home, id)).await?;
    ensure!(
        record["operationId"] == id && record["serverId"] == server,
        "Desktop operation identity mismatch"
    );
    Ok(record)
}

pub(crate) async fn status(home: &Path, server: &str, id: &str) -> Result<Value> {
    // A live owner reconciles interrupted sessions. Its durable helper journal,
    // not the IPC acknowledgement, remains the authority for commit.
    if let Ok(result) = call(home, server, "status", json!({"operationId":id})).await {
        ensure!(
            result["operationId"] == id && result["serverId"] == server,
            "Desktop status identity mismatch"
        );
    }
    status_local(home, server, id).await
}

async fn status_local(home: &Path, server: &str, id: &str) -> Result<Value> {
    let record = operation(home, server, id).await?;
    let helper = if record["helperStarted"] == true {
        let home = tokio::fs::canonicalize(home).await?;
        let path = operation_path(&home, id).with_file_name("status.json");
        match private_json(&home, &path).await {
            Ok(status) => {
                let request = private_json(
                    &home,
                    &operation_path(&home, id).with_file_name("request.json"),
                )
                .await?;
                let request_home = Path::new(
                    request["daemonHome"]
                        .as_str()
                        .context("Missing helper daemon home")?,
                );
                ensure!(
                    request["schemaVersion"] == 2
                        && request["operationId"] == id
                        && request["serverId"] == server
                        && desktop_protocol_path(request_home) == desktop_protocol_path(&home)
                        && request["restartGroup"] == record["restartGroup"],
                    "Desktop helper request belongs to another operation or host"
                );
                let identities = request["targetComponents"]
                    .as_object()
                    .context("Missing helper target components")?
                    .iter()
                    .map(|(id, installed)| (id.clone(), installed["component"]["identity"].clone()))
                    .collect::<serde_json::Map<_, _>>();
                ensure!(
                    Value::Object(identities) == record["targetComponents"],
                    "Desktop helper target selection changed"
                );
                use base64::Engine;
                let expected: super::updater::ReleaseIdentity =
                    serde_json::from_value(record["request"]["expectedRelease"].clone())?;
                expected.validate()?;
                let digest =
                    base64::engine::general_purpose::STANDARD.decode(&expected.manifest_sha512)?;
                let mut digest_hex = String::with_capacity(128);
                for byte in digest {
                    use std::fmt::Write as _;
                    write!(&mut digest_hex, "{byte:02x}")?;
                }
                ensure!(
                    request["manifestSha512"] == digest_hex,
                    "Desktop helper release pin changed"
                );
                Some(status)
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    public_operation(&record, helper.as_ref(), server, id)
}

fn public_operation(
    record: &Value,
    helper: Option<&Value>,
    server: &str,
    id: &str,
) -> Result<Value> {
    ensure!(
        record["operationId"] == id && record["serverId"] == server,
        "Desktop operation identity mismatch"
    );
    let source = if let Some(helper) = helper {
        ensure!(
            helper["schemaVersion"] == 2
                && helper["operationId"] == id
                && helper["restartGroup"] == record["restartGroup"],
            "Desktop helper identity mismatch"
        );
        ensure!(
            helper["expectedComponents"] == record["targetComponents"],
            "Desktop helper component identity mismatch"
        );
        helper
    } else {
        record
    };
    let restart: RestartGroup = serde_json::from_value(record["restartGroup"].clone())?;
    let request = super::remote_submit_request(&record["request"])?;
    let selected = super::selected_components(&record["request"])?;
    ensure!(
        request.operation_id == id && request.expected_release.is_some(),
        "Desktop operation has no exact release approval"
    );
    let expected = request
        .expected_release
        .as_ref()
        .context("Missing approved release")?;
    let phase = source["phase"]
        .as_str()
        .context("Missing desktop operation phase")?;
    ensure!(
        matches!(
            phase,
            "admitted"
                | "draining"
                | "preparation"
                | "waitingForParent"
                | "waitingForAuthorization"
                | "installing"
                | "activating"
                | "rollingBack"
                | "starting"
                | "checking"
                | "committed"
                | "failed"
                | "rolledBack"
        ),
        "Unknown desktop update phase"
    );
    let targets = if record.get("candidate") == Some(&Value::Null) {
        ensure!(
            helper.is_none()
                && record["helperStarted"] == false
                && matches!(phase, "admitted" | "preparation" | "failed")
                && record["restartGroup"] == "none"
                && record["targetComponents"]
                    .as_object()
                    .is_some_and(|map| map.is_empty())
                && record["observedComponents"].is_null()
                && record["recoveryRequired"] != true,
            "Unplanned desktop operation has activation state"
        );
        BTreeMap::new()
    } else {
        let targets = component_map(&record["targetComponents"])?;
        ensure!(
            record["candidate"]["version"] == expected.version,
            "Desktop operation collection differs from approval"
        );
        paseo_protocol::components::validate_source_sha(
            record["candidate"]["sourceSha"]
                .as_str()
                .context("Missing collection source")?,
        )?;
        targets
    };
    // Only the helper's health-verified durable status may commit an operation.
    ensure!(
        phase != "committed" || helper.is_some(),
        "Desktop commit has no helper health confirmation"
    );
    let observed = source
        .get("observedComponents")
        .filter(|value| !value.is_null())
        .map(component_map)
        .transpose()?;
    ensure!(
        phase != "committed" || observed.as_ref() == Some(&targets),
        "Desktop commit has no exact observed component identities"
    );
    let outcome = match phase {
        "committed" => Some("committed"),
        "failed" => Some("failed"),
        "rolledBack" => Some("rolled_back"),
        _ => None,
    };
    let text = |key: &str| {
        source[key]
            .as_str()
            .map(|s| s.chars().take(1024).collect::<String>())
    };
    let candidate = record["candidate"].as_object().map(|_| json!({"version":record["candidate"]["version"],"sourceSha":record["candidate"]["sourceSha"]}));
    Ok(
        json!({"operationId":id,"serverId":server,"targetKind":"app","accepted":true,
        "phase":phase,"terminal":outcome.is_some(),"outcome":outcome,"candidate":candidate,
        "targetComponents":targets,"restartGroup":restart,"observedComponents":observed,
        "expectedRelease":request.expected_release,"selectedComponents":selected,
        "message":text("message"),"error":if matches!(outcome, Some("failed" | "rolled_back")) { text("error").or_else(|| text("message")) } else { text("error") },"recoveryRequired":source["recoveryRequired"] == true,
        "actionRequired":if phase == "waitingForAuthorization" { Some("osAuthorization") } else { None }}),
    )
}

pub(crate) async fn ensure_no_active(home: &Path) -> Result<()> {
    let directory = home.join("desktop-app-update/operations");
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Invalid desktop operation name"))?;
        ensure!(
            update_operation_id(&id)? == id,
            "Invalid desktop operation name"
        );
        let record = private_json(home, &entry.path().join("operation.json")).await?;
        let server = record["serverId"]
            .as_str()
            .context("Missing desktop operation server")?;
        let view = status_local(home, server, &id).await?;
        ensure!(
            view["terminal"] == true && view["recoveryRequired"] != true,
            "A desktop app update still owns this installation"
        );
    }
    Ok(())
}

pub(crate) async fn initialize(app: &Arc<AppState>) -> Result<()> {
    let home = tokio::fs::canonicalize(&app.home).await?;
    let mut entries = match tokio::fs::read_dir(home.join("desktop-app-update/operations")).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let server = app.transport.identity()["serverId"]
        .as_str()
        .context("Missing server identity")?
        .to_owned();
    while let Some(entry) = entries.next_entry().await? {
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Invalid desktop operation name"))?;
        let record = operation(&home, &server, &id).await?;
        let view = status_local(&home, &server, &id).await?;
        if record["helperStarted"] == true
            && record["restartGroup"] != "none"
            && (view["terminal"] != true || view["recoveryRequired"] == true)
        {
            pause(app, &id)?;
        }
    }
    Ok(())
}

fn pause(app: &Arc<AppState>, id: &str) -> Result<()> {
    if app.agents.update_admission.operation()?.as_deref() == Some(id) {
        return Ok(());
    }
    let lease = update_operation::AdmissionLease::acquire(&app.home, "desktop-cutover")?;
    app.agents.update_admission.pause(id)?;
    update_operation::announce_app_admission(app, id, true);
    reconcile_pause(app, id.to_owned(), lease);
    Ok(())
}

pub(crate) async fn handle_drain(app: &Arc<AppState>, request: &Value) -> Result<Value> {
    let id = request["operationId"]
        .as_str()
        .context("Missing operationId")?;
    ensure!(
        update_operation_id(id)? == id,
        "Invalid desktop operation ID"
    );
    let server = app.transport.identity()["serverId"]
        .as_str()
        .context("Missing server identity")?
        .to_owned();
    let owner = owner(&app.home, &server).await?;
    use subtle::ConstantTimeEq;
    ensure!(
        bool::from(
            owner
                .token
                .as_bytes()
                .ct_eq(request["ownerToken"].as_str().unwrap_or("").as_bytes())
        ),
        "Desktop drain requires local owner authentication"
    );
    let record = operation(&app.home, &server, id).await?;
    ensure!(
        record["ownerSession"] == owner.token,
        "Desktop operation belongs to another owner session"
    );
    if request["type"] == "daemon.appUpdate.drain.release.request" {
        ensure!(
            record["phase"] == "failed" || record["phase"] == "rolledBack",
            "Active desktop update still owns admission"
        );
        ensure!(
            record["recoveryRequired"] != true,
            "Desktop recovery is still required"
        );
        update_operation::release_app_admission(app, id)?;
        return Ok(json!({"operationId":id,"released":true}));
    }
    ensure!(
        record["restartGroup"] != "none",
        "Helper-only component updates must not stop admission"
    );
    let policy = super::remote_submit_request(&record["request"])?;
    ensure!(
        policy.operation_id == id
            && request["forceRunning"] == policy.force_running
            && request["idleTimeoutMs"] == policy.idle_timeout_ms,
        "Desktop drain policy differs from admitted request"
    );
    let _lease = update_operation::AdmissionLease::acquire(&app.home, "admission")?;
    update_operation::ensure_app_cutover_allowed(&app.home).await?;
    ensure!(
        record["phase"] == "draining"
            || record["phase"] == "admitted"
            || record["phase"] == "preparation",
        "Desktop operation cannot start draining in this phase"
    );
    drain_admitted(app, id, policy.idle_timeout_ms, policy.force_running).await
}

async fn drain_admitted(
    app: &Arc<AppState>,
    id: &str,
    timeout_ms: u64,
    force: bool,
) -> Result<Value> {
    pause(app, id)?;
    let result = drain_work(app, timeout_ms, force).await;
    if result.is_err() {
        update_operation::release_app_admission(app, id)?;
    }
    result.map(|forced| json!({"operationId":id,"ready":true,"forced":forced}))
}

async fn drain_work(app: &Arc<AppState>, timeout_ms: u64, force: bool) -> Result<bool> {
    if force {
        return Ok(true);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    tokio::time::timeout_at(deadline, async {
        loop {
            if app.agents.update_admission.active()? == 0
                && app.agents.update_busy_agents(None).await?.is_empty()
            {
                app.agents.flush_update_events().await?;
                if app.agents.update_admission.active()? == 0
                    && app.agents.update_busy_agents(None).await?.is_empty()
                {
                    return Ok(false);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("UPDATE_BUSY: work did not drain; desktop installation is unchanged")?
}

fn reconcile_pause(app: &Arc<AppState>, id: String, lease: update_operation::AdmissionLease) {
    let app = Arc::downgrade(app);
    tokio::spawn(async move {
        let _lease = lease;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(app) = app.upgrade() else {
                return;
            };
            if app.cancel.is_cancelled() {
                return;
            }
            if app
                .agents
                .update_admission
                .operation()
                .ok()
                .flatten()
                .as_deref()
                != Some(id.as_str())
            {
                return;
            }
            let server = app.transport.identity()["serverId"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let result = async {
                let record = operation(&app.home, &server, &id).await?;
                let view = status_local(&app.home, &server, &id).await?;
                let abandoned =
                    record["helperStarted"] != true && owner(&app.home, &server).await.is_err();
                if (view["terminal"] == true && view["recoveryRequired"] != true) || abandoned {
                    update_operation::release_app_admission(&app, &id)?;
                    return Ok::<_, anyhow::Error>(true);
                }
                Ok(false)
            }
            .await;
            match result {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    // Unknown journal state is not evidence that cutover is safe.
                    tracing::error!(operation_id = id, %error, "Cannot reconcile desktop update admission");
                    return;
                }
            }
        }
    });
}

#[cfg(windows)]
async fn windows_private_acl(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            paseo_client::windows_security::verify_private_directory(
                &path,
                paseo_client::windows_security::AllowedDirectoryUsers::CurrentUser,
            )
        }),
    )
    .await
    .context("Desktop update path ACL verification timed out")??
    .context("Desktop update path ACL is not current-user-only")
}

#[cfg(test)]
mod tests;
