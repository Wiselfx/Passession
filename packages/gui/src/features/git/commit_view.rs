//! Session-directory commit form. The file list observes summaries only;
//! committing sends an explicit selection and never includes unrelated index entries.
use super::chrome::{ButtonSize, ButtonVariant, button, button_height, palette, ui as ui_px};
use super::host;
use super::json::{array, flag, text};
use super::paint::{Font, GitPaint, Hits, rect};
use super::store::GitStore;
use super::text::{t, tr};
use super::wire;
use crate::common::{PanelTarget, UiContext};
use makepad_widgets::scroll_bars::TouchScroll;
use makepad_widgets::*;
use paseo_gui_core::{CoreEvent, RequestId};
use serde_json::{Value, json};
use std::collections::HashSet;

pub const FEATURE: &str = "gitSelectedCommit";

#[derive(Clone, Debug, PartialEq)]
enum Action {
    File(String),
    All,
    Refresh,
    Commit,
}

#[derive(Default)]
struct Selection {
    files: Vec<Value>,
    selected: HashSet<String>,
    initialized: bool,
}

impl Selection {
    fn replace(&mut self, files: Vec<Value>) {
        let present: HashSet<&str> = files.iter().map(|file| text(file, "path")).collect();
        self.selected.retain(|path| present.contains(path.as_str()));
        // Only the first snapshot selects all. New files arriving while the user
        // writes a message must not silently enter that pending commit.
        if !self.initialized {
            self.selected.extend(present.into_iter().map(str::to_owned));
            self.initialized = true;
        }
        self.files = files;
    }

    fn paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter_map(|file| {
                let path = text(file, "path");
                self.selected.contains(path).then(|| path.to_owned())
            })
            .collect()
    }
}

pub struct CommitView {
    target: PanelTarget,
    selection: Selection,
    message: String,
    observation: Option<RequestId>,
    commit: Option<RequestId>,
    active: bool,
    loaded: bool,
    failed: Option<String>,
    commit_failure: Option<String>,
    feedback: Option<String>,
    uncertain: bool,
    refresh_requested: bool,
    input_initialized: bool,
    clear_input: bool,
    seen_invalidation: u64,
    hits: Hits<Action>,
    pressed: Option<Action>,
    hovered: Option<Action>,
    list: Rect,
    offset: f64,
    max_scroll: f64,
    touch_scroll: TouchScroll,
    pub icons_pending: bool,
    pub spinning: bool,
}

impl CommitView {
    pub fn new(cx: &mut Cx, ui: &UiContext, store: &mut GitStore, target: PanelTarget) -> Self {
        store.acquire_checkout(cx, ui, &target.server_id, &target.cwd);
        Self {
            seen_invalidation: store.invalidation(&target.server_id, &target.cwd),
            target,
            selection: Selection::default(),
            message: String::new(),
            observation: None,
            commit: None,
            active: false,
            loaded: false,
            commit_failure: None,
            failed: None,
            feedback: None,
            uncertain: false,
            refresh_requested: true,
            input_initialized: false,
            clear_input: false,
            hits: Hits::default(),
            pressed: None,
            hovered: None,
            list: Rect::default(),
            offset: 0.0,
            max_scroll: 0.0,
            touch_scroll: TouchScroll::default(),
            icons_pending: false,
            spinning: false,
        }
    }

    pub fn dispose(&mut self, store: &mut GitStore) {
        if let Some(id) = self.observation.take()
            && let Some(core) = store.core()
        {
            core.cancel_request(&id);
        }
        // A submitted mutation continues on the host; closing a tab is not cancel.
        store.release_checkout(&self.target.server_id, &self.target.cwd);
    }

    fn available(&self, ui: &UiContext) -> bool {
        host::connected(ui, &self.target.server_id)
            && host::supports(ui, &self.target.server_id, FEATURE)
    }

