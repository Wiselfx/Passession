pub(crate) mod input;
#[cfg(test)]
mod tests;

use crate::daemon_control::{self as control, DaemonInstance, StartOptions, StopOptions};
use anyhow::{Context as _, Result, bail, ensure};
use paseo_client::{ClientError, DaemonTarget};
use paseo_protocol::components::{ComponentIdentity, validate_identity, validate_sha256};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Runtime {
    pub executable: PathBuf,
    pub cli_shim_path: PathBuf,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub identity: Option<ComponentIdentity>,
    #[serde(default)]
    pub executable_sha256: Option<String>,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default)]
    managed: bool,
    #[serde(default)]
    keep_running_after_quit: bool,
}
#[derive(Default)]
struct Intake {
    generation: u64,
    pending: Option<CancellationToken>,
}
impl Intake {
    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(token) = &self.pending {
            token.cancel();
        }
    }
    fn begin_start(&mut self, generation: u64) -> Result<CancellationToken> {
        ensure!(
            self.generation == generation,
            "Desktop daemon start cancelled."
        );
        let token = CancellationToken::new();
        self.pending = Some(token.clone());
        Ok(token)
    }
}
struct Session {
    runtime: Option<Runtime>,
    home: Option<PathBuf>,
    settings: Settings,
    owned: Arc<Mutex<Option<DaemonInstance>>>,
    closing: bool,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            runtime: None,
            home: None,
            settings: Settings::default(),
            owned: Arc::new(Mutex::new(None)),
            closing: false,
        }
    }
}
fn failure(code: &str, message: impl Into<String>) -> anyhow::Error {
    ClientError::new(code, message).into()
}
const PROTOCOL_VERSION: u32 = 1;
const RUNTIME_CAPABILITY: &str = "component-runtime-v1";

fn initialize_protocol(args: &Value) -> Result<()> {
    ensure!(
        args["protocolVersion"].as_u64() == Some(u64::from(PROTOCOL_VERSION))
            && args["capabilities"].as_array().is_some_and(|capabilities| {
                capabilities.len() <= 16
                    && capabilities.iter().all(Value::is_string)
                    && capabilities.iter().any(|value| value == RUNTIME_CAPABILITY)
            }),
        "DESKTOP_ABI_MISMATCH: a compatible component-aware desktop host is required"
    );
    Ok(())
}

