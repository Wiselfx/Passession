//! In-context OS notification permission for the shell. Nothing prompts at launch: once a
//! host connects, a one-time explanation offers the OS prompt while it can still show, and a
//! notification that could not be shown because notifications are off raises one toast per
//! session with the action that fixes it. Settings › Notifications stays the place to change
//! it later; the Embedded Tailscale page explains its own notification.

use makepad_widgets::*;
use paseo_gui_platform::{
    PermissionSettings, PermissionState, PlatformError, PlatformRequestId, PlatformValue,
};
use serde_json::Value;

use crate::common::{UiContext, tr};
use crate::shell::commands::{self, ShellOverlay};
use crate::shell::{PaseoShell, overlays, platform};

/// Set once the explanation was answered, allowed or dismissed: it never shows again on this
/// install.
pub const ASKED_PREFERENCE: &str = "notificationPermissionAsked";

/// What the fix toast's action does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Show the OS prompt again.
    Allow,
    /// Open the app's notification settings.
    OpenSettings,
}

#[derive(Default)]
pub struct NotificationPermission {
    /// Status read before the explanation.
    ask_status: Option<PlatformRequestId>,
    /// The explanation was considered this session.
    ask_checked: bool,
    /// Status read after a notification failed.
    failure_status: Option<PlatformRequestId>,
    /// The fix toast showed this session.
    failure_shown: bool,
}

/// Whether asking shows the OS prompt (Android also re-prompts a plain denial).
fn can_prompt(state: PermissionState) -> bool {
    match state {
        PermissionState::NotDetermined => true,
        PermissionState::Denied => cfg!(target_os = "android"),
        _ => false,
    }
}

fn permission(result: &Result<PlatformValue, PlatformError>) -> Option<PermissionState> {
    match result {
        Ok(PlatformValue::Permission(state)) => Some(*state),
        _ => None,
    }
}

/// A host connected: read the permission once per session unless the explanation was
/// already answered on this install.
pub fn host_connected(shell: &mut PaseoShell, ui: &UiContext) {
    let state = &mut shell.data.notification_permission;
    if state.ask_checked || ui.preference(ASKED_PREFERENCE) == Some(&Value::Bool(true)) {
        return;
    }
    state.ask_checked = true;
    state.ask_status = platform::notification_permission_status();
}

/// A notification failed: read the permission to tell whether it was the cause.
pub fn notify_failed(shell: &mut PaseoShell) {
    let state = &mut shell.data.notification_permission;
    if state.failure_shown || state.failure_status.is_some() {
        return;
    }
    state.failure_status = platform::notification_permission_status();
}

/// Routes a platform completion; `true` when it belonged to this flow.
pub fn on_completed(
    shell: &mut PaseoShell,
    cx: &mut Cx,
    ui: &UiContext,
    request: PlatformRequestId,
    result: &Result<PlatformValue, PlatformError>,
) -> bool {
    let state = &mut shell.data.notification_permission;
    if state.ask_status == Some(request) {
        state.ask_status = None;
        if permission(result).is_some_and(can_prompt)
            && ui.preference(ASKED_PREFERENCE) != Some(&Value::Bool(true))
        {
            commands::confirm(
                shell,
                cx,
                &tr("systemPermissions.notifications.askTitle"),
                &tr("systemPermissions.notifications.askBody"),
                &tr("systemPermissions.notifications.allow"),
                false,
                ShellOverlay::NotificationAsk,
            );
        }
        return true;
    }
    if state.failure_status == Some(request) {
        state.failure_status = None;
        let Some(permission) = permission(result).filter(|p| !p.allowed()) else {
            // Allowed: the failure had another cause, which stays the platform's to report.
            return true;
        };
        state.failure_shown = true;
        let (message, fix) = if can_prompt(permission) {
            ("systemPermissions.notifications.failed", Some(Fix::Allow))
        } else if cfg!(target_os = "linux") {
            // Blocked on Linux: no notification server answers, nothing in-app can fix it.
            ("systemPermissions.notifications.noServer", None)
        } else {
            (
                "systemPermissions.notifications.failed",
                Some(Fix::OpenSettings),
            )
        };
        let action = fix.map(|fix| {
            let label = match fix {
                Fix::Allow => tr("systemPermissions.allow"),
                Fix::OpenSettings => tr("systemPermissions.openSettings"),
            };
            overlays::ToastAction { label, fix }
        });
        overlays::toast_with_action(&mut shell.data.overlays, cx, tr(message), action);
        return true;
    }
    false
}

/// The explanation was answered: it never shows again; Allow shows the OS prompt.
pub fn answered(shell: &mut PaseoShell, ui: &mut UiContext, allow: bool) {
    commands::set_preference(shell, ui, ASKED_PREFERENCE, Value::Bool(true));
    if allow {
        platform::request_notification_permission();
    }
}

/// The fix toast's action.
pub fn fix(fix: Fix) {
    match fix {
        Fix::Allow => {
            platform::request_notification_permission();
        }
        Fix::OpenSettings => {
            platform::open_permission_settings(PermissionSettings::Notifications);
        }
    }
}