    fn observe(&mut self, ui: &UiContext) {
        if let Some(id) = self.observation.take() {
            ui.core.cancel_request(&id);
        }
        self.loaded = false;
        self.failed = None;
        self.refresh_requested = false;
        self.observation = Some(ui.core.observe(
            &self.target.server_id,
            json!({
                "type":"subscribe_checkout_diff_request", "cwd":self.target.cwd,
                "compare":{"mode":"uncommitted", "detail":"summary", "ignoreWhitespace":false}
            }),
            "subscribe_checkout_diff_response",
        ));
    }

    fn snapshot(&mut self, result: Result<Value, wire::Failure>) {
        self.loaded = true;
        match result {
            Ok(payload) => {
                self.failed = None;
                self.selection.replace(array(&payload, "files").to_vec());
            }
            Err(error) => self.failed = Some(error.message),
        }
    }

    pub fn process(
        &mut self,
        cx: &mut Cx,
        ui: &UiContext,
        store: &mut GitStore,
        visible: bool,
    ) -> bool {
        let mut changed = self.active != visible;
        if self.active != visible {
            self.active = visible;
            self.refresh_requested = true;
            if !visible {
                if let Some(id) = self.observation.take() {
                    ui.core.cancel_request(&id);
                }
                self.touch_scroll.reset();
            }
        }
        for event in &ui.events {
            match event {
                CoreEvent::Response {
                    request_id, result, ..
                } if self.observation.as_ref() == Some(request_id) => {
                    self.snapshot(wire::git_result(result));
                    changed = true;
                }
                CoreEvent::Response {
                    request_id, result, ..
                } if self.commit.as_ref() == Some(request_id) => {
                    self.commit = None;
                    match wire::git_result(result) {
                        Ok(_) => {
                            self.message.clear();
                            self.clear_input = true;
                            self.selection.selected.clear();
                            self.feedback = Some(t("workspace.git.commit.success"));
                            self.failed = None;
                        }
                        Err(error) => {
                            self.commit_failure = Some(error.message);
                            // Transport failure can follow a committed mutation. Require
                            // an explicit refresh/inspection, never an automatic replay.
                            self.uncertain = match result {
                                Err(error) => error.outcome_unknown,
                                Ok(payload) => paseo_gui_core::response_error(payload, "commit")
                                    .is_some_and(|error| error.outcome_unknown),
                            };
                        }
                    }
                    store.invalidate(cx, &self.target.server_id, &self.target.cwd);
                    self.refresh_requested = true;
                    changed = true;
                }
                CoreEvent::HostEvent { host_id, event } if *host_id == self.target.server_id => {
                    let payload = &event["payload"];
                    let observed = self.observation.as_ref().is_some_and(|id| {
                        array(event, "observations")
                            .iter()
                            .any(|v| v.as_str() == Some(id.as_str()))
                            || payload["observationId"].as_str() == Some(id.as_str())
                    });
                    if observed {
                        if text(event, "type") == "subscription.error" {
                            self.snapshot(Err(wire::Failure::message(text(payload, "error"))));
                        } else {
                            self.snapshot(wire::git_result(&Ok(payload.clone())));
                        }
                        changed = true;
                    }
                }
                CoreEvent::Disconnected { host_id, .. } if *host_id == self.target.server_id => {
                    self.refresh_requested = true;
                    self.loaded = false;
                    changed = true;
                }
                CoreEvent::Connected { host_id, .. } if *host_id == self.target.server_id => {
                    self.refresh_requested = true;
                    changed = true;
                }
                _ => {}
            }
        }
        let invalidation = store.invalidation(&self.target.server_id, &self.target.cwd);
        if invalidation != self.seen_invalidation {
            self.seen_invalidation = invalidation;
            self.refresh_requested = true;
        }
        if visible && self.available(ui) && self.refresh_requested && self.commit.is_none() {
            self.observe(ui);
            changed = true;
        }
        changed
    }

