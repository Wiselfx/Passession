use super::{diagnostics, updater};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, io::Write};

fn release() -> (Value, Value) {
    let artifact = "Paseo-Daemon-2.0.0-linux-x64.tar.gz";
    let url = format!("https://github.com/example/paseo/releases/download/v2.0.0/{artifact}");
    (
        json!({"schemaVersion":1,"channel":"stable","version":"2.0.0","sourceSha":"a".repeat(40),
            "targets":{"linux-x64":{"components":{"daemon":{"id":"daemon","target":"linux-x64",
                "identity":{"version":"2.0.0","sourceSha":"a".repeat(40),"inputHash":"b".repeat(64),"abiVersion":1},
                "artifact":{"url":url,"originTag":"v2.0.0","format":"tar.gz","size":123,"sha256":"c".repeat(64)},
                "executable":"paseo","executableSha256":"d".repeat(64),"requires":{}}},
                "packages":[],"atomicGroups":[]}}}),
        json!({"tag_name":"v2.0.0","author":{"login":"github-actions[bot]"},"assets":[{"name":artifact,"size":123,"browser_download_url":url,"uploader":{"login":"github-actions[bot]"}}]}),
    )
}
fn selected(manifest: &Value, release: &Value) -> anyhow::Result<updater::Release> {
    updater::validate_component_release(
        &serde_json::from_value(manifest.clone())?, release, "example/paseo",
        "stable", "linux", "x64", STANDARD.encode([1u8; 64]),
    )
}
fn admitted(manifest: &Value, release: &Value) -> bool {
    let Ok(selected) = selected(manifest, release) else { return false };
    updater::require_origin_component(&selected, &serde_json::from_value(manifest.clone()).unwrap(),
        release, "example/paseo").is_ok()
}
#[test]
fn update_admission_binds_component_origin_target_repository_size_and_digest() {
    let (manifest, published) = release();
    assert!(admitted(&manifest, &published));
    for (pointer, value) in [
        ("/version", json!("3.0.0")),
        ("/sourceSha", json!("not-provenance")),
        ("/targets/linux-x64/components/daemon/target", json!("windows-x64")),
        ("/targets/linux-x64/components/daemon/identity/abiVersion", json!(2)),
        ("/targets/linux-x64/components/daemon/artifact/size", json!(124)),
        ("/targets/linux-x64/components/daemon/artifact/sha256", json!("bad")),
        ("/targets/linux-x64/components/daemon/artifact/originTag", json!("v3.0.0")),
        ("/targets/linux-x64/components/daemon/artifact/format", json!("binary")),
    ] {
        let mut changed = manifest.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(!admitted(&changed, &published), "{pointer}");
    }
    let mut redirected = published.clone();
    redirected["assets"][0]["browser_download_url"] = json!(
        "https://github.com/other/paseo/releases/download/v2.0.0/Paseo-Daemon-2.0.0-linux-x64.tar.gz"
    );
    assert!(!admitted(&manifest, &redirected));
    let mut duplicate = published.clone();
    let extra = duplicate["assets"][0].clone();
    duplicate["assets"].as_array_mut().unwrap().push(extra);
    assert!(!admitted(&manifest, &duplicate));
}

#[test]
fn reused_daemon_preserves_original_identity_and_requires_exact_origin_ref() {
    let (original, published) = release();
    let mut collection = original.clone();
    collection["version"] = json!("3.0.0");
    collection["sourceSha"] = json!("e".repeat(40));
    let newer = json!({"tag_name":"v3.0.0","assets":[]});
    let selected = selected(&collection, &newer).unwrap();
    assert_eq!(selected.version, "2.0.0");
    assert_eq!(selected.identity().version, "3.0.0");
    let origin = serde_json::from_value(original.clone()).unwrap();
    updater::require_origin_component(&selected, &origin, &published, "example/paseo").unwrap();
    for pointer in [
        "/targets/linux-x64/components/daemon/identity/inputHash",
        "/targets/linux-x64/components/daemon/executableSha256",
        "/targets/linux-x64/components/daemon/artifact/sha256",
    ] {
        let mut wrong = original.clone();
        *wrong.pointer_mut(pointer).unwrap() = json!("f".repeat(64));
        assert!(updater::require_origin_component(&selected, &serde_json::from_value(wrong).unwrap(),
            &published, "example/paseo").is_err(), "{pointer}");
    }
    assert!(updater::require_origin_component(&selected, &serde_json::from_value(collection).unwrap(),
        &newer, "example/paseo").is_err());
}

