//! Historical commit files and lazy per-file patches. Generation and patch
//! revision reject replies from a previous refresh or visibility lifetime.

use super::host;
use super::json::{array, error_message, flag, object, text};
use super::store::GitStore;
use super::text::t;
use super::wire::{self, Failure};
use crate::common::{PanelTarget, UiContext};
use paseo_gui_core::{CoreError, CoreEvent, RequestId};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

const HOST_DISCONNECTED: &str = "workspace.git.diff.hostDisconnectedCached";

enum Pending {
    Status {
        generation: u64,
    },
    Commits {
        generation: u64,
    },
    CommitGet {
        generation: u64,
    },
    FileDiff {
        path: String,
        generation: u64,
        revision: u64,
    },
}

pub struct DiffController {
    pub host: String,
    pub cwd: String,
    pub sha: String,
    pub subject: Option<String>,
    active: bool,
    stale: bool,
    patch_loads_deferred: bool,
    pub status: Option<Value>,
    pub files: Vec<Value>,
    pub patches: HashMap<String, Value>,
    pub loading_patches: HashSet<String>,
    pub patch_errors: HashMap<String, String>,
    pub selected_path: String,
    pub loading: bool,
    pub error: Option<String>,
    generation: u64,
    pub patch_revision: u64,
    pending: HashMap<RequestId, Pending>,
    pub dirty: bool,
}