    fn can_commit(&self, ui: &UiContext) -> bool {
        self.available(ui)
            && self.loaded
            && self.failed.is_none()
            && self.commit_failure.is_none()
            && !self.uncertain
            && self.commit.is_none()
            && !self.selection.selected.is_empty()
            && !self.message.trim().is_empty()
    }

    fn activate(&mut self, ui: &UiContext, action: Action) {
        if self.commit.is_some() {
            return;
        }
        match action {
            Action::File(path) => {
                if !self.selection.selected.remove(&path) {
                    self.selection.selected.insert(path);
                }
            }
            Action::All => {
                if self.selection.selected.len() == self.selection.files.len() {
                    self.selection.selected.clear();
                } else {
                    self.selection.selected = self
                        .selection
                        .files
                        .iter()
                        .map(|file| text(file, "path").to_owned())
                        .collect();
                }
            }
            Action::Refresh => {
                self.uncertain = false;
                self.commit_failure = None;
                self.refresh_requested = true;
                if self.available(ui) {
                    self.observe(ui);
                }
                self.feedback = None;
            }
            Action::Commit if self.can_commit(ui) => {
                self.feedback = None;
                self.commit = Some(wire::send(
                    &ui.core,
                    &self.target.server_id,
                    "checkout.commit_selected.request",
                    json!({
                        "cwd": self.target.cwd, "paths": self.selection.paths(), "message": self.message,
                    }),
                ));
            }
            Action::Commit => {}
        }
    }

    pub fn handle_input(&mut self, cx: &mut Cx, event: &Event, input: &mut TextInput) -> bool {
        input.set_is_read_only(cx, self.commit.is_some());
        let uid = input.widget_uid();
        let actions = cx.capture_actions(|cx| input.handle_event(cx, event, &mut Scope::empty()));
        for action in &actions {
            if let Some(action) = action.as_widget_action()
                && action.widget_uid == uid
                && let Some(TextInputAction::Changed(value)) =
                    action.action.downcast_ref::<TextInputAction>()
            {
                self.message.clone_from(value);
                self.feedback = None;
                return true;
            }
        }
        false
    }