async fn validate_runtime(runtime: &mut Runtime) -> Result<()> {
    for (name, path) in [
        ("executable", &runtime.executable),
        ("cliShimPath", &runtime.cli_shim_path),
    ] {
        ensure!(path.is_absolute(), "Missing absolute runtime path: {name}");
        assert_executable(path).await?;
    }
    ensure!(
        runtime.working_directory.is_absolute()
            && tokio::fs::metadata(&runtime.working_directory)
                .await?
                .is_dir(),
        "Missing absolute runtime directory: workingDirectory"
    );
    runtime.executable = tokio::fs::canonicalize(&runtime.executable).await?;
    runtime.cli_shim_path = tokio::fs::canonicalize(&runtime.cli_shim_path).await?;
    runtime.working_directory = tokio::fs::canonicalize(&runtime.working_directory).await?;
    ensure!(
        runtime.executable == std::env::current_exe()?.canonicalize()?
            && runtime.cli_shim_path == runtime.executable,
        "Desktop runtime must use this exact selected host image for the daemon and CLI"
    );
    ensure!(
        runtime.executable.starts_with(&runtime.working_directory),
        "Desktop runtime working directory does not contain its image"
    );
    match (&runtime.identity, &runtime.executable_sha256) {
        (Some(identity), Some(expected)) => {
            validate_identity(identity)?;
            validate_sha256(expected)?;
            ensure!(
                identity.abi_version == PROTOCOL_VERSION
                    && identity.input_hash
                        == option_env!("PASEO_COMPONENT_INPUT_HASH").unwrap_or("")
                    && identity.version == crate::VERSION
                    && identity.source_sha == option_env!("PASEO_BUILD_SOURCE_SHA").unwrap_or(""),
                "DESKTOP_ABI_MISMATCH: selected daemon component does not match this host"
            );
            use sha2::{Digest, Sha256};
            let mut file = tokio::fs::File::open(&runtime.executable).await?;
            let mut hash = Sha256::new();
            let mut bytes = [0u8; 64 * 1024];
            loop {
                let count = file.read(&mut bytes).await?;
                if count == 0 {
                    break;
                }
                hash.update(&bytes[..count]);
            }
            ensure!(
                format!("{:x}", hash.finalize()) == *expected,
                "Desktop runtime executable digest changed"
            );
        }
        (None, None) => {}
        _ => bail!("Desktop runtime identity and executable digest must be supplied together"),
    }
    Ok(())
}
fn error_code(error: &anyhow::Error) -> &str {
    if let Some(error) = error.downcast_ref::<ClientError>() {
        return &error.code;
    }
    if let Some(error) = error.downcast_ref::<control::DaemonInstanceError>() {
        return error.code.as_str();
    }
    "DAEMON_HELPER_ERROR"
}
async fn assert_executable(path: &std::path::Path) -> Result<()> {
    let metadata = tokio::fs::metadata(path).await?;
    ensure!(
        metadata.is_file(),
        "Runtime path must identify a regular file: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Runtime path is not executable: {}", path.display()),
            )
            .into());
        }
    }
    Ok(())
}
impl Session {
    fn home(&self) -> Result<&std::path::Path> {
        self.home
            .as_deref()
            .context("Daemon helper has not been initialized.")
    }
    fn owns(&self, instance: &DaemonInstance) -> bool {
        self.owned
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|owned| control::same_daemon_instance(owned, instance))
    }
    fn confirmed(instance: &DaemonInstance, args: &Value) -> bool {
        args["pid"].as_u64() == Some(instance.pid as u64)
            && args["startedAt"].as_str() == Some(instance.started_at.as_str())
    }
    async fn initialize(&mut self, args: &Value) -> Result<Value> {
        ensure!(
            self.runtime.is_none(),
            "Daemon helper is already initialized."
        );
        initialize_protocol(args)?;
        let mut runtime: Runtime = serde_json::from_value(args["runtime"].clone())
            .context("Missing native runtime paths: executable, cliShimPath, workingDirectory")?;
        validate_runtime(&mut runtime).await?;
        let configured_home = std::env::var("PASEO_HOME").ok();
        let home = paseo_client::resolve_home(
            args["home"]
                .as_str()
                .filter(|v| !v.is_empty())
                .or(configured_home.as_deref()),
        )?;
        let settings = serde_json::from_value(args.get("settings").cloned().unwrap_or(json!({})))?;
        self.settings = settings;
        self.home = Some(home);
        self.runtime = Some(runtime);
        Ok(
            json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":[RUNTIME_CAPABILITY],
            "daemon":{"version":crate::VERSION,"sourceSha":option_env!("PASEO_BUILD_SOURCE_SHA").unwrap_or(""),
                "abiVersion":PROTOCOL_VERSION}}),
        )
    }
    fn status_from(&self, payload: &Value) -> Value {
        let state = match payload["localDaemon"].as_str() {
            Some("running") => "running",
            Some("not_ready") => "starting",
            _ => "stopped",
        };
        let owned = self.owned.lock().unwrap();
        let home = self
            .home
            .as_deref()
            .map(control::managed::desktop_protocol_path);
        json!({"serverId":payload["serverId"].as_str().unwrap_or(""),"status":state,"listen":payload["listen"],"hostname":if state=="running"{payload["hostname"].clone()}else{Value::Null},"pid":if state=="stopped"{Value::Null}else{payload["pid"].clone()},"home":home,"version":payload["daemonVersion"],"sourceSha":payload["sourceSha"],"componentIdentity":payload["componentIdentity"],"desktopManaged":payload["desktopManaged"]==true,"ownedByDesktop":owned.as_ref().is_some_and(|o|payload["pid"].as_u64()==Some(o.pid as u64)&&payload["startedAt"]==o.started_at),"startedAt":payload["startedAt"],"error":null,"connectedDaemon":payload["connectedDaemon"],"note":payload["note"]})
    }
    async fn status(&self) -> Result<Value> {
        let result = async {
            let home = self.home()?;
            if control::read_daemon_instance(home).await?.is_none() {
                return Ok(self.status_from(&json!({})));
            }
            let payload = crate::cli::system::observe_status(
                &DaemonTarget::Instance {
                    home: home.to_owned(),
                },
                &CancellationToken::new(),
            )
            .await?;
            Ok::<_, anyhow::Error>(self.status_from(&payload))
        }
        .await;
        Ok(match result {
            Ok(status) => status,
            Err(error) => {
                let mut status = self.status_from(&json!({}));
                status["status"] = json!("errored");
                status["error"] = json!(error.to_string());
                status
            }
        })
    }
    async fn stop(&mut self, args: &Value) -> Result<Value> {
        let instance = control::read_daemon_instance(self.home()?).await?;
        let explicit = instance.as_ref().is_some_and(|instance| {
            args["reason"] == "manual_ipc" && Self::confirmed(instance, args)
        });
        if (args.get("pid").is_some() || args.get("startedAt").is_some()) && !explicit {
            return Err(failure(
                "DAEMON_REPLACED",
                "Daemon changed since confirmation; inspect its current home, PID, and start time.",
            ));
        }
        if let Some(instance) = instance {
            let owned = self.owns(&instance);
            if owned || explicit {
                control::stop_daemon_instance(
                    self.home()?,
                    StopOptions {
                        instance: Some(instance),
                        timeout: Duration::from_secs(15),
                        ..Default::default()
                    },
                    &CancellationToken::new(),
                )
                .await?;
                if owned {
                    *self.owned.lock().unwrap() = None;
                }
            }
        }
        self.status().await
    }
    async fn start(&mut self, cancel: &CancellationToken) -> Result<Value> {
        ensure!(
            self.settings.managed,
            "Built-in daemon management is disabled."
        );
        ensure!(!cancel.is_cancelled(), "Desktop daemon start cancelled.");
        let status = self.status().await?;
        if status["status"] == "errored" {
            bail!(
                "{}",
                status["error"].as_str().unwrap_or("Daemon status failed")
            );
        }
        if matches!(status["status"].as_str(), Some("running" | "starting")) {
            // The selected runtime is pinned at initialize. App and daemon versions
            // are independent; a live owned daemon is never replaced by GUI skew.
            return Ok(status);
        }
        ensure!(!cancel.is_cancelled(), "Desktop daemon start cancelled.");
        validate_runtime(
            self.runtime
                .as_mut()
                .context("Daemon helper has not been initialized.")?,
        )
        .await?;
        let runtime = self
            .runtime
            .as_ref()
            .context("Daemon helper has not been initialized.")?;
        let owned = self.owned.clone();
        let result = control::start_daemon_instance(
            StartOptions {
                home: self.home()?.to_owned(),
                desktop_managed: true,
                timeout: Duration::from_secs(30),
                executable: Some(runtime.executable.clone()),
                working_directory: Some(runtime.working_directory.clone()),
                cli_shim_path: Some(runtime.cli_shim_path.clone()),
                on_acquired: Some(Arc::new(move |instance| {
                    *owned.lock().unwrap() = Some(instance);
                })),
                ..Default::default()
            },
            cancel,
        )
        .await;
        if let Err(error) = result
            && error_code(&error) != "DAEMON_NOT_READY"
        {
            return Err(error);
        }
        self.status().await
    }
    async fn restart(&mut self, args: &Value) -> Result<Value> {
        ensure!(
            self.settings.managed,
            "Built-in daemon management is disabled."
        );
        let instance = control::read_daemon_instance(self.home()?)
            .await?
            .ok_or_else(|| failure("DAEMON_NOT_RUNNING", "Daemon is not running."))?;
        if args.get("pid").is_some() || args.get("startedAt").is_some() {
            if !Self::confirmed(&instance, args) {
                return Err(failure(
                    "DAEMON_REPLACED",
                    "Daemon changed since confirmation.",
                ));
            }
        } else {
            ensure!(
                self.owns(&instance),
                "Confirm the current daemon PID and startedAt before restarting a daemon not owned by this app session."
            );
        }
        crate::cli::system::restart_confirmed(
            &DaemonTarget::Instance {
                home: self.home()?.to_owned(),
            },
            Some(instance),
            "desktop_restart",
            Duration::from_secs(45),
            &CancellationToken::new(),
        )
        .await?;
        self.status().await
    }
    async fn app_update_drain(&self, args: &Value, release: bool) -> Result<Value> {
        let home = self.home()?;
        let instance = control::read_daemon_instance(home)
            .await?
            .context("Owned desktop daemon is not running")?;
        ensure!(
            instance.desktop_managed && self.owns(&instance),
            "This desktop session does not own the daemon"
        );
        let server = instance
            .server_id
            .as_deref()
            .context("Desktop daemon has no server identity")?;
        let owner = crate::daemon_admin::desktop_update::owner(home, server).await?;
        let id =
            crate::daemon_admin::update_operation_id(args["operationId"].as_str().unwrap_or(""))?;
        let mut options = crate::client::ConnectOptions::new(DaemonTarget::Instance {
            home: home.to_owned(),
        });
        options.instance = Some(instance.clone());
        let client = crate::client::connect(options, CancellationToken::new()).await?;
        let result = async {
            ensure!(
                client.server_info()["serverId"] == server,
                "Desktop drain server identity changed"
            );
            let (kind, response) = if release {
                (
                    "daemon.appUpdate.drain.release.request",
                    "daemon.appUpdate.drain.release.response",
                )
            } else {
                (
                    "daemon.appUpdate.drain.request",
                    "daemon.appUpdate.drain.response",
                )
            };
            let mut request = json!({"type":kind,"operationId":id,"ownerToken":owner.token});
            let timeout = if release {
                Duration::from_secs(10)
            } else {
                let policy = crate::daemon_admin::desktop_update::drain_policy(args)?;
                request["forceRunning"] = json!(policy.0);
                request["idleTimeoutMs"] = json!(policy.1);
                Duration::from_millis(policy.1) + Duration::from_secs(10)
            };
            let result = client.request(request, response, timeout).await?;
            let current = control::read_daemon_instance(home)
                .await?
                .context("Desktop daemon stopped during drain")?;
            ensure!(
                control::same_daemon_instance(&instance, &current),
                "Desktop daemon changed during drain"
            );
            Ok(result)
        }
        .await;
        client.close().await;
        result
    }
    async fn runtime_processes(&self) -> Result<Value> {
        let Some(home) = self.home.as_deref() else {
            return tokio::task::spawn_blocking(control::managed::desktop_host_process).await?;
        };
        let Some(instance) = control::read_daemon_instance(home).await? else {
            ensure!(
                self.owned.lock().unwrap().is_none(),
                "Owned desktop supervisor disappeared; its runtime cohort requires recovery"
            );
            return tokio::task::spawn_blocking(control::managed::desktop_host_process).await?;
        };
        ensure!(
            instance.desktop_managed && self.owns(&instance),
            "This desktop session does not own the daemon"
        );
        let runtime = self
            .runtime
            .as_ref()
            .context("Desktop runtime is not initialized")?;
        let mut options = crate::client::ConnectOptions::new(DaemonTarget::Instance {
            home: home.to_owned(),
        });
        options.instance = Some(instance.clone());
        let status = match crate::client::connect(options, CancellationToken::new()).await {
            Ok(client) => {
                let status = client.daemon_status(Duration::from_secs(10)).await;
                client.close().await;
                status
            }
            Err(error) => Err(error),
        };
        let status = match status {
            Ok(status) => status,
            Err(_) => {
                let expected = runtime.executable.clone();
                return tokio::task::spawn_blocking(move || {
                    control::managed::desktop_partial_runtime_processes(&instance, &expected)
                })
                .await?;
            }
        };
        ensure!(
            status["serverId"].as_str() == instance.server_id.as_deref(),
            "Desktop runtime server changed"
        );
        let worker = u32::try_from(status["pid"].as_u64().context("Missing worker PID")?)?;
        let expected = runtime.executable.clone();
        tokio::task::spawn_blocking(move || {
            control::managed::desktop_runtime_processes(&instance, worker, &expected)
        })
        .await?
    }
    async fn close(&mut self, keep: bool) -> Result<()> {
        if self.closing {
            return Ok(());
        }
        self.closing = true;
        if self.runtime.is_some()
            && !keep
            && let Err(error) = self.stop(&json!({"reason":"quit"})).await
        {
            self.closing = false;
            return Err(error);
        }
        Ok(())
    }
    async fn close_for_update(&mut self) -> Result<()> {
        if let Some(instance) = control::read_daemon_instance(self.home()?).await? {
            ensure!(
                instance.desktop_managed && self.owns(&instance),
                "APP handoff refused: a live daemon is not owned by this desktop session"
            );
        }
        self.close(false).await?;
        if control::read_daemon_instance(self.home()?).await?.is_some() {
            self.closing = false;
            bail!("APP handoff refused: a daemon is still running in this home");
        }
        Ok(())
    }
    async fn dispatch(
        &mut self,
        command: &str,
        args: &Value,
        start_cancel: Option<&CancellationToken>,
    ) -> Result<Value> {
        if command == "initialize" {
            return self.initialize(args).await;
        }
        if command == "runtime_processes" {
            return self.runtime_processes().await;
        }
        if command == "close" && self.home.is_none() && args["requireOwnedStopped"] != true {
            self.close(false).await?;
            return Ok(Value::Null);
        }
        self.home()?;
        if let Some(settings) = args.get("settings") {
            self.settings = serde_json::from_value(settings.clone())?;
        }
        match command {
            "status" => self.status().await,
            "start" => {
                self.start(start_cancel.context("Missing start cancellation")?)
                    .await
            }
            "stop" => self.stop(args).await,
            "restart" => self.restart(args).await,
            "app_update_drain" => self.app_update_drain(args, false).await,
            "app_update_drain_release" => self.app_update_drain(args, true).await,
            "logs" => read_logs(&control::daemon_log_path(self.home()?)).await,
            "close" => {
                if args["requireOwnedStopped"] == true {
                    ensure!(
                        args["keepRunning"] != true,
                        "APP handoff cannot keep the daemon running"
                    );
                    self.close_for_update().await?;
                    return Ok(Value::Null);
                }
                self.close(
                    args["keepRunning"]
                        .as_bool()
                        .unwrap_or(self.settings.keep_running_after_quit),
                )
                .await?;
                Ok(Value::Null)
            }
            _ => bail!("Unknown daemon helper command: {command}"),
        }
    }
}
async fn read_logs(path: &std::path::Path) -> Result<Value> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"logPath":path,"contents":""}));
        }
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata().await?.len();
    let offset = size.saturating_sub(1024 * 1024);
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut bytes = Vec::with_capacity(size.min(1024 * 1024) as usize);
    file.take(1024 * 1024).read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if offset > 0 {
        text.find('\n').map(|n| &text[n + 1..]).unwrap_or(&text)
    } else {
        &text
    };
    let lines: Vec<_> = text.split('\n').collect();
    Ok(json!({"logPath":path,"contents":lines[lines.len().saturating_sub(100)..].join("\n")}))
}
/// Dedicated JSONL process. Input reception cancels starts before queued dispatch.
pub async fn run() -> Result<i32> {
    run_io(input::Input::open()?, tokio::io::stdout()).await
}
async fn run_io<R, W>(input: R, mut output: W) -> Result<i32>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let intake = Arc::new(Mutex::new(Intake::default()));
    let reader_intake = intake.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let reader = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(input).lines();
        let result = async {
            while let Some(line) = lines.next_line().await? {
                let request = serde_json::from_str::<Value>(&line);
                let generation = {
                    let mut state = reader_intake.lock().unwrap();
                    let generation = state.generation;
                    if request
                        .as_ref()
                        .ok()
                        .and_then(|r| r["command"].as_str())
                        .is_some_and(|c| matches!(c, "stop" | "close"))
                    {
                        state.cancel();
                    }
                    generation
                };
                if tx.send((request, generation)).await.is_err() {
                    break;
                }
            }
            Ok::<_, std::io::Error>(())
        }
        .await;
        reader_intake.lock().unwrap().cancel();
        result
    });
    let mut session = Session::default();
    let mut output_error = None;
    while let Some((request, generation)) = rx.recv().await {
        let mut close = false;
        let reply = match request {
            Err(_) => {
                json!({"id":null,"error":{"code":"INVALID_REQUEST","message":"Malformed JSON request."}})
            }
            Ok(request) => {
                let command = request["command"].as_str().unwrap_or("");
                let token = if command == "start" {
                    intake.lock().unwrap().begin_start(generation).map(Some)
                } else {
                    Ok(None)
                };
                let result = match token {
                    Ok(token) => {
                        session
                            .dispatch(
                                command,
                                request.get("args").unwrap_or(&Value::Null),
                                token.as_ref(),
                            )
                            .await
                    }
                    Err(error) => Err(error),
                };
                if command == "start" {
                    intake.lock().unwrap().pending = None;
                }
                match result {
                    Ok(result) => {
                        close = command == "close";
                        json!({"id":request.get("id").unwrap_or(&Value::Null),"result":result})
                    }
                    Err(error) => {
                        json!({"id":request.get("id").unwrap_or(&Value::Null),"error":{"code":error_code(&error),"message":error.to_string()}})
                    }
                }
            }
        };
        let mut bytes = serde_json::to_vec(&reply)?;
        bytes.push(b'\n');
        if let Err(error) = async {
            output.write_all(&bytes).await?;
            output.flush().await
        }
        .await
        {
            output_error = Some(error);
            break;
        }
        if close {
            break;
        }
    }
    intake.lock().unwrap().cancel();
    reader.abort();
    let reader_result = reader.await;
    let cleanup = session
        .close(session.settings.keep_running_after_quit)
        .await;
    if let Err(error) = cleanup {
        eprintln!("Daemon cleanup failed: {error}");
        return Ok(1);
    }
    if let Some(error) = output_error {
        return Err(error.into());
    }
    if let Ok(Err(error)) = reader_result {
        return Err(error.into());
    }
    Ok(0)
}