impl DiffController {
    pub fn new(target: &PanelTarget, _store: &GitStore) -> Self {
        let selected_path = target
            .data
            .get("focusPath")
            .or_else(|| target.data.get("path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let sha = target
            .data
            .get("sha")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| target.id.clone().unwrap_or_default());
        Self {
            host: target.server_id.clone(),
            cwd: target.cwd.clone(),
            sha,
            subject: None,
            active: true,
            stale: false,
            patch_loads_deferred: false,
            status: None,
            files: Vec::new(),
            patches: HashMap::new(),
            loading_patches: HashSet::new(),
            patch_errors: HashMap::new(),
            selected_path,
            loading: false,
            error: None,
            generation: 0,
            patch_revision: 0,
            pending: HashMap::new(),
            dirty: true,
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn dispose(&mut self, core: &paseo_gui_core::CoreHandle, _store: &mut GitStore) {
        self.generation += 1;
        for (request, _) in self.pending.drain() {
            core.cancel_request(&request);
        }
    }

    pub fn set_active(&mut self, ui: &UiContext, store: &mut GitStore, value: bool) {
        if value == self.active {
            return;
        }
        self.active = value;
        self.dirty = true;
        if !value {
            self.stale |= self.loading;
            self.patch_loads_deferred |= !self.loading_patches.is_empty();
            self.dispose(&ui.core, store);
            self.loading_patches.clear();
            self.loading = false;
            return;
        }
        let deferred = std::mem::take(&mut self.patch_loads_deferred);
        if std::mem::take(&mut self.stale) {
            self.refresh(ui, store, true);
        } else if deferred {
            self.patch_revision += 1;
        }
    }

    /// A resumed immutable commit keeps already loaded patches.
    pub fn refresh(&mut self, ui: &UiContext, store: &mut GitStore, resume: bool) {
        if !self.active {
            self.stale = true;
            return;
        }
        self.dispose(&ui.core, store);
        self.loading = true;
        self.error = None;
        self.stale = false;
        self.patch_revision += 1;
        if !resume {
            self.patches.clear();
            self.patch_errors.clear();
        }
        self.loading_patches.clear();
        self.dirty = true;
        let request = wire::send(
            &ui.core,
            &self.host,
            "checkout_status_request",
            json!({ "cwd": self.cwd }),
        );
        self.pending.insert(
            request,
            Pending::Status {
                generation: self.generation,
            },
        );
    }

    fn finish_commit(&mut self, ui: &UiContext, subject: Option<String>, files: Vec<Value>) {
        self.subject = subject.filter(|s| !s.is_empty());
        self.files = files;
        if self.files.is_empty() && !self.selected_path.is_empty() {
            self.files = vec![json!({ "path": self.selected_path })];
        }
        if !self.selected_path.is_empty() {
            let path = self.selected_path.clone();
            self.select_file(ui, &path);
        }
        self.loading = false;
        self.dirty = true;
    }

    pub fn select_file(&mut self, ui: &UiContext, path: &str) {
        self.selected_path = path.to_owned();
        self.dirty = true;
        if !self.active {
            self.patch_loads_deferred = true;
            return;
        }
        if self.patches.contains_key(path) || self.loading_patches.contains(path) {
            return;
        }
        self.fetch(ui, path);
    }

    /// Loads a file scrolled near the viewport without selecting it.
    pub fn ensure_patch(&mut self, ui: &UiContext, path: &str) {
        if self.patches.contains_key(path) || self.loading_patches.contains(path) {
            return;
        }
        if !self.active {
            self.patch_loads_deferred = true;
            return;
        }
        self.fetch(ui, path);
    }

    fn fetch(&mut self, ui: &UiContext, path: &str) {
        self.loading_patches.insert(path.to_owned());
        self.patch_errors.remove(path);
        self.dirty = true;
        let request = wire::send(
            &ui.core,
            &self.host,
            "checkout.commits.file_diff.request",
            json!({ "cwd": self.cwd, "sha": self.sha, "path": path }),
        );
        self.pending.insert(
            request,
            Pending::FileDiff {
                path: path.to_owned(),
                generation: self.generation,
                revision: self.patch_revision,
            },
        );
    }

    pub fn process(&mut self, ui: &UiContext, store: &mut GitStore) -> bool {
        for event in &ui.events {
            match event {
                CoreEvent::Connected { host_id, .. } if *host_id == self.host => {
                    self.refresh(ui, store, true);
                }
                CoreEvent::Disconnected { host_id, .. } if *host_id == self.host => {
                    self.dispose(&ui.core, store);
                    self.patch_revision += 1;
                    self.loading_patches.clear();
                    self.loading = false;
                    self.stale = true;
                    self.error = Some(t(HOST_DISCONNECTED));
                    self.dirty = true;
                }
                CoreEvent::Response {
                    request_id, result, ..
                } => {
                    if let Some(pending) = self.pending.remove(request_id) {
                        self.on_response(ui, pending, result);
                    }
                }
                _ => {}
            }
        }
        std::mem::take(&mut self.dirty)
    }

    fn on_response(&mut self, ui: &UiContext, pending: Pending, result: &Result<Value, CoreError>) {
        match pending {
            Pending::Status { generation } => {
                if generation != self.generation {
                    return;
                }
                self.dirty = true;
                let payload = wire::checkout_result(result).and_then(|payload| {
                    if let Some(message) = error_message(&payload) {
                        return Err(Failure::message(message));
                    }
                    if payload.get("success") == Some(&Value::Bool(false))
                        || payload.get("accepted") == Some(&Value::Bool(false))
                    {
                        return Err(Failure::message("The host did not accept this operation."));
                    }
                    Ok(payload)
                });
                let status = match payload {
                    Ok(status) => status,
                    Err(failure) => {
                        self.error = Some(failure.message);
                        self.loading = false;
                        return;
                    }
                };
                let is_git = flag(&status, "isGit");
                self.status = Some(status);
                if !is_git {
                    self.loading = false;
                    return;
                }
                let request = wire::send(
                    &ui.core,
                    &self.host,
                    "checkout.commits.list.request",
                    json!({ "cwd": self.cwd }),
                );
                self.pending
                    .insert(request, Pending::Commits { generation });
            }
            Pending::Commits { generation } => {
                if generation != self.generation {
                    return;
                }
                self.dirty = true;
                let payload = match wire::git_result(result) {
                    Ok(payload) => payload,
                    Err(failure) => {
                        self.error = Some(failure.message);
                        self.loading = false;
                        return;
                    }
                };
                if let Some(commit) = array(&payload, "commits")
                    .iter()
                    .find(|c| text(c, "sha") == self.sha)
                {
                    self.finish_commit(
                        ui,
                        Some(text(commit, "subject").to_owned()),
                        array(commit, "files").to_vec(),
                    );
                } else if host::supports(ui, &self.host, "gitLog") {
                    let request = wire::send(
                        &ui.core,
                        &self.host,
                        "checkout.commit.get.request",
                        json!({ "cwd": self.cwd, "sha": self.sha }),
                    );
                    self.pending
                        .insert(request, Pending::CommitGet { generation });
                } else {
                    self.finish_commit(ui, None, Vec::new());
                }
            }
            Pending::CommitGet { generation } => {
                if generation != self.generation {
                    return;
                }
                self.dirty = true;
                // Preserve focused-file navigation if commit metadata could not load.
                let (subject, files) = match wire::git_result(result) {
                    Ok(payload) => {
                        let commit = object(&payload, "commit");
                        (
                            Some(text(commit, "subject").to_owned()),
                            array(commit, "files").to_vec(),
                        )
                    }
                    Err(_) => (None, Vec::new()),
                };
                self.finish_commit(ui, subject, files);
            }
            Pending::FileDiff {
                path,
                generation,
                revision,
            } => {
                if generation != self.generation || revision != self.patch_revision {
                    return;
                }
                self.loading_patches.remove(&path);
                match wire::git_result(result) {
                    Ok(payload) => {
                        let file = object(&payload, "file");
                        let patch = if file.is_null() {
                            json!({ "path": path, "hunks": [], "status": "binary" })
                        } else {
                            file.clone()
                        };
                        self.patches.insert(path, patch);
                    }
                    Err(failure) => {
                        self.patch_errors.insert(path, failure.message);
                    }
                }
                self.dirty = true;
            }
        }
    }
}