    pub fn handle_pointer(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        area: Area,
        ui: &UiContext,
    ) -> bool {
        let hit = event.hits(cx, area);
        if let Some(delta) = self.touch_scroll.handle_event(cx, event, &hit) {
            self.pressed = None;
            if self.list.contains(self.touch_scroll.origin()) {
                return self
                    .touch_scroll
                    .apply_delta(&mut self.offset, delta, self.max_scroll);
            }
            self.touch_scroll.reset();
        }
        match hit {
            Hit::FingerScroll(e) if self.list.contains(e.abs) => {
                return self.touch_scroll.apply_delta(
                    &mut self.offset,
                    e.scroll.y,
                    self.max_scroll,
                );
            }
            Hit::FingerDown(e) => self.pressed = self.hits.find(e.abs).map(|(_, action)| action),
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let next = self.hits.find(e.abs).map(|(_, action)| action);
                if next != self.hovered {
                    self.hovered = next;
                    return true;
                }
            }
            Hit::FingerHoverOut(_) => return self.hovered.take().is_some(),
            Hit::FingerUp(e) => {
                let pressed = self.pressed.take();
                if e.is_over
                    && !e.has_long_press_occurred
                    && (!e.device.is_touch() || e.was_tap())
                    && let Some((_, action)) = self.hits.find(e.abs)
                    && pressed.as_ref() == Some(&action)
                {
                    self.activate(ui, action);
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    pub fn hide_input(&mut self, cx: &mut Cx, input: &mut TextInput) {
        if cx.has_key_focus(input.area()) {
            cx.revert_key_focus();
        }
        self.touch_scroll.reset();
    }

    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        ui: &UiContext,
        paint: &mut GitPaint,
        input: &mut TextInput,
        area: Rect,
        compact: bool,
    ) {
        self.hits.clear();
        self.icons_pending = false;
        self.spinning = self.commit.is_some() || (self.available(ui) && !self.loaded);
        let colors = palette();
        let font = ui_px(13.0);
        let line = paint.measure(cx, Font::Regular, font, "Ag").y.max(18.0);
        let row = (line + 14.0).max(if compact { 44.0 } else { 30.0 });
        let pad = 12.0;
        let width = (area.size.x - pad * 2.0).max(1.0);
        let left = area.pos.x + pad;
        let mut y = area.pos.y + 6.0;
        paint.text_fit(
            cx,
            Font::Regular,
            font,
            colors.muted,
            dvec2(left, y),
            &self.target.cwd,
            width,
        );
        y += line + 8.0;
        let refresh = rect(area.pos.x + area.size.x - pad - row, y, row, row);
        paint.icon(cx, "refresh-cw", 16.0, colors.muted, refresh.center());
        if self.commit.is_none() {
            self.hits.push(refresh, Action::Refresh);
        }
        let all = rect(left, y, (width - row - 4.0).max(0.0), row);
        let all_selected = !self.selection.files.is_empty()
            && self.selection.selected.len() == self.selection.files.len();
        paint.icon(
            cx,
            if all_selected {
                "square-check"
            } else {
                "square"
            },
            16.0,
            colors.foreground,
            dvec2(left + 10.0, all.center().y),
        );
        let count = tr(
            "workspace.git.commit.files",
            &[
                ("selected", self.selection.selected.len().to_string()),
                ("total", self.selection.files.len().to_string()),
            ],
        );
        paint.text_centered(
            cx,
            Font::Regular,
            font,
            colors.foreground,
            left + 28.0,
            y,
            row,
            &count,
            (all.size.x - 28.0).max(0.0),
        );
        if self.loaded && self.commit.is_none() {
            self.hits.push(all, Action::All);
        }
        y += row + 4.0;
        let button_h = button_height(ButtonSize::Sm, compact).max(row);
        let footer_h =
            (area.size.y * 0.37).clamp(line * 3.0 + button_h + 30.0, line * 6.0 + button_h + 30.0);
        let list_h = (area.pos.y + area.size.y - footer_h - y).max(0.0);
        self.list = rect(left, y, width, list_h);
        self.max_scroll = (row * self.selection.files.len() as f64 - list_h).max(0.0);
        self.offset = self.offset.clamp(0.0, self.max_scroll);
        paint.begin_clip(cx, self.list);
        let show_files = self.available(ui)
            && self.loaded
            && self.failed.is_none()
            && self.commit_failure.is_none();
        if show_files {
            for (index, file) in self.selection.files.iter().enumerate() {
                let band = rect(left, y + index as f64 * row - self.offset, width, row);
                if band.pos.y + row < y || band.pos.y > y + list_h {
                    continue;
                }
                let path = text(file, "path");
                let action = Action::File(path.to_owned());
                if self.hovered.as_ref() == Some(&action) {
                    paint.fill(cx, band, colors.interaction_highlight);
                }
                paint.icon(
                    cx,
                    if self.selection.selected.contains(path) {
                        "square-check"
                    } else {
                        "square"
                    },
                    16.0,
                    colors.foreground,
                    dvec2(left + 10.0, band.center().y),
                );
                let status = if flag(file, "isDeleted") {
                    "D"
                } else if !text(file, "oldPath").is_empty() {
                    "R"
                } else if flag(file, "isNew") {
                    "A"
                } else {
                    "M"
                };
                paint.text_centered(
                    cx,
                    Font::Regular,
                    font,
                    colors.muted,
                    left + 29.0,
                    band.pos.y,
                    row,
                    status,
                    18.0,
                );
                paint.text_centered(
                    cx,
                    Font::Regular,
                    font,
                    colors.foreground,
                    left + 51.0,
                    band.pos.y,
                    row,
                    path,
                    (width - 55.0).max(0.0),
                );
                if self.commit.is_none() {
                    self.hits.push_clipped(band, self.list, action);
                }
            }
        }
        let state = if !host::connected(ui, &self.target.server_id) {
            Some(t("workspace.git.commit.disconnected"))
        } else if !host::supports(ui, &self.target.server_id, FEATURE) {
            Some(t("workspace.git.commit.unsupported"))
        } else if let Some(error) = &self.commit_failure {
            Some(error.clone())
        } else if let Some(error) = &self.failed {
            Some(error.clone())
        } else if !self.loaded {
            Some(t("workspace.git.commit.loading"))
        } else if self.selection.files.is_empty() {
            Some(t("workspace.git.commit.empty"))
        } else {
            None
        };
        if let Some(state) = state {
            paint.text_wrapped(
                cx,
                Font::Regular,
                font,
                colors.muted,
                dvec2(left, y + 8.0),
                &state,
                width,
            );
        }
        paint.end_clip(cx);
        let bottom = area.pos.y + area.size.y - 10.0;
        let message_label_y = (y + list_h + 8.0)
            .min(bottom - button_h - line * 2.0 - 16.0)
            .max(y);
        paint.text_fit(
            cx,
            Font::Regular,
            font,
            colors.muted,
            dvec2(left, message_label_y),
            &t("workspace.git.commit.message"),
            width,
        );
        let input_y = message_label_y + line + 4.0;
        let action_y = bottom - button_h;
        let input_frame = rect(left, input_y, width, (action_y - input_y - 10.0).max(line));
        paint.rounded(cx, input_frame, colors.surface2, 6.0, None);
        if !self.input_initialized || std::mem::take(&mut self.clear_input) {
            input.set_text(cx, &self.message);
            input.set_is_multiline(cx, true);
            self.input_initialized = true;
        }
        let font_size = crate::theme::Typography::pt(font);
        let foreground = colors.foreground;
        let muted = colors.muted;
        input.apply_over(cx, live! { draw_text: { color: (foreground), color_empty: (muted), text_style: { font_size: (font_size) } } });
        input.set_empty_text(cx, t("workspace.git.commit.messagePlaceholder"));
        input.set_is_read_only(cx, self.commit.is_some());
        let _ = input.draw_walk(
            cx,
            &mut Scope::empty(),
            Walk::abs_rect(rect(
                input_frame.pos.x + 8.0,
                input_frame.pos.y + 6.0,
                (input_frame.size.x - 16.0).max(1.0),
                (input_frame.size.y - 12.0).max(1.0),
            )),
        );
        let commit_button = rect(left, action_y, width.min(160.0), button_h);
        let label = t(if self.commit.is_some() {
            "workspace.git.commit.committing"
        } else {
            "workspace.git.commit.submit"
        });
        let enabled = self.can_commit(ui);
        button(
            cx,
            paint,
            commit_button,
            &label,
            Some("git-commit-horizontal"),
            ButtonVariant::Primary,
            ButtonSize::Sm,
            compact,
            enabled,
            self.hovered == Some(Action::Commit),
        );
        if enabled {
            self.hits.push(commit_button, Action::Commit);
        }
        if let Some(feedback) = self.feedback.as_ref() {
            paint.text_fit(
                cx,
                Font::Regular,
                font,
                colors.success,
                dvec2(
                    commit_button.pos.x + commit_button.size.x + 8.0,
                    action_y + 6.0,
                ),
                feedback,
                (width - commit_button.size.x - 8.0).max(0.0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshed_files_preserve_exclusions_without_silently_selecting_new_changes() {
        let mut selection = Selection::default();
        selection.replace(vec![
            json!({"path":"keep.rs"}),
            json!({"path":"exclude.rs"}),
        ]);
        selection.selected.remove("exclude.rs");
        selection.replace(vec![
            json!({"path":"keep.rs"}),
            json!({"path":"exclude.rs"}),
            json!({"path":"new.rs"}),
        ]);
        assert_eq!(selection.paths(), vec!["keep.rs"]);
        selection.replace(vec![json!({"path":"exclude.rs"}), json!({"path":"new.rs"})]);
        assert!(selection.paths().is_empty());
    }
}