#[test]
fn checked_release_pin_rejects_same_collection_changed_manifest() {
    let (manifest, published) = release();
    let selected = selected(&manifest, &published).unwrap();
    let pin = selected.identity();
    assert!(pin.validate().is_ok());
    assert!(selected.require_identity(&pin).is_ok());
    let mut changed = pin.clone();
    changed.manifest_sha512 = STANDARD.encode([2u8; 64]);
    assert!(selected.require_identity(&changed).is_err());
    changed = pin.clone();
    changed.version = "2.0.1".into();
    assert!(selected.require_identity(&changed).is_err());
    assert!(serde_json::from_value::<updater::ReleaseIdentity>(
        json!({"version":"2.0.0","manifestSha512":pin.manifest_sha512,"url":"https://other/runtime"})).is_err());
}

#[test]
fn collection_publication_binds_publisher_source_workflow_and_inventory_bytes() {
    use sha2::{Digest, Sha256};
    let (manifest, mut published) = release();
    let inventory = serde_json::from_value(manifest.clone()).unwrap();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    published["target_commitish"] = manifest["sourceSha"].clone();
    let asset = json!({"size":bytes.len(),"uploader":{"login":"github-actions[bot]"}});
    let provenance = json!({"schemaVersion":2,"version":"2.0.0","sourceSha":"a".repeat(40),
        "channel":"stable","workflow":".github/workflows/desktop-release.yml","runId":123,
        "inventory":{"name":"components-stable.json","size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}});
    assert_eq!(updater::validate_publication(&inventory, &published, &asset, &asset,
        &provenance, &bytes, "example/paseo").unwrap(), 123);
    for (pointer, value) in [
        ("/sourceSha", json!("e".repeat(40))),
        ("/workflow", json!("other.yml")),
        ("/inventory/sha256", json!("f".repeat(64))),
        ("/runId", json!(0)),
    ] {
        let mut changed = provenance.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(updater::validate_publication(&inventory, &published, &asset, &asset,
            &changed, &bytes, "example/paseo").is_err(), "{pointer}");
    }
    let mut wrong = asset.clone();
    wrong["uploader"]["login"] = json!("untrusted");
    assert!(updater::validate_publication(&inventory, &published, &wrong, &asset,
        &provenance, &bytes, "example/paseo").is_err());
    published["target_commitish"] = json!("main");
    assert!(updater::validate_publication(&inventory, &published, &asset, &asset,
        &provenance, &bytes, "example/paseo").is_err());
}

#[test]
fn original_component_requires_exact_target_proof_and_archive_hash() {
    let (manifest, published) = release();
    let inventory = serde_json::from_value(manifest.clone()).unwrap();
    let component = selected(&manifest, &published).unwrap();
    let proof = json!({"schemaVersion":2,"workflow":".github/workflows/desktop-release.yml",
        "sha":manifest["sourceSha"],"sourceTag":"v2.0.0","version":"2.0.0","channel":"stable",
        "runId":"123","target":"linux-x64","components":manifest["targets"]["linux-x64"]["components"],
        "packages":[],"files":[{"name":"Paseo-Daemon-2.0.0-linux-x64.tar.gz","size":123,"sha256":"c".repeat(64)}]});
    assert_eq!(updater::validate_target_proof(&inventory, &component, &proof).unwrap(), 123);
    let mut platform_tag = proof.clone();
    platform_tag["sourceTag"] = json!("refs/tags/desktop-linux-v2.0.0");
    assert_eq!(updater::validate_target_proof(&inventory, &component, &platform_tag).unwrap(), 123);
    for (pointer, value) in [
        ("/sha", json!("e".repeat(40))),
        ("/sourceTag", json!("v3.0.0")),
        ("/target", json!("windows-x64")),
        ("/components/daemon/identity/inputHash", json!("e".repeat(64))),
        ("/files/0/sha256", json!("f".repeat(64))),
        ("/files/0/size", json!(124)),
    ] {
        let mut changed = proof.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(updater::validate_target_proof(&inventory, &component, &changed).is_err(), "{pointer}");
    }
    let mut duplicate = proof.clone();
    let file = duplicate["files"][0].clone();
    duplicate["files"].as_array_mut().unwrap().push(file);
    assert!(updater::validate_target_proof(&inventory, &component, &duplicate).is_err());
}
#[test]
fn standalone_marker_rejects_desktop_package_manager_and_mismatched_assets() {
    let marker = json!({"schemaVersion":1,"runtime":"rust","appId":"sh.paseo.daemon","installationKind":"standalone-archive","version":"2.0.0","sourceSha":"a".repeat(40),"platform":"linux","arch":"x64"});
    assert!(updater::validate_marker(&marker, "2.0.0", &"a".repeat(40), "linux", "x64").is_ok());
    for kind in ["app", "npm", "package-manager"] {
        let mut other = marker.clone();
        other["installationKind"] = json!(kind);
        assert!(
            updater::validate_marker(&other, "2.0.0", &"a".repeat(40), "linux", "x64").is_err()
        );
    }
    assert!(updater::validate_marker(&marker, "2.0.1", &"a".repeat(40), "linux", "x64").is_err());
}
#[test]
fn tar_runtime_rejects_symlinks_without_touching_outside_staging() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_mode(0o777);
    header.set_size(0);
    tar.append_link(&mut header, "escape", "../outside")
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "tar.gz").is_err());
    assert!(!temp.path().join("outside").exists());
}
#[test]
fn zip_runtime_rejects_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file("../outside", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"not outside").unwrap();
    zip.finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "zip").is_err());
    assert!(!temp.path().join("outside").exists());
}
#[test]
fn archive_extraction_preserves_executable_and_runtime_assets() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    for (name, content, mode) in [
        ("paseo", b"binary".as_slice(), 0o755),
        ("lib/runtime", b"library".as_slice(), 0o644),
        ("skills/rule", b"skill".as_slice(), 0o644),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode);
        header.set_size(content.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, content).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    let root = temp.path().join("runtime");
    updater::extract(&archive, &root, "tar.gz").unwrap();
    assert_eq!(fs::read(root.join("skills/rule")).unwrap(), b"skill");
    assert_eq!(fs::read(root.join("lib/runtime")).unwrap(), b"library");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(root.join("paseo"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
}
#[tokio::test]
async fn diagnostics_tail_is_bounded_and_redacts_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("daemon.log");
    fs::write(
        &path,
        (0..200).map(|i| format!("line {i}\n")).collect::<String>(),
    )
    .unwrap();
    let tail = diagnostics::tail(&path).await.unwrap();
    assert_eq!(tail.lines().count(), 80);
    assert!(tail.contains("line 199"));
    assert!(!tail.contains("line 119"));
    let text = diagnostics::redact(
        "paseo://pairing-secret https://relay.internal/?token=abc&key=xyz {\"password\":\"hidden\"} authorization=secret",
        &["https://relay.internal/"],
    );
    for secret in [
        "pairing-secret",
        "relay.internal",
        "abc",
        "xyz",
        "hidden",
        "authorization=secret",
    ] {
        assert!(!text.contains(secret), "{secret}");
    }
}

#[test]
fn tar_runtime_rejects_duplicate_asset_paths() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    for content in [b"first".as_slice(), b"second".as_slice()] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(content.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "paseo", content).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "tar.gz").is_err());
}
