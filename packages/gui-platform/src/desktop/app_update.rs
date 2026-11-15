//! Private authenticated owner bridge and durable native APP update handoff.
use super::{daemon_service::DaemonRuntime, *};
use paseo_protocol::components::ComponentId;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::{path::Path, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const LIMIT: u64 = 64 * 1024;

fn error(message: impl Into<String>) -> PlatformError {
    PlatformError::new("APP_UPDATE_FAILED", message)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseIdentity {
    version: String,
    manifest_sha512: String,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Submit {
    operation_id: String,
    expected_release: ReleaseIdentity,
    #[serde(default)]
    force_running: bool,
    idle_timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_components: Option<Vec<ComponentId>>,
}
impl Submit {
    fn parse(args: &Value) -> Result<Self, PlatformError> {
        let request: Self =
            serde_json::from_value(args.clone()).map_err(|e| error(e.to_string()))?;
        validate_id(&request.operation_id)?;
        if let Some(selected) = &request.selected_components
            && (selected.is_empty()
                || selected.iter().copied().collect::<BTreeSet<_>>().len() != selected.len())
        {
            return Err(error("Selected components must be nonempty and unique."));
        }
        use base64::Engine as _;
        if request.expected_release.version.is_empty()
            || !base64::engine::general_purpose::STANDARD
                .decode(&request.expected_release.manifest_sha512)
                .is_ok_and(|d| d.len() == 64)
            || !(1..=86_400_000).contains(&request.idle_timeout_ms)
        {
            return Err(error("Invalid pinned APP release or idle timeout."));
        }
        Ok(request)
    }
}
fn validate_id(id: &str) -> Result<(), PlatformError> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| error("Invalid operation UUID."))?;
    if parsed.is_nil() || parsed.hyphenated().to_string() != id {
        return Err(error("Operation UUID must be canonical."));
    }
    Ok(())
}

pub(crate) struct Bridge {
    session: String,
    address: String,
    registration: tokio::sync::Mutex<Option<Value>>,
    admission: tokio::sync::Mutex<()>,
}

pub async fn start(shared: Arc<Shared>) -> Result<(), PlatformError> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let bridge = Arc::new(Bridge {
        session: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        address: listener.local_addr()?.to_string(),
        registration: Default::default(),
        admission: Default::default(),
    });
    shared
        .desktop
        .app_updates
        .set(bridge.clone())
        .map_err(|_| error("APP update bridge already started."))?;
    let refresh_shared = shared.clone();
    let refresh_bridge = bridge.clone();
    shared.spawn(async move {
        loop {
            if let Err(e) = refresh_bridge.refresh(&refresh_shared).await {
                refresh_shared
                    .desktop
                    .log(&format!("APP owner registration: {}", e.message));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
    let recovery = shared.clone();
    shared.spawn(async move {
        if let Err(e) = recover_health(&recovery).await {
            recovery.desktop.report(&e.message);
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let bridge = bridge.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(read.take(LIMIT + 1));
            let mut line = String::new();
            let result = async {
                tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                    .await
                    .map_err(|_| error("Owner authentication timed out."))??;
                if line.len() as u64 > LIMIT {
                    return Err(error("Owner request exceeds limit."));
                }
                let message: Value = super::component_store::strict_json(line.as_bytes())
                    .map_err(PlatformError::from)?;
                bridge.dispatch(&shared, &message).await
            }
            .await;
            let response = match result {
                Ok(result) => json!({"ok":true,"result":result}),
                Err(e) => json!({"ok":false,"code":e.code,"message":e.message}),
            };
            if let Ok(mut bytes) = serde_json::to_vec(&response) {
                bytes.push(b'\n');
                let _ = tokio::time::timeout(Duration::from_secs(5), write.write_all(&bytes)).await;
            }
        });
    }
}

impl Bridge {
    async fn refresh(&self, shared: &Arc<Shared>) -> Result<(), PlatformError> {
        let status = shared.desktop.daemon().await?.status().await?;
        if status["ownedByDesktop"] != true || status["status"] != "running" {
            return Ok(());
        }
        let home = canonical_home(&status)?;
        let root = home.join("desktop-app-update");
        private_dir(&root).await?;
        let identity = process_identity(std::process::id()).await?;
        let mut components = super::component_store::Store::discover()
            .map_err(PlatformError::from)?
            .map(|store| {
                store
                    .selected()
                    .map(|selected| super::component_store::identities(&selected))
            })
            .transpose()
            .map_err(PlatformError::from)?
            .unwrap_or_default();
        if !components.is_empty() {
            for (id, actual) in [
                (ComponentId::App, super::component_store::app_identity()),
                (ComponentId::Daemon, status["componentIdentity"].clone()),
                (
                    ComponentId::Tailnet,
                    paseo_tailnet::native_identity().map_err(PlatformError::from)?,
                ),
            ] {
                components.insert(
                    id,
                    serde_json::from_value(actual)
                        .map_err(|e| error(format!("Missing observed component identity: {e}")))?,
                );
            }
        }
        let registration = json!({"schemaVersion":2,"address":self.address,"token":self.session,
            "serverId":status["serverId"],"daemonPid":status["pid"],"daemonStartedAt":status["startedAt"],
            "daemonHome":home,"guiPid":std::process::id(),"guiBirthToken":identity["birthToken"],
            "guiExecutable":super::component_store::canonical_path(std::env::current_exe()?).map_err(PlatformError::from)?,
            "components":components,"capabilities":["desktop-components-v1"]});
        let mut current = self.registration.lock().await;
        if current.as_ref() != Some(&registration) {
            atomic_json(&root.join("owner.json"), &registration).await?;
            *current = Some(registration);
        }
        Ok(())
    }

    async fn owned_status(&self, shared: &Arc<Shared>) -> Result<Value, PlatformError> {
        let status = shared.desktop.daemon().await?.status().await?;
        if status["ownedByDesktop"] != true || status["status"] != "running" {
            return Err(error(
                "The GUI does not own this running daemon. Start it from this packaged desktop application first.",
            ));
        }
        Ok(status)
    }

    async fn dispatch(
        &self,
        shared: &Arc<Shared>,
        message: &Value,
    ) -> Result<Value, PlatformError> {
        let owner = self
            .registration
            .lock()
            .await
            .clone()
            .ok_or_else(|| error("Desktop owner is not registered."))?;
        if message["schemaVersion"] != 2
            || message["token"] != self.session
            || [
                "serverId",
                "daemonPid",
                "daemonStartedAt",
                "daemonHome",
                "guiPid",
                "guiBirthToken",
                "guiExecutable",
            ]
            .iter()
            .any(|key| message[*key] != owner[*key])
        {
            return Err(PlatformError::new(
                "APP_UPDATE_OWNER_MISMATCH",
                "APP update owner authentication failed.",
            ));
        }
        let status = self.owned_status(shared).await?;
        if status["serverId"] != owner["serverId"]
            || status["pid"] != owner["daemonPid"]
            || status["startedAt"] != owner["daemonStartedAt"]
            || json!(canonical_home(&status)?) != owner["daemonHome"]
        {
            return Err(error(
                "The owned daemon changed; reconnect and check again.",
            ));
        }
        match message["command"].as_str() {
            Some("check") => self.check(shared, &status).await,
            Some("submit") => self.submit(shared, &status, &message["args"]).await,
            Some("status") => {
                let id = message["args"]["operationId"]
                    .as_str()
                    .ok_or_else(|| error("Missing operationId."))?;
                self.status(
                    &canonical_home(&status)?,
                    status["serverId"].as_str().unwrap_or(""),
                    id,
                )
                .await
            }
            _ => Err(error("Unknown APP owner command.")),
        }
    }

    async fn check(&self, shared: &Arc<Shared>, status: &Value) -> Result<Value, PlatformError> {
        let channel = channel(shared, &Value::Null).await;
        let mut result = match shared.desktop.updater.check_app_release(&channel).await {
            Ok(result) => result,
            Err(e) => {
                let local = super::component_store::Store::discover()
                    .map_err(PlatformError::from)?
                    .map(|store| store.selected())
                    .transpose()
                    .map_err(PlatformError::from)?
                    .unwrap_or_default();
                let requirements: std::collections::BTreeMap<_, _> = local
                    .iter()
                    .map(|(id, r)| (*id, &r.component.requires))
                    .collect();
                json!({"current":{"version":shared.desktop.app_version,"sourceSha":option_env!("PASEO_BUILD_SOURCE_SHA").unwrap_or("")},
                    "supported":!local.is_empty(),"updateAvailable":false,"latest":null,"owner":{"kind":"desktopApp"},
                    "supportError":null,"checkError":e.message,"components":super::component_store::identities(&local),
                    "componentRequirements":requirements,"availableComponents":[],"restartGroup":"none",
                    "atomicGroups":paseo_protocol::components::required_atomic_groups(&super::component_store::target().map_err(PlatformError::from)?).map_err(|e|error(e.to_string()))?})
            }
        };
        result["serverId"] = status["serverId"].clone();
        result["targetKind"] = json!("app");
        if result["supported"] == true {
            let actual = observed_components(shared, status).await?;
            if actual != result["components"] {
                result["supported"] = json!(false);
                result["updateAvailable"] = json!(false);
                result["availableComponents"] = json!([]);
                result["supportError"] = json!(
                    "Running component identities differ from the selected installation; complete recovery before updating."
                );
            }
            result["components"] = actual;
        }
        if !helper_path()?.is_file() {
            result["supported"] = json!(false);
            result["supportError"] = json!(
                "Install a packaged desktop APP containing passession-update-helper once before unattended APP updates are available."
            );
        }
        Ok(result)
    }

    async fn submit(
        &self,
        shared: &Arc<Shared>,
        status: &Value,
        args: &Value,
    ) -> Result<Value, PlatformError> {
        let request = Submit::parse(args)?;
        let home = canonical_home(status)?;
        let server = status["serverId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Missing owned server identity."))?;
        let _gate = self.admission.lock().await;
        private_dir(&home.join("desktop-app-update")).await?;
        let root = home.join("desktop-app-update").join("operations");
        private_dir(&root).await?;
        let directory = root.join(&request.operation_id);
        if directory.exists() {
            let existing = read_json(&directory.join("operation.json")).await?;
            if existing["serverId"] != server
                || existing["request"]
                    != serde_json::to_value(&request).map_err(|e| error(e.to_string()))?
            {
                return Err(error(
                    "Operation identity is already bound to another target or policy.",
                ));
            }
            return self.status(&home, server, &request.operation_id).await;
        }
        let mut entries = tokio::fs::read_dir(&root).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if validate_id(&id).is_ok() {
                let operation = self.status(&home, server, &id).await?;
                if operation["terminal"] != true || operation["recoveryRequired"] == true {
                    return Err(error(format!(
                        "APP update {id} is active or requires recovery; inspect that operation instead."
                    )));
                }
            }
        }
        if !helper_path()?.is_file() {
            return Err(error(
                "One-time packaged desktop APP upgrade required: updater helper is missing.",
            ));
        }
        private_dir(&directory).await?;
        let operation = json!({"operationId":request.operation_id,"serverId":server,"targetKind":"app","accepted":true,
            "phase":"preparation","terminal":false,"outcome":null,"candidate":null,"message":"Preparing pinned component update",
            "error":null,"recoveryRequired":false,"actionRequired":null,"request":request,
            "expectedRelease":request.expected_release,"selectedComponents":request.selected_components,
            "targetComponents":{},"observedComponents":null,"restartGroup":"none",
            "ownerSession":self.session,"helperStarted":false});
        atomic_json(&directory.join("operation.json"), &operation).await?;
        let task_shared = shared.clone();
        let task_home = home.clone();
        let task_server = server.to_owned();
        shared.spawn(async move {
            if let Err(e) =
                execute(&task_shared, &task_home, &task_server, &directory, &request).await
            {
                let _ = fail_operation(&directory, &e.message).await;
                if let Ok(daemon) = task_shared.desktop.daemon().await {
                    let _ = daemon.app_update_drain_release(&request.operation_id).await;
                }
                task_shared.desktop.quit.lock().unwrap().quitting = false;
                post_event(PlatformEvent::QuitCancelled);
            }
        });
        Ok(public_operation(operation))
    }

    async fn status(&self, home: &Path, server: &str, id: &str) -> Result<Value, PlatformError> {
        validate_id(id)?;
        let directory = home.join("desktop-app-update").join("operations").join(id);
        let mut operation = read_json(&directory.join("operation.json")).await?;
        if operation["operationId"] != id || operation["serverId"] != server {
            return Err(error("APP operation host identity mismatch."));
        }
        if operation["helperStarted"] == true && operation["terminal"] != true {
            if let Ok(helper) = read_json(&directory.join("status.json")).await {
                if helper["phase"] != "committed"
                    && helper["phase"] != "failed"
                    && helper["phase"] != "rolledBack"
                    && operation["ownerSession"] != self.session
                {
                    let pid = helper["helperProcess"]["pid"]
                        .as_u64()
                        .and_then(|p| u32::try_from(p).ok());
                    let alive = match pid {
                        Some(pid) => process_identity(pid)
                            .await
                            .is_ok_and(|identity| identity == helper["helperProcess"]),
                        None => false,
                    };
                    if !alive {
                        operation["phase"] = json!("failed");
                        operation["terminal"] = json!(true);
                        operation["outcome"] = json!("failed");
                        operation["error"] = json!(
                            "APP updater was interrupted. Native installation state requires recovery; this operation will not be replayed."
                        );
                        operation["recoveryRequired"] = json!(true);
                        atomic_json(&directory.join("operation.json"), &operation).await?;
                        return Ok(public_operation(operation));
                    }
                }
                merge_helper(&mut operation, &helper)?;
            } else if operation["ownerSession"] != self.session {
                operation["phase"] = json!("failed");
                operation["terminal"] = json!(true);
                operation["outcome"] = json!("failed");
                operation["error"] = json!(
                    "APP helper handoff was interrupted before its journal appeared. Inspect retained recovery files."
                );
                operation["recoveryRequired"] = json!(true);
                atomic_json(&directory.join("operation.json"), &operation).await?;
            }
        } else if operation["terminal"] != true && operation["ownerSession"] != self.session {
            operation["phase"] = json!("failed");
            operation["terminal"] = json!(true);
            operation["outcome"] = json!("failed");
            operation["error"] = json!(
                "APP preparation was interrupted before helper handoff. This operation will not be replayed."
            );
            atomic_json(&directory.join("operation.json"), &operation).await?;
        }
        Ok(public_operation(operation))
    }
}

pub async fn install_local(shared: &Arc<Shared>) -> Result<Value, PlatformError> {
    let bridge = shared
        .desktop
        .app_updates
        .get()
        .ok_or_else(|| error("APP updater is starting."))?;
    let status = bridge.owned_status(shared).await?;
    let check = bridge.check(shared, &status).await?;
    if check["supported"] != true || check["updateAvailable"] != true {
        return Err(error("No supported APP update is available."));
    }
    let operation = bridge
        .submit(
            shared,
            &status,
            &json!({"operationId":uuid::Uuid::new_v4().to_string(),
        "expectedRelease":check["latest"],"forceRunning":false,"idleTimeoutMs":300_000}),
        )
        .await?;
    Ok(
        json!({"installed":false,"installerLaunched":false,"operationId":operation["operationId"],
        "version":check["latest"]["version"],"message":"APP update accepted. Running tasks will drain; unsaved documents block shutdown."}),
    )
}

fn canonical_home(status: &Value) -> Result<PathBuf, PlatformError> {
    let home = status["home"]
        .as_str()
        .ok_or_else(|| error("Owned daemon home is missing."))?;
    super::component_store::canonical_path(home).map_err(PlatformError::from)
}
fn helper_path() -> Result<PathBuf, PlatformError> {
    if let Some(store) = super::component_store::Store::discover().map_err(PlatformError::from)? {
        let selected = store.selected().map_err(PlatformError::from)?;
        return store
            .image(&selected[&ComponentId::UpdateHelper])
            .map_err(PlatformError::from);
    }
    Ok(std::env::current_exe()?
        .parent()
        .ok_or_else(|| error("GUI directory is missing."))?
        .join(if cfg!(windows) {
            "passession-update-helper.exe"
        } else {
            "passession-update-helper"
        }))
}
fn public_operation(mut operation: Value) -> Value {
    if let Some(map) = operation.as_object_mut() {
        for key in ["request", "ownerSession", "helperStarted"] {
            map.remove(key);
        }
    }
    operation
}
fn merge_helper(operation: &mut Value, helper: &Value) -> Result<(), PlatformError> {
    if helper["schemaVersion"] != 2
        || helper["operationId"] != operation["operationId"]
        || helper["restartGroup"] != operation["restartGroup"]
        || helper["expectedComponents"] != operation["targetComponents"]
    {
        return Err(error(
            "Helper journal identity differs from admitted APP operation.",
        ));
    }
    let phase = helper["phase"]
        .as_str()
        .ok_or_else(|| error("Missing helper phase."))?;
    let outcome = match phase {
        "committed" => Some("committed"),
        "failed" => Some("failed"),
        "rolledBack" => Some("rolled_back"),
        "preparation" | "waitingForParent" | "activating" | "rollingBack" | "starting"
        | "checking" => None,
        _ => return Err(error("Unknown helper journal phase.")),
    };
    if phase == "committed" {
        if helper["observedComponents"] != operation["targetComponents"] {
            return Err(error(
                "Committed component update lacks independently verified expected identities.",
            ));
        }
        operation["observedComponents"] = helper["observedComponents"].clone();
    }
    operation["phase"] = json!(phase);
    operation["outcome"] = json!(outcome);
    operation["terminal"] = json!(outcome.is_some());
    operation["message"] = helper["message"].clone();
    operation["recoveryRequired"] = helper["recoveryRequired"].clone();
    operation["actionRequired"] = if phase == "waitingForAuthorization" {
        json!("osAuthorization")
    } else {
        Value::Null
    };
    if matches!(outcome, Some("failed" | "rolled_back")) {
        operation["error"] = helper["message"].clone();
    }
    Ok(())
}
async fn change_operation(
    directory: &Path,
    phase: &str,
    message: &str,
) -> Result<Value, PlatformError> {
    let mut operation = read_json(&directory.join("operation.json")).await?;
    operation["phase"] = json!(phase);
    operation["message"] = json!(message);
    atomic_json(&directory.join("operation.json"), &operation).await?;
    Ok(operation)
}
async fn fail_operation(directory: &Path, message: &str) -> Result<(), PlatformError> {
    let mut operation = read_json(&directory.join("operation.json")).await?;
    if operation["helperStarted"] == true {
        // The helper remains the sole activation authority after handoff.
        // Keep observing its durable result; a GUI monitor error cannot turn a
        // live rollback/commit into an invented terminal failure.
        operation["error"] = json!(message);
        operation["message"] = json!(message);
        operation["recoveryRequired"] = json!(true);
        if let Ok(status) = read_json(&directory.join("status.json")).await {
            merge_helper(&mut operation, &status)?;
            if operation["terminal"] != true {
                operation["recoveryRequired"] = json!(true);
            }
        }
        return atomic_json(&directory.join("operation.json"), &operation).await;
    }
    operation["phase"] = json!("failed");
    operation["terminal"] = json!(true);
    operation["outcome"] = json!("failed");
    operation["error"] = json!(message);
    operation["message"] = json!(message);
    atomic_json(&directory.join("operation.json"), &operation).await
}

// Called only after the launched child has exited, before any runtime handoff.
async fn retire_pre_handoff_helper(
    directory: &Path,
    helper_identity: &Value,
) -> Result<(), PlatformError> {
    let mut operation = read_json(&directory.join("operation.json")).await?;
    let path = directory.join("status.json");
    let safe = match tokio::fs::symlink_metadata(&path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
        Ok(_) => {
            let status = read_json(&path).await?;
            if status["helperProcess"] != *helper_identity {
                return Err(error("Exited helper process identity mismatch"));
            }
            merge_helper(&mut operation, &status)?;
            operation["restartGroup"] != "none"
                && matches!(
                    status["phase"].as_str(),
                    Some("preparation" | "waitingForParent")
                )
                && status["recoveryRequired"] != true
        }
    };
    if safe {
        operation["helperStarted"] = json!(false);
        atomic_json(&directory.join("operation.json"), &operation).await?;
    }
    Ok(())
}

async fn execute(
    shared: &Arc<Shared>,
    home: &Path,
    server: &str,
    directory: &Path,
    request: &Submit,
) -> Result<(), PlatformError> {
    let channel = channel(shared, &Value::Null).await;
    let expected =
        serde_json::to_value(&request.expected_release).map_err(|e| error(e.to_string()))?;
    let selected = request
        .selected_components
        .as_ref()
        .map(|ids| ids.iter().copied().collect::<BTreeSet<_>>());
    let mut helper_request = shared
        .desktop
        .updater
        .prepare_helper(super::updater::HelperPreparation {
            channel: &channel,
            operation: &request.operation_id,
            directory,
            home,
            server,
            expected: &expected,
            selected: selected.as_ref(),
        })
        .await?;
    let group = helper_request["restartGroup"]
        .as_str()
        .ok_or_else(|| error("Missing restart group"))?
        .to_owned();
    let target: super::component_store::Installed =
        serde_json::from_value(helper_request["targetComponents"].clone())
            .map_err(|e| error(e.to_string()))?;
    let inventory: paseo_protocol::components::ReleaseInventory =
        super::component_store::read_json(
            Path::new(
                helper_request["inventoryPath"]
                    .as_str()
                    .ok_or_else(|| error("Missing inventory path"))?,
            ),
            true,
        )
        .map_err(PlatformError::from)?;
    let mut operation = read_json(&directory.join("operation.json")).await?;
    operation["candidate"] = json!({"version":inventory.version,"sourceSha":inventory.source_sha});
    operation["targetComponents"] = json!(super::component_store::identities(&target));
    operation["restartGroup"] = json!(group);
    atomic_json(&directory.join("operation.json"), &operation).await?;
    let daemon = shared.desktop.daemon().await?;
    if group != "none" {
        change_operation(
            directory,
            "draining",
            "Waiting for owned runtime tasks before component activation",
        )
        .await?;
        daemon
            .app_update_drain(
                &request.operation_id,
                request.force_running,
                request.idle_timeout_ms,
            )
            .await?;
        helper_request["runtimeProcesses"] = daemon.runtime_processes().await?;
    }
    if group == "gui" && !guarded_consent(shared, QuitReason::RemoteAppUpdate).await {
        return Err(error(
            "Unsaved local documents block GUI/tailnet activation.",
        ));
    }
    let helper = directory.join(if cfg!(windows) {
        "passession-update-helper.exe"
    } else {
        "passession-update-helper"
    });
    super::updater::copy_private(&helper_path()?, &helper, true).await?;
    let request_path = directory.join("request.json");
    atomic_json(&request_path, &helper_request).await?;
    operation["helperStarted"] = json!(true);
    atomic_json(&directory.join("operation.json"), &operation).await?;
    let mut command = tokio::process::Command::new(&helper);
    command
        .arg("--request")
        .arg(&request_path)
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            operation["helperStarted"] = json!(false);
            atomic_json(&directory.join("operation.json"), &operation).await?;
            return Err(e.into());
        }
    };
    let helper_identity = process_identity(
        child
            .id()
            .ok_or_else(|| error("Missing spawned helper PID"))?,
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let ready = loop {
        if let Ok(status) = read_json(&directory.join("status.json")).await {
            if status["helperProcess"] != helper_identity {
                return Err(error(
                    "Helper readiness process identity differs from the launched child",
                ));
            }
            merge_helper(&mut operation, &status)?;
            if operation["terminal"] == true {
                atomic_json(&directory.join("operation.json"), &operation).await?;
                return if operation["outcome"] == "committed" {
                    Ok(())
                } else {
                    Err(error(
                        status["message"]
                            .as_str()
                            .unwrap_or("Component helper failed"),
                    ))
                };
            }
            if status["phase"] == "waitingForParent" && status["ready"] == true {
                break status;
            }
        }
        if child.try_wait()?.is_some() {
            retire_pre_handoff_helper(directory, &helper_identity).await?;
            return Err(error(
                "Component helper exited before authenticated readiness",
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error(
                "Component helper readiness timed out; existing selection retained",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if group == "none" {
        return Err(error(
            "No-restart helper unexpectedly requested runtime handoff",
        ));
    }
    if group == "gui" && !guarded_consent(shared, QuitReason::RemoteAppUpdate).await {
        return Err(error(
            "Unsaved local documents appeared before component handoff.",
        ));
    }
    daemon.close_for_update().await?;
    let handoff =
        json!({"schemaVersion":2,"operationId":request.operation_id,"nonce":ready["nonce"]});
    if let Err(write_error) = atomic_json(&directory.join("handoff.json"), &handoff).await
        && read_json(&directory.join("handoff.json"))
            .await
            .ok()
            .as_ref()
            != Some(&handoff)
    {
        *shared.desktop.daemon.lock().await = None;
        shared.desktop.daemon().await?.initialize().await?;
        return Err(write_error);
    }
    if group == "gui" {
        shared.desktop.quit.lock().unwrap().prepared = true;
        finish_quit(shared, QuitReason::AppUpdate).await;
        return Ok(());
    }
    monitor_health(
        shared,
        directory,
        &helper_request,
        ready["nonce"]
            .as_str()
            .ok_or_else(|| error("Missing helper nonce"))?,
        true,
    )
    .await
}

pub(super) async fn private_dir(path: &Path) -> Result<(), PlatformError> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(path)?;
        }
        #[cfg(windows)]
        {
            std::fs::create_dir(path)?;
        }
    }
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(error("APP update directory must be a real directory."));
    }
    if !super::component_store::same_path(
        &super::component_store::canonical_path(path).map_err(PlatformError::from)?,
        path,
    ) {
        return Err(error(
            "APP update directory has a symlink or noncanonical ancestor.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(error(
                "APP update directory must be private and owned by this user.",
            ));
        }
    }
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().replace('\'', "''");
        super::updater::powershell(&format!("$ErrorActionPreference='Stop'; $p='{path}'; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=New-Object Security.AccessControl.DirectorySecurity; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); $rule=New-Object Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $acl.AddAccessRule($rule); Set-Acl -LiteralPath $p -AclObject $acl")).await?;
    }
    Ok(())
}
async fn read_json(path: &Path) -> Result<Value, PlatformError> {
    let meta = tokio::fs::symlink_metadata(path).await?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > LIMIT {
        return Err(error("Invalid APP update journal file."));
    }
    if !super::component_store::same_path(
        &super::component_store::canonical_path(path).map_err(PlatformError::from)?,
        path,
    ) {
        return Err(error("APP journal has a symlink or noncanonical ancestor."));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(error("APP journal is not private to the current user."));
        }
    }
    let bytes = tokio::fs::read(path).await?;
    super::component_store::strict_json(&bytes).map_err(PlatformError::from)
}
pub(super) async fn atomic_json(path: &Path, value: &Value) -> Result<(), PlatformError> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await?;
    file.write_all(&serde_json::to_vec(value).map_err(|e| error(e.to_string()))?)
        .await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temporary, path).await?;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| error("Missing journal directory."))?,
    )?
    .sync_all()?;
    Ok(())
}

