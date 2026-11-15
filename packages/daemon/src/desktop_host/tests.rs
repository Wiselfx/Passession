use super::*;
use tokio::io::AsyncReadExt;

#[test]
fn intake_cancels_current_and_queued_starts_before_dispatch() {
    let mut intake = Intake::default();
    let generation = intake.generation;
    let pending = intake.begin_start(generation).unwrap();
    intake.cancel();
    assert!(pending.is_cancelled());
    assert!(intake.begin_start(generation).is_err());
    assert!(
        !intake
            .begin_start(intake.generation)
            .unwrap()
            .is_cancelled()
    );
}
#[test]
fn persisted_desktop_marker_never_reconstructs_session_ownership() {
    let session = Session::default();
    let status=session.status_from(&json!({"localDaemon":"running","desktopManaged":true,"pid":777,"startedAt":"2026-01-01T00:00:00.000Z"}));
    assert_eq!(status["desktopManaged"], true);
    assert_eq!(status["ownedByDesktop"], false);
}
#[tokio::test]
async fn jsonl_errors_keep_correlation_and_continue_until_eof() {
    let (input, mut producer) = tokio::io::duplex(4096);
    let (output, mut consumer) = tokio::io::duplex(4096);
    producer
        .write_all(b"not json\n{\"id\":4,\"command\":\"status\"}\n")
        .await
        .unwrap();
    producer.shutdown().await.unwrap();
    let task = tokio::spawn(run_io(input, output));
    let mut contents = String::new();
    consumer.read_to_string(&mut contents).await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), 0);
    let replies: Vec<Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[0]["error"]["code"], "INVALID_REQUEST");
    assert_eq!(replies[1]["id"], 4);
    assert_eq!(
        replies[1]["error"]["message"],
        "Daemon helper has not been initialized."
    );
}
#[tokio::test]
async fn initializes_native_runtime_without_starting_a_process() {
    let directory = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let home = directory.path().join("isolated-home");
    let mut session = Session::default();
    let response = session.initialize(&json!({"protocolVersion":1,"capabilities":["component-runtime-v1"],
        "runtime":{"executable":executable,"cliShimPath":executable,"workingDirectory":executable.parent().unwrap()},
        "appVersion":"0.0.1","home":home,"settings":{"managed":false,"keepRunningAfterQuit":false}})).await.unwrap();
    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["daemon"]["version"], crate::VERSION);
    assert_eq!(session.home.as_deref(), Some(home.as_path()));
    assert!(session.start(&CancellationToken::new()).await.is_err());
    assert!(
        session
            .stop(&json!({"reason":"manual_ipc","pid":1,"startedAt":"unconfirmed"}))
            .await
            .is_err()
    );
    session.close(false).await.unwrap();
    assert!(!home.exists());
}

