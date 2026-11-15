//! Binds the action policy to one checkout (original git_actions.dart
//! `CheckoutGitActions`): runtime state, labels, icons and handlers for the
//! repository header split button.

use super::host;
use super::json::text;
use super::model::format_base_ref_label;
use super::overlay::{MenuEntry, MenuItem};
use super::policy::{
    ActionRuntime, ActionStatus, ActionsContext, GitAction, GitActions, action_label_keys,
    actions_input_from, build_git_actions, forge_presentation, normalize_forge,
};
use super::store::{GitStore, ReviewPlan, pr_status};
use super::text::{msg, t, tr_forge};
use crate::common::{PanelTarget, TabPlacement, UiContext, UiEffect};
use makepad_widgets::Cx;
use serde_json::{Map, Value, json};

/// Git actions menu values of the last failure's controls.
const FAILURE_RETRY: &str = "failure-retry";
const FAILURE_COPY: &str = "failure-copy";
const FAILURE_DISMISS: &str = "failure-dismiss";
/// Menu value suffix of Create PR without its review.
const DIRECT_SUFFIX: &str = ":direct";

/// One checkout's identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checkout {
    pub host: String,
    pub workspace_id: String,
    pub cwd: String,
}

impl Checkout {
    fn status<'a>(&self, store: &'a GitStore) -> Option<&'a Value> {
        store
            .checkout(&self.host, &self.cwd)
            .and_then(|s| s.status.as_ref())
    }

    fn pr_payload<'a>(&self, store: &'a GitStore) -> Option<&'a Value> {
        store
            .checkout(&self.host, &self.cwd)
            .and_then(|s| s.pr_payload.as_ref())
    }

    fn repo_root(&self, store: &GitStore) -> String {
        self.status(store)
            .map(|s| text(s, "repoRoot").to_owned())
            .unwrap_or_default()
    }

    /// `preferences.changesShipDefault[repoRoot]`, `merge` or `pr`.
    pub fn ship_default(&self, ui: &UiContext, store: &GitStore) -> &'static str {
        let root = self.repo_root(store);
        if root.is_empty() {
            return "pr";
        }
        match host::preference(ui, "changesShipDefault")
            .get(&root)
            .and_then(Value::as_str)
        {
            Some("merge") => "merge",
            _ => "pr",
        }
    }

    fn persist_ship_default(&self, ui: &mut UiContext, store: &GitStore, next: &str) {
        let root = self.repo_root(store);
        if root.is_empty() {
            return;
        }
        let mut map = match host::preference(ui, "changesShipDefault") {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        };
        map.insert(root, json!(next));
        ui.push(UiEffect::SetPreference {
            key: "changesShipDefault".into(),
            value: Value::Object(map),
        });
    }

    fn auto_merge_actions_enabled(&self, ui: &UiContext) -> bool {
        host::supports(ui, &self.host, "checkoutForgeSetAutoMerge")
            || host::supports(ui, &self.host, "checkoutGithubSetAutoMerge")
    }

    fn forge(&self, store: &GitStore) -> String {
        normalize_forge(self.pr_payload(store).map(|p| text(p, "forge")))
    }

    fn merge_request(&self, store: &GitStore) -> bool {
        forge_presentation(Some(&self.forge(store))).is_merge_request()
    }

    fn has_pull_request(&self, store: &GitStore) -> bool {
        !text(pr_status(store.checkout(&self.host, &self.cwd)), "url").is_empty()
    }

    /// Original `build`.
    pub fn build(&self, ui: &UiContext, store: &GitStore) -> GitActions {
        let state = store.checkout(&self.host, &self.cwd);
        let base = t("workspace.git.diff.base");
        let context = ActionsContext {
            status_loading: state.is_some_and(|s| s.status_loading),
            auto_merge_actions_enabled: self.auto_merge_actions_enabled(ui),
            ship_default: self.ship_default(ui, store),
            base_fallback_label: &base,
        };
        let has_pull_request = self.has_pull_request(store);
        let mut runtime = |id: &'static str, actions_disabled: bool| {
            let store_id = if id == "pr" { "create-pr" } else { id };
            let status = store.action_status(&self.host, &self.cwd, store_id);
            ActionRuntime {
                disabled: actions_disabled || status == ActionStatus::Pending,
                status: if id == "pr" && has_pull_request {
                    ActionStatus::Idle
                } else {
                    status
                },
            }
        };
        let input = actions_input_from(
            state.and_then(|s| s.status.as_ref()),
            state.and_then(|s| s.pr_payload.as_ref()),
            &context,
            &mut runtime,
        );
        build_git_actions(&input)
    }

    /// Original `label`: the copy for the action's current status.
    pub fn label(&self, store: &GitStore, action: &GitAction) -> String {
        let base = format_base_ref_label(
            self.status(store).map(|s| text(s, "baseRef")).unwrap_or(""),
            &t("workspace.git.diff.base"),
        );
        let [idle, pending, success] =
            action_label_keys(action.id, self.has_pull_request(store), &base);
        let chosen = match action.status {
            ActionStatus::Idle => idle,
            ActionStatus::Pending => pending,
            ActionStatus::Success => success,
        };
        tr_forge(&chosen.key, self.merge_request(store), &chosen.args)
    }

    /// Original `icon`.
    pub fn icon(action: &GitAction) -> &'static str {
        match action.id {
            "commit" => "git-commit-horizontal",
            "pull" => "download",
            "push" => "upload",
            "pull-and-push" => "arrow-down-up",
            "merge-branch" => "git-merge",
            "merge-from-base" => "refresh-ccw",
            _ => "git-pull-request",
        }
    }

    fn forge_label(&self, store: &GitStore, key: &str) -> String {
        tr_forge(key, self.merge_request(store), &[])
    }

    /// Original `resolveAutoMergeActionsRpc`.
    fn auto_merge_rpc(&self, ui: &UiContext) -> Option<&'static str> {
        if host::supports(ui, &self.host, "checkoutForgeSetAutoMerge") {
            Some("checkout.forge.set_auto_merge.request")
        } else if host::supports(ui, &self.host, "checkoutGithubSetAutoMerge") {
            Some("checkout.github.set_auto_merge.request")
        } else {
            None
        }
    }

    /// Runs a git actions menu answer: the last failure's Retry / Copy /
    /// Dismiss, an action without its review (`…:direct`), or an action.
    pub fn run_menu(
        &self,
        cx: &mut Cx,
        ui: &mut UiContext,
        store: &mut GitStore,
        actions: &GitActions,
        id: &str,
    ) {
        match id {
            FAILURE_RETRY => store.retry_failure(cx, &self.host, &self.cwd),
            FAILURE_COPY => {
                if let Some(failure) = store.last_failure(&self.host, &self.cwd) {
                    let text = failure.message.clone();
                    ui.push(UiEffect::CopyText {
                        text,
                        toast: Some(t("workspace.git.actions.failure.copied")),
                    });
                }
            }
            FAILURE_DISMISS => store.dismiss_failure(cx, &self.host, &self.cwd),
            _ => {
                let (action_id, review) = if id == "pr:direct" {
                    ("pr", false)
                } else {
                    (id, true)
                };
                if let Some(action) = actions
                    .primary
                    .iter()
                    .chain(actions.secondary.iter())
                    .find(|a| a.id == action_id)
                {
                    self.run_with(cx, ui, store, action, review);
                }
            }
        }
    }

    /// Menu entries besides the actions: the last failure (title with the
    /// host's message, Retry, Copy, Dismiss) first, and Create PR without review.
    pub fn menu_extras(
        &self,
        ui: &UiContext,
        store: &GitStore,
        actions: &GitActions,
    ) -> (Vec<MenuEntry>, Vec<MenuEntry>) {
        let mut failure_entries = Vec::new();
        if let Some(failure) = store.last_failure(&self.host, &self.cwd) {
            let pending = store.action_status(&self.host, &self.cwd, &failure.action_id)
                == ActionStatus::Pending;
            failure_entries.push(MenuEntry::Item(
                MenuItem::new(
                    "failure-message",
                    format!("{}: {}", failure.title, failure.message),
                )
                .icon("circle-alert")
                .enabled(false),
            ));
            failure_entries.push(MenuEntry::Item(
                MenuItem::new(FAILURE_RETRY, t("workspace.git.actions.failure.retry"))
                    .icon("rotate-cw")
                    .enabled(failure.retryable && !pending && host::connected(ui, &self.host)),
            ));
            failure_entries.push(MenuEntry::Item(
                MenuItem::new(FAILURE_COPY, t("workspace.git.actions.failure.copy")).icon("copy"),
            ));
            failure_entries.push(MenuEntry::Item(
                MenuItem::new(FAILURE_DISMISS, t("workspace.git.actions.failure.dismiss"))
                    .icon("x"),
            ));
        }
        let mut direct = Vec::new();
        let all = || actions.primary.iter().chain(actions.secondary.iter());
        let ready = |id: &str| {
            all().any(|a| {
                a.id == id
                    && !a.disabled
                    && a.unavailable.is_none()
                    && a.status == ActionStatus::Idle
            })
        };
        if ready("pr") && !self.has_pull_request(store) {
            direct.push(MenuEntry::Item(
                MenuItem::new(
                    format!("pr{DIRECT_SUFFIX}"),
                    self.forge_label(store, "workspace.git.actions.review.prDirect"),
                )
                .icon("git-pull-request"),
            ));
        }
        (failure_entries, direct)
    }

    /// Commit opens its file-selection tab; Create PR opens its review.
    pub fn run(&self, cx: &mut Cx, ui: &mut UiContext, store: &mut GitStore, action: &GitAction) {
        self.run_with(cx, ui, store, action, true)
    }

    fn run_with(
        &self,
        cx: &mut Cx,
        ui: &mut UiContext,
        store: &mut GitStore,
        action: &GitAction,
        review: bool,
    ) {
        if action.disabled {
            return;
        }
        if let Some(unavailable) = &action.unavailable {
            ui.toast(msg(unavailable), false);
            return;
        }
        let base_ref = self
            .status(store)
            .map(|s| text(s, "baseRef").to_owned())
            .unwrap_or_default();
        let (host, cwd) = (self.host.clone(), self.cwd.clone());
        let success = |store: &GitStore, key: &str| Some(self.forge_label(store, key));
        let plan = |success_label| ReviewPlan {
            host: host.clone(),
            cwd: cwd.clone(),
            success_label,
        };
        match action.id {
            "commit" => {
                let target = PanelTarget::new("git_commit", &host, &self.workspace_id, &cwd);
                ui.push(UiEffect::OpenTab {
                    target,
                    placement: TabPlacement::Main,
                });
            }
            "pull" => {
                let label = success(store, "workspace.git.actions.pull.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "pull",
                    vec![("checkout_pull_request", json!({}))],
                    "workspace.git.actions.toasts.failedPull",
                    label,
                );
            }
            "push" => {
                let label = success(store, "workspace.git.actions.push.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "push",
                    vec![("checkout_push_request", json!({}))],
                    "workspace.git.actions.toasts.failedPush",
                    label,
                );
            }
            "pull-and-push" => {
                let label = success(store, "workspace.git.actions.pullAndPush.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "pull-and-push",
                    vec![
                        ("checkout_pull_request", json!({})),
                        ("checkout_push_request", json!({})),
                    ],
                    "workspace.git.actions.toasts.failedPullAndPush",
                    label,
                );
            }
            "pr" => {
                let url = text(pr_status(store.checkout(&host, &cwd)), "url").to_owned();
                if !url.is_empty() {
                    ui.push(UiEffect::OpenExternal { url });
                    return;
                }
                self.persist_ship_default(ui, store, "pr");
                let label = success(store, "workspace.git.actions.createPr.success");
                if review {
                    let branch = self
                        .status(store)
                        .map(|s| text(s, "currentBranch").to_owned())
                        .unwrap_or_default();
                    store.review_pr(cx, ui, plan(label), base_ref, branch);
                } else {
                    store.run_action(
                        cx,
                        &host,
                        &cwd,
                        "create-pr",
                        vec![("checkout_pr_create_request", json!({}))],
                        "workspace.git.actions.toasts.failedCreatePr",
                        label,
                    );
                }
            }
            "disable-pr-auto-merge" => {
                let Some(rpc) = self.auto_merge_rpc(ui) else {
                    ui.toast(t("workspace.git.actions.toasts.autoMergeUnsupported"), true);
                    return;
                };
                let label = success(store, "workspace.git.actions.autoMerge.disabled");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "disable-pr-auto-merge",
                    vec![(rpc, json!({ "enabled": false }))],
                    "workspace.git.actions.toasts.failedDisableAutoMerge",
                    label,
                );
            }
            "merge-branch" => {
                if base_ref.is_empty() {
                    ui.toast(t("workspace.git.actions.toasts.baseRefUnavailable"), true);
                    return;
                }
                self.persist_ship_default(ui, store, "merge");
                let label = success(store, "workspace.git.actions.mergeBranch.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "merge-branch",
                    vec![("checkout_merge_request", json!({ "baseRef": base_ref, "strategy": "merge", "requireCleanTarget": true }))],
                    "workspace.git.actions.toasts.failedMerge",
                    label,
                );
            }
            "merge-from-base" => {
                if base_ref.is_empty() {
                    ui.toast(t("workspace.git.actions.toasts.baseRefUnavailable"), true);
                    return;
                }
                let label = success(store, "workspace.git.actions.mergeFromBase.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    "merge-from-base",
                    vec![(
                        "checkout_merge_from_base_request",
                        json!({ "baseRef": base_ref, "requireCleanTarget": true }),
                    )],
                    "workspace.git.actions.toasts.failedMergeFromBase",
                    label,
                );
            }
            id if id.starts_with("merge-pr-") => {
                let method = &id["merge-pr-".len()..];
                self.persist_ship_default(ui, store, "pr");
                let label = success(store, "workspace.git.actions.mergePr.success");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    action.id,
                    vec![(
                        "checkout_pr_merge_request",
                        json!({ "mergeMethod": method }),
                    )],
                    "workspace.git.actions.toasts.failedMergePr",
                    label,
                );
            }
            id if id.starts_with("enable-pr-auto-merge-") => {
                let method = &id["enable-pr-auto-merge-".len()..];
                self.persist_ship_default(ui, store, "pr");
                let Some(rpc) = self.auto_merge_rpc(ui) else {
                    ui.toast(t("workspace.git.actions.toasts.autoMergeUnsupported"), true);
                    return;
                };
                let label = success(store, "workspace.git.actions.autoMerge.enabled");
                store.run_action(
                    cx,
                    &host,
                    &cwd,
                    action.id,
                    vec![(rpc, json!({ "enabled": true, "mergeMethod": method }))],
                    "workspace.git.actions.toasts.failedEnableAutoMerge",
                    label,
                );
            }
            _ => {}
        }
    }
}

/// Original `gitActionMenuEntries`: separators before group starters (never
/// first); unavailable actions muted; a pending or succeeded row disabled.
pub fn menu_entries(
    checkout: &Checkout,
    store: &GitStore,
    actions: &[GitAction],
) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        if action.starts_group && index > 0 {
            entries.push(MenuEntry::Separator);
        }
        entries.push(MenuEntry::Item(
            MenuItem::new(action.id, checkout.label(store, action))
                .icon(Checkout::icon(action))
                .enabled(!action.disabled && action.status == ActionStatus::Idle)
                .muted(action.unavailable.is_some()),
        ));
    }
    entries
}
