//! Project scripts for a session's existing directory. The legacy setup route stays
//! readable without exposing worktree lifecycle actions.
use makepad_widgets::*;
use serde_json::{json, Value};

use super::host::{self, Connection};
use super::i18n::{l, t};
use super::kit::{clicked_in, set_alert, set_optional, WaButtonWidgetExt, WaButtonWidgetRefExt, WaRepeatWidgetExt, WaRepeatWidgetRefExt};
use super::rpc::{array_of, payload, require_success, str_of, Pending};
use super::scripts;
use crate::common::UiContext;
use paseo_gui_core::CoreEvent;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;
    use crate::theme::*;
    use crate::common::widgets::*;
    use crate::features::workspace::kit::*;

    // Read-only, selectable mono text, also used by schedule output.
    pub WaCodeBlock = <WaBareInput> {
        mono: true
        input = {
            padding: 12.
            is_read_only: true
            draw_text: { text_style: <PASEO_FONT_MONO> { line_spacing: 1.6 } }
        }
    }


    WaScriptRow = <WaRow> {
        controls = {
            toggle = <WaButtonGhost> {}
            view_log = <WaButtonGhost> { icon: "square-terminal" }
            open_service = <WaButtonGhost> { icon: "external-link" }
        }
    }

    pub WaSetupView = {{WaSetupView}} {
        width: Fill, height: Fill, flow: Overlay
        wa_root = <Surface> {
            width: Fill, height: Fill, flow: Overlay
            role: Surface0
            scroll = <ScrollYView> {
                width: Fill, height: Fill, flow: Down, spacing: 8.
                padding: 16.
                load_error = <WaAlert> {
                    visible: false
                    padding: { left: 24., right: 24., top: 16., bottom: 16. }
                    body = { actions = { load_retry = <WaButtonOutlineSmall> {} } }
                }
                empty = <View> {
                    width: Fill, height: 240., visible: false
                    align: { x: 0.5, y: 0.5 }
                    text = <WaMuted> { width: Fit }
                }
                scripts_section = <View> {
                    width: Fill, height: Fit, flow: Down, visible: false
                    padding: { top: 24. }
                    title = <WaSectionTitle> {}
                    card = <WaCard> {
                        scripts = <WaRepeat> { template: <WaScriptRow> {} }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Req {
    Scripts,
    Script,
}

#[derive(Live, LiveHook, Widget)]
pub struct WaSetupView {
    #[deref]
    view: View,
    #[rust]
    host_id: String,
    #[rust]
    workspace_id: String,
    #[rust]
    cwd: String,
    #[rust]
    started: bool,
    #[rust]
    last_status: Option<Connection>,
    #[rust]
    scripts: Vec<Value>,
    #[rust]
    error: Option<String>,
    #[rust]
    loading: bool,
    #[rust]
    busy: bool,
    #[rust]
    pending: Pending<Req>,
}


impl WaSetupView {
    /// The content root as a handle (the deref `View` is not `Clone`).
    fn root(&self) -> WidgetRef {
        self.view.widget(id!(wa_root))
    }

    pub fn set_workspace(&mut self, cx: &mut Cx, host_id: &str, workspace_id: &str, cwd: &str) {
        if self.host_id == host_id && self.workspace_id == workspace_id {
            return;
        }
        self.host_id = host_id.to_owned();
        self.workspace_id = workspace_id.to_owned();
        self.cwd = cwd.to_owned();
        self.started = false;
        self.loading = true;
        self.error = None;
        self.scripts.clear();
        self.view.redraw(cx);
    }

    fn load(&mut self, ctx: &mut UiContext) {
        self.started = true;
        self.loading = true;
        self.error = None;
        self.load_scripts(ctx);
    }

    fn load_scripts(&mut self, ctx: &UiContext) {
        self.pending.send(&ctx.core, &self.host_id, "workspace.script.list.request", json!({ "workspaceId": self.workspace_id }), Req::Scripts);
    }


    fn handle_core(&mut self, cx: &mut Cx, ctx: &mut UiContext) {
        if self.host_id.is_empty() {
            return;
        }
        host::learn(&ctx.events);
        let status = host::host(&ctx.state, &self.host_id).map(|h| h.status());
        if !self.started || (status == Some(Connection::Connected) && status != self.last_status) {
            self.load(ctx);
            self.view.redraw(cx);
        }
        self.last_status = status;
        let mut changed = false;
        for event in &ctx.events {
            if let CoreEvent::HostEvent { host_id, event } = event && *host_id == self.host_id && str_of(event, "type") == "script_status_update" {
                let body = payload(event);
                if str_of(body, "workspaceId") == self.workspace_id {
                    self.scripts = array_of(body, "scripts").to_vec();
                    changed = true;
                }
            }
        }
        for (req, result) in self.pending.take(&ctx.events) {
            changed = true;
            match (req, result) {
                (Req::Scripts, result) => {
                    self.loading = false;
                    match result.and_then(|body| require_success(&body).map(|_| body)) {
                        Ok(body) => self.scripts = array_of(&body, "scripts").to_vec(),
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                (Req::Script, result) => {
                    self.busy = false;
                    match result.and_then(|body| require_success(&body)) {
                        Ok(()) => self.load_scripts(ctx),
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
        }
        if changed {
            self.view.redraw(cx);
        }
    }


    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, ctx: &mut UiContext) {
        if self.view.wa_button(id!(load_retry)).clicked(actions) && !self.loading {
            self.load(ctx);
            self.view.redraw(cx);
        }
        let scripts_list = self.view.wa_repeat(id!(scripts));
        if let Some(index) = clicked_in(&scripts_list, id!(toggle), actions) && let Some(script) = self.scripts.get(index).cloned() && !self.busy {
            self.busy = true;
            self.error = None;
            scripts::toggle(&mut self.pending, &ctx.core, &self.host_id, &self.workspace_id, &script, Req::Script);
            self.view.redraw(cx);
        }
        if let Some(index) = clicked_in(&scripts_list, id!(view_log), actions) && let Some(effect) = self.scripts.get(index).and_then(|s| scripts::log_tab(&self.host_id, &self.workspace_id, &self.cwd, s)) {
            ctx.effects.push(effect);
        }
        if let Some(index) = clicked_in(&scripts_list, id!(open_service), actions) && let Some(script) = self.scripts.get(index) && let Err(error) = scripts::open_service(&mut ctx.effects, scripts::service_url(script)) {
            self.error = Some(error);
            self.view.redraw(cx);
        }
    }


    fn present(&mut self, cx: &mut Cx) {
        let view = self.root();
        let alert = view.widget(id!(load_error));
        alert.set_visible(cx, self.error.is_some());
        if let Some(error) = &self.error {
            set_alert(cx, &alert, error, "");
            let retry = view.wa_button(id!(load_retry));
            retry.set_text(cx, &t("common.actions.retry"));
            retry.set_enabled(cx, !self.loading);
        }
        let empty = view.widget(id!(empty));
        empty.set_visible(cx, !self.loading && self.error.is_none() && self.scripts.is_empty());
        empty.widget(id!(text)).set_text(cx, l("No scripts configured."));
        let section = view.widget(id!(scripts_section));
        section.set_visible(cx, !self.scripts.is_empty());
        section.widget(id!(title)).set_text(cx, &t("workspace.scripts.title"));
        let scripts_list = view.wa_repeat(id!(scripts));
        scripts_list.set_count(cx, self.scripts.len());
        let compact = crate::common::is_compact_layout(cx);
        for (index, script) in self.scripts.iter().enumerate() {
            let row = scripts_list.item(cx, index);
            // Keep script metadata above the actions on narrow panes; long localized
            // action labels can wrap without consuming the hint's available width.
            if compact {
                row.apply_over(cx, live! {
                    flow: Down, align: { y: 0.0 }
                    controls = { width: Fill, flow: RightWrap }
                });
            } else {
                row.apply_over(cx, live! {
                    flow: Right, align: { y: 0.5 }
                    controls = { width: Fit, flow: Right }
                });
            }
            row.widget(id!(body.label)).set_text(cx, str_of(script, "scriptName"));
            set_optional(cx, &row.widget(id!(body.hint)), &scripts::script_hint(script));
            let toggle = row.wa_button(id!(toggle));
            let running = scripts::is_running(script);
            let label = if running { t("workspace.scripts.actions.stop") } else { t("workspace.scripts.actions.run") };
            toggle.set_text(cx, &label);
            toggle.set_icon(cx, if running { "square" } else { "play" });
            toggle.set_enabled(cx, !self.busy);
            let view_log = row.wa_button(id!(view_log));
            view_log.set_visible(cx, !str_of(script, "terminalId").is_empty());
            view_log.set_text(cx, &t("workspace.scripts.actions.view"));
            let open = row.wa_button(id!(open_service));
            open.set_visible(cx, !scripts::service_url(script).is_empty());
            open.set_text(cx, &t("workspace.scripts.actions.openService"));
        }
    }
}

impl Widget for WaSetupView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(ctx) = scope.data.get_mut::<UiContext>() {
            self.handle_core(cx, ctx);
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event && let Some(ctx) = scope.data.get_mut::<UiContext>() {
            self.handle_actions(cx, actions, ctx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.present(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}