#[tokio::test]
async fn incompatible_initialize_never_establishes_ownership_or_runtime() {
    for args in [
        json!({"protocolVersion":2,"capabilities":["component-runtime-v1"]}),
        json!({"protocolVersion":1,"capabilities":[]}),
        json!({}),
    ] {
        let mut session = Session::default();
        assert!(
            session
                .initialize(&args)
                .await
                .unwrap_err()
                .to_string()
                .contains("DESKTOP_ABI_MISMATCH")
        );
        assert!(session.runtime.is_none());
        assert!(session.home.is_none());
        assert!(session.owned.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn runtime_selection_cannot_substitute_another_executable_or_unpaired_pin() {
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let mut runtime = Runtime {
        executable: executable.clone(),
        cli_shim_path: executable.clone(),
        working_directory: executable.parent().unwrap().to_owned(),
        identity: None,
        executable_sha256: Some("a".repeat(64)),
    };
    assert!(validate_runtime(&mut runtime).await.is_err());
    runtime.executable_sha256 = None;
    validate_runtime(&mut runtime).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let other = directory.path().join("other-runtime");
    std::fs::copy(&executable, &other).unwrap();
    runtime.executable = other.clone();
    runtime.cli_shim_path = other;
    runtime.working_directory = directory.path().to_owned();
    assert!(
        validate_runtime(&mut runtime)
            .await
            .unwrap_err()
            .to_string()
            .contains("exact selected host")
    );
}
#[tokio::test]
async fn logs_are_bounded_and_drop_truncated_first_line() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("daemon.log");
    let mut contents = String::from("first line\n");
    contents.push_str(&"x".repeat(1024 * 1024));
    contents.push_str("\nlast line\n");
    tokio::fs::write(&path, contents).await.unwrap();
    let logs = read_logs(&path).await.unwrap();
    assert_eq!(logs["contents"], "last line\n");
}

#[test]
fn authenticated_source_sha_is_preserved_without_inventing_one() {
    let session = Session::default();
    let status = session.status_from(
        &json!({"localDaemon":"running","daemonVersion":"2.0.0","sourceSha":"a".repeat(40)}),
    );
    assert_eq!(status["sourceSha"], "a".repeat(40));
    let unknown = session.status_from(&json!({"localDaemon":"running","daemonVersion":"2.0.0"}));
    assert!(unknown["sourceSha"].is_null());
}

#[tokio::test]
async fn app_update_commands_require_real_session_ownership_before_connecting() {
    let home = tempfile::tempdir().unwrap();
    let mut session = Session {
        home: Some(home.path().to_owned()),
        ..Session::default()
    };
    let request = json!({"operationId":"0f8b6c5e-2a71-4c5b-9a43-7f2a1d0c9e11","forceRunning":true,"idleTimeoutMs":1});
    assert!(
        session
            .dispatch("app_update_drain", &request, None)
            .await
            .is_err()
    );
    assert!(
        session
            .dispatch("app_update_drain_release", &request, None)
            .await
            .is_err()
    );
    assert!(!home.path().join("desktop-app-update").exists());
    assert!(!home.path().join("native-update-operation.json").exists());
}

#[tokio::test]
async fn update_close_allows_absent_daemon_but_rejects_keep_running() {
    let home = tempfile::tempdir().unwrap();
    let mut session = Session {
        home: Some(home.path().to_owned()),
        ..Session::default()
    };
    assert!(
        session
            .dispatch(
                "close",
                &json!({"requireOwnedStopped":true,"keepRunning":true}),
                None
            )
            .await
            .is_err()
    );
    assert!(!session.closing);
    session
        .dispatch(
            "close",
            &json!({"requireOwnedStopped":true,"keepRunning":false}),
            None,
        )
        .await
        .unwrap();
    assert!(session.closing);
}

#[tokio::test]
async fn failed_runtime_start_can_snapshot_and_close_only_its_host() {
    let mut session = Session::default();
    let before_initialize = session
        .dispatch("runtime_processes", &json!({}), None)
        .await
        .unwrap();
    assert_eq!(before_initialize.as_array().unwrap().len(), 1);
    assert_eq!(before_initialize[0]["process"]["pid"], std::process::id());
    let home = tempfile::tempdir().unwrap();
    session.home = Some(home.path().to_owned());
    let after_initialize = session
        .dispatch("runtime_processes", &json!({}), None)
        .await
        .unwrap();
    assert_eq!(before_initialize, after_initialize);
    assert!(session.owned.lock().unwrap().is_none());
    session
        .dispatch("close", &json!({"keepRunning":false}), None)
        .await
        .unwrap();
    assert!(session.closing);
    assert!(!home.path().join("paseo.pid").exists());
    let mut failed_initialize = Session::default();
    failed_initialize
        .dispatch("close", &json!({"keepRunning":false}), None)
        .await
        .unwrap();
    assert!(failed_initialize.closing);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn update_close_never_stops_an_unowned_live_process() {
    let home = tempfile::tempdir().unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
    let token = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap();
    let instance = json!({"pid":std::process::id(),"startedAt":chrono::Utc::now().to_rfc3339(),
        "hostname":"fixture","uid":unsafe { libc::geteuid() },"desktopManaged":true,
        "processStartIdentity":format!("linux:{token}")});
    let path = home.path().join("paseo.pid");
    let original = serde_json::to_vec(&instance).unwrap();
    crate::transport::storage::write_private(&path, &original)
        .await
        .unwrap();
    let mut session = Session {
        home: Some(home.path().to_owned()),
        ..Session::default()
    };
    assert!(
        session
            .dispatch("runtime_processes", &json!({}), None)
            .await
            .is_err()
    );
    let error = session
        .dispatch(
            "close",
            &json!({"requireOwnedStopped":true,"keepRunning":false}),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not owned"));
    assert!(!session.closing);
    assert_eq!(std::fs::read(path).unwrap(), original);
}