pub(super) async fn process_identity(pid: u32) -> Result<Value, PlatformError> {
    #[cfg(target_os = "linux")]
    let birth = {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await?;
        stat.rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .ok_or_else(|| error("Missing process birth token."))?
            .to_owned()
    };
    #[cfg(target_os = "macos")]
    let birth = {
        let result = tokio::process::Command::new("/bin/ps")
            .env("LC_ALL", "C")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .await?;
        if !result.status.success() {
            return Err(error("Cannot identify process birth."));
        }
        String::from_utf8_lossy(&result.stdout).trim().to_owned()
    };
    #[cfg(windows)]
    let birth = unsafe {
        use ::windows::Win32::{
            Foundation::{CloseHandle, FILETIME},
            System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| error(e.to_string()))?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        result.map_err(|e| error(e.to_string()))?;
        (((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64).to_string()
    };
    if birth.is_empty() {
        return Err(error("Empty process birth token."));
    }
    Ok(json!({"pid":pid,"birthToken":birth}))
}

async fn recover_health(shared: &Arc<Shared>) -> Result<(), PlatformError> {
    let Some(directory) = std::env::var_os("PASEO_UPDATE_OPERATION_DIRECTORY") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    let id = std::env::var("PASEO_UPDATE_OPERATION_ID")
        .map_err(|_| error("Missing recovery operation identity"))?;
    validate_id(&id)?;
    let nonce = std::env::var("PASEO_UPDATE_NONCE").map_err(|_| error("Missing recovery nonce"))?;
    let request = read_json(&directory.join("request.json")).await?;
    if request["schemaVersion"] != 2 || request["operationId"] != id {
        return Err(error("Component recovery request identity differs"));
    }
    monitor_health(shared, &directory, &request, &nonce, false).await
}

async fn monitor_health(
    shared: &Arc<Shared>,
    directory: &Path,
    request: &Value,
    nonce: &str,
    mut restart: bool,
) -> Result<(), PlatformError> {
    let id = request["operationId"]
        .as_str()
        .ok_or_else(|| error("Missing component operation"))?;
    let home = super::component_store::canonical_path(
        request["daemonHome"]
            .as_str()
            .ok_or_else(|| error("Missing daemon home"))?,
    )
    .map_err(PlatformError::from)?;
    if directory != home.join("desktop-app-update").join("operations").join(id) {
        return Err(error("Component recovery directory mismatch"));
    }
    let group = request["restartGroup"]
        .as_str()
        .ok_or_else(|| error("Missing restart group"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    let mut reported = String::new();
    let mut stopped = false;
    let mut runtime_ready = !restart;
    loop {
        let status = read_json(&directory.join("status.json")).await?;
        if status["schemaVersion"] != 2 || status["operationId"] != id || status["nonce"] != nonce {
            return Err(error("Component health transaction authentication failed"));
        }
        let helper_pid = status["helperProcess"]["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| error("Missing helper PID"))?;
        let phase = status["phase"].as_str().unwrap_or("");
        if matches!(phase, "committed" | "failed" | "rolledBack") {
            return Ok(());
        }
        if process_identity(helper_pid).await? != status["helperProcess"] {
            return Err(error(
                "Component helper process identity changed; recovery is required",
            ));
        }
        if let Ok(stop) = read_json(&directory.join("stop.json")).await
            && !stopped
            && stop["schemaVersion"] == 2
            && stop["operationId"] == id
            && stop["nonce"] == nonce
            && stop["restartGroup"] == group
        {
            if group == "gui" && !guarded_consent(shared, QuitReason::RemoteAppUpdate).await {
                return Err(error(
                    "Unsaved documents prevented rollback shutdown; recovery files retained",
                ));
            }
            let daemon = shared.desktop.daemon().await?;
            let processes = daemon.runtime_processes().await?;
            if !processes
                .as_array()
                .ok_or_else(|| error("Invalid runtime process list"))?
                .is_empty()
            {
                daemon.close_for_update().await?;
            } else {
                daemon.close(Some(false)).await?;
            }
            atomic_json(&directory.join("stopped.json"), &json!({"schemaVersion":2,"operationId":id,"nonce":nonce,"runtimeProcesses":processes})).await?;
            stopped = true;
            restart = true;
            runtime_ready = false;
            if group == "gui" {
                shared.desktop.quit.lock().unwrap().prepared = true;
                finish_quit(shared, QuitReason::AppUpdate).await;
                return Ok(());
            }
        }
        let store = super::component_store::Store::discover()
            .map_err(PlatformError::from)?
            .ok_or_else(|| error("Component store disappeared"))?;
        let selection = store.selection().map_err(PlatformError::from)?;
        let rollback = phase == "rollingBack" && selection.trial.is_none();
        let runnable = phase == "checking" || rollback;
        if runnable && restart {
            *shared.desktop.daemon.lock().await = None;
            match shared.desktop.daemon().await?.initialize().await {
                Ok(()) => runtime_ready = true,
                Err(e) => {
                    runtime_ready = false;
                    shared.desktop.log(&format!(
                        "Component trial runtime did not start: {}",
                        e.message
                    ));
                }
            }
            restart = false;
        }
        let health_name = if rollback {
            "rollback-health.json"
        } else {
            "health.json"
        };
        if runnable && reported != health_name && !restart && runtime_ready {
            let daemon = shared.desktop.daemon().await?;
            let actual = daemon.status().await?;
            if actual["ownedByDesktop"] == true
                && actual["status"] == "running"
                && actual["serverId"] == request["serverId"]
                && canonical_home(&actual)? == home
            {
                let expected = if rollback {
                    &request["current"]
                } else {
                    &request["targetComponents"]
                };
                let expected: super::component_store::Installed =
                    serde_json::from_value(expected.clone()).map_err(|e| error(e.to_string()))?;
                let observed = observed_components(shared, &actual).await?;
                if observed != json!(super::component_store::identities(&expected)) {
                    shared.desktop.log(
                        "Running component identities differ; awaiting authenticated rollback.",
                    );
                    if tokio::time::Instant::now() >= deadline {
                        return Err(error("Component identity verification deadline expired"));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                let pid = actual["pid"]
                    .as_u64()
                    .and_then(|p| u32::try_from(p).ok())
                    .ok_or_else(|| error("Missing daemon PID"))?;
                let runtime = DaemonRuntime::locate().map_err(PlatformError::from)?;
                atomic_json(&directory.join(health_name), &json!({"schemaVersion":2,"operationId":id,"nonce":nonce,
                    "serverId":actual["serverId"],"daemonHome":home,"components":observed,
                    "guiProcess":process_identity(std::process::id()).await?,"daemonProcess":process_identity(pid).await?,
                    "daemonExecutable":super::component_store::canonical_path(runtime.executable).map_err(PlatformError::from)?,"runtimeProcesses":daemon.runtime_processes().await?})).await?;
                reported = health_name.into();
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error(
                "Component health/recovery deadline expired; retained journal requires recovery",
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn observed_components(shared: &Arc<Shared>, daemon: &Value) -> Result<Value, PlatformError> {
    let tailnet = paseo_tailnet::native_identity().map_err(PlatformError::from)?;
    let mut command = tokio::process::Command::new(helper_path()?);
    command
        .arg("--identity")
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| error("Helper identity query timed out"))??;
    if !output.status.success() || output.stdout.len() > LIMIT as usize {
        return Err(error("Selected helper identity query failed"));
    }
    let helper =
        super::component_store::strict_json(&output.stdout).map_err(PlatformError::from)?;
    if helper["schemaVersion"] != 1 || helper["id"] != "update-helper" {
        return Err(error("Selected helper identity response differs"));
    }
    Ok(
        json!({"app":{"version":shared.desktop.app_version,"sourceSha":option_env!("PASEO_BUILD_SOURCE_SHA").unwrap_or(""),
        "inputHash":option_env!("PASEO_COMPONENT_INPUT_HASH").unwrap_or(""),"abiVersion":1},
        "daemon":daemon["componentIdentity"],"tailnet":tailnet,"update-helper":helper["identity"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn exited_helper_releases_only_proven_pre_handoff_authority() {
        let identity = json!({"pid":123,"birthToken":"child"});
        for (group, phase, recovery, safe) in [
            ("daemon", None, false, true),
            ("none", None, false, true),
            ("daemon", Some("preparation"), false, true),
            ("gui", Some("waitingForParent"), false, true),
            ("none", Some("preparation"), false, false),
            ("daemon", Some("activating"), false, false),
            ("gui", Some("checking"), false, false),
            ("daemon", Some("preparation"), true, false),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let directory = std::fs::canonicalize(temporary.path())
                .unwrap()
                .join("operation");
            private_dir(&directory).await.unwrap();
            let operation = json!({"operationId":"one","restartGroup":group,
                "targetComponents":{"daemon":{"version":"2"}},"helperStarted":true,
                "phase":"draining","terminal":false,"recoveryRequired":false});
            atomic_json(&directory.join("operation.json"), &operation)
                .await
                .unwrap();
            if let Some(phase) = phase {
                atomic_json(
                    &directory.join("status.json"),
                    &json!({
                        "schemaVersion":2,"operationId":"one","restartGroup":group,
                        "expectedComponents":operation["targetComponents"],
                        "helperProcess":identity,"phase":phase,"recoveryRequired":recovery
                    }),
                )
                .await
                .unwrap();
            }
            retire_pre_handoff_helper(&directory, &identity)
                .await
                .unwrap();
            assert_eq!(
                read_json(&directory.join("operation.json")).await.unwrap()["helperStarted"],
                !safe
            );
            fail_operation(&directory, "Child exited").await.unwrap();
            let failed = read_json(&directory.join("operation.json")).await.unwrap();
            assert_eq!(failed["terminal"], safe, "{group} {phase:?}");
            assert_eq!(failed["recoveryRequired"], !safe, "{group} {phase:?}");
            if safe {
                assert_eq!(failed["outcome"], "failed");
            } else {
                assert_ne!(failed["outcome"], "committed");
                assert_ne!(failed["outcome"], "rolled_back");
            }
        }
    }
    #[test]
    fn request_is_pinned_and_has_no_remote_paths() {
        use base64::Engine as _;
        let mut args = json!({"operationId":"0f8b6c5e-2a71-4c5b-9a43-7f2a1d0c9e11","expectedRelease":{"version":"2.0.0","manifestSha512":base64::engine::general_purpose::STANDARD.encode([0u8;64])},"forceRunning":false,"idleTimeoutMs":300_000});
        assert!(Submit::parse(&args).is_ok());
        args["path"] = json!("/tmp/installer");
        assert!(Submit::parse(&args).is_err());
        args.as_object_mut().unwrap().remove("path");
        args["idleTimeoutMs"] = json!(0);
        assert!(Submit::parse(&args).is_err());
    }
    #[test]
    fn selected_components_are_part_of_idempotent_request() {
        use base64::Engine as _;
        let mut args = json!({"operationId":"0f8b6c5e-2a71-4c5b-9a43-7f2a1d0c9e11","expectedRelease":{"version":"2.0.0","manifestSha512":base64::engine::general_purpose::STANDARD.encode([0u8;64])},"idleTimeoutMs":100,"selectedComponents":["daemon"]});
        let daemon = Submit::parse(&args).unwrap();
        args["selectedComponents"] = json!(["update-helper"]);
        assert_ne!(daemon, Submit::parse(&args).unwrap());
        args["selectedComponents"] = json!(["daemon", "daemon"]);
        assert!(Submit::parse(&args).is_err());
        args["selectedComponents"] = json!([]);
        assert!(Submit::parse(&args).is_err());
    }
    #[test]
    fn helper_commit_requires_same_operation_and_observed_components() {
        let mut operation = json!({"operationId":"one","restartGroup":"daemon","targetComponents":{"app":{"version":"1"},"daemon":{"version":"2"}}});
        let mut helper = json!({"schemaVersion":2,"operationId":"one","restartGroup":"daemon","expectedComponents":operation["targetComponents"],"phase":"checking"});
        merge_helper(&mut operation, &helper).unwrap();
        assert_eq!(operation["terminal"], false);
        helper["operationId"] = json!("other");
        assert!(merge_helper(&mut operation, &helper).is_err());
        helper["operationId"] = json!("one");
        helper["phase"] = json!("committed");
        assert!(merge_helper(&mut operation, &helper).is_err());
        helper["observedComponents"] = helper["expectedComponents"].clone();
        merge_helper(&mut operation, &helper).unwrap();
        assert_eq!(operation["outcome"], "committed");
        assert_eq!(operation["terminal"], true);
    }
    #[tokio::test]
    async fn private_journal_preserves_host_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path())
            .unwrap()
            .join("operation");
        private_dir(&root).await.unwrap();
        let path = root.join("operation.json");
        atomic_json(&path, &json!({"serverId":"a","operationId":"one"}))
            .await
            .unwrap();
        assert_eq!(read_json(&path).await.unwrap()["serverId"], "a");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o077, 0);
        }
    }

    #[tokio::test]
    async fn interrupted_preparation_is_failed_once_and_remains_host_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let base = home.join("desktop-app-update");
        private_dir(&base).await.unwrap();
        let operations = base.join("operations");
        private_dir(&operations).await.unwrap();
        let id = "0f8b6c5e-2a71-4c5b-9a43-7f2a1d0c9e11";
        let directory = operations.join(id);
        private_dir(&directory).await.unwrap();
        atomic_json(
            &directory.join("operation.json"),
            &json!({
                "operationId":id,"serverId":"host","targetKind":"app","accepted":true,
                "ownerSession":"old-session","helperStarted":false,"phase":"preparation",
                "terminal":false,"outcome":null,"request":{}
            }),
        )
        .await
        .unwrap();
        let bridge = Bridge {
            session: "new-session".into(),
            address: String::new(),
            registration: Default::default(),
            admission: Default::default(),
        };
        let first = bridge.status(&home, "host", id).await.unwrap();
        assert_eq!(first["phase"], "failed");
        assert_eq!(first["terminal"], true);
        assert!(first.get("ownerSession").is_none());
        assert!(first.get("request").is_none());
        assert_eq!(bridge.status(&home, "host", id).await.unwrap(), first);
        assert!(bridge.status(&home, "other-host", id).await.is_err());
        assert!(!directory.join("request.json").exists());
    }

    #[test]
    fn activation_failure_does_not_claim_rollback() {
        let mut operation = json!({"operationId":"one","restartGroup":"gui","targetComponents":{"app":{"version":"2"}}});
        let mut helper = json!({"schemaVersion":2,"operationId":"one","restartGroup":"gui",
            "expectedComponents":operation["targetComponents"],"phase":"activating","message":"Activating","recoveryRequired":false});
        merge_helper(&mut operation, &helper).unwrap();
        assert!(operation["actionRequired"].is_null());
        assert_eq!(operation["terminal"], false);
        helper["phase"] = json!("failed");
        helper["recoveryRequired"] = json!(true);
        merge_helper(&mut operation, &helper).unwrap();
        assert_eq!(operation["outcome"], "failed");
        assert_eq!(operation["recoveryRequired"], true);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_journal_rejects_symlink_and_world_readable_inputs() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let file = home.join("journal.json");
        atomic_json(&file, &json!({"operationId":"one"}))
            .await
            .unwrap();
        let link = home.join("link.json");
        symlink(&file, &link).unwrap();
        assert!(read_json(&link).await.is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_json(&file).await.is_err());
    }
}
