//! Native diagrams: one bounded renderer/export worker, revision-owned results and
//! an LRU of GUI textures. Source remains in the normal Markdown code painter.

pub mod engine;

use super::canvas::{Font, HitTarget, rect_xywh};
use super::i18n::t;
use super::markdown::view::MdOpts;
use super::paint::{Draw, RowState};
use super::scrollbar;
use makepad_widgets::*;
use paseo_gui_platform::{PlatformAction, PlatformValue};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, SystemTime};

const MAX_PENDING: usize = 16;
const MAX_CACHE_ENTRIES: usize = 32;
const MAX_CACHE_BYTES: usize = 96 << 20;
const MAX_EXPORTS: usize = 4;
const MAX_EXPORT_FILES: usize = 64;
const MAX_EXPORT_BYTES: u64 = 128 << 20;
const EXPORT_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Svg,
    Png,
}

impl ExportFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Svg => "diagram.svg",
            Self::Png => "diagram.png",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
        }
    }
}

struct Artifact {
    rendered: Arc<engine::Rendered>,
    texture: Texture,
}

impl Artifact {
    fn bytes(&self) -> usize {
        self.rendered.rgba.len().saturating_mul(2) + self.rendered.svg.len()
    }
}

#[derive(Default)]
struct Slot {
    source: Arc<str>,
    source_too_large: bool,
    dark: bool,
    epoch: u64,
    revision: Arc<AtomicU64>,
    submitted: Option<u64>,
    artifact: Weak<Artifact>,
    error: Option<String>,
    export_status: Option<(&'static str, String)>,
    exporting: bool,
}

/// Row ownership prevents a completed job from attaching to a recycled PortalList item.
#[derive(Default)]
pub struct Request(Rc<RefCell<Slot>>);

struct RenderWork {
    source: Arc<str>,
    dark: bool,
    revision: u64,
    current: Arc<AtomicU64>,
}

struct PendingRender {
    slot: Weak<RefCell<Slot>>,
    work: RenderWork,
}

struct ExportWork {
    rendered: Arc<engine::Rendered>,
    format: ExportFormat,
    destination: Option<PathBuf>,
    revision: u64,
    current: Arc<AtomicU64>,
}

struct PendingExport {
    slot: Weak<RefCell<Slot>>,
    work: ExportWork,
}

enum Work {
    Render(RenderWork),
    Export(ExportWork),
}
enum Finished {
    Render(Result<Option<engine::Rendered>, String>),
    Export(Result<Option<PathBuf>, String>),
}
enum Active {
    Render(Weak<RefCell<Slot>>, u64),
    Export(Weak<RefCell<Slot>>, u64, ExportFormat, bool),
}
enum PlatformPending {
    Choose(PendingExport),
    Save(Weak<RefCell<Slot>>, u64, PathBuf),
}

struct Worker {
    sender: Option<SyncSender<Work>>,
    receiver: ToUIReceiver<Finished>,
    active: Option<Active>,
    pending: VecDeque<PendingRender>,
    exports: VecDeque<PendingExport>,
    platform: HashMap<u64, PlatformPending>,
    cache: VecDeque<Rc<Artifact>>,
    cache_bytes: usize,
    epoch: u64,
}

impl Worker {
    fn new() -> Self {
        let receiver = ToUIReceiver::default();
        let done = receiver.sender();
        let (sender, jobs) = sync_channel::<Work>(1);
        let spawned = std::thread::Builder::new()
            .name("paseo-mermaid".into())
            .spawn(move || {
                let session = uuid::Uuid::new_v4().simple().to_string();
                while let Ok(job) = jobs.recv() {
                    let finished = match job {
                        Work::Render(job) => {
                            let result = if current(&job.current, job.revision) {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    engine::render(&job.source, job.dark)
                                }))
                                .unwrap_or_else(|_| Err("The diagram renderer failed.".into()))
                                .map(Some)
                            } else {
                                Ok(None)
                            };
                            Finished::Render(result)
                        }
                        Work::Export(job) => Finished::Export(
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                export_file(&job, &session)
                            }))
                            .unwrap_or_else(|_| Err("The diagram export worker failed.".into())),
                        ),
                    };
                    if done.send(finished).is_err() {
                        break;
                    }
                }
            });
        Self {
            sender: spawned.ok().map(|_| sender),
            receiver,
            active: None,
            pending: VecDeque::new(),
            exports: VecDeque::new(),
            platform: HashMap::new(),
            cache: VecDeque::new(),
            cache_bytes: 0,
            epoch: 0,
        }
    }

    fn touch(&mut self, artifact: &Rc<Artifact>) {
        if let Some(index) = self
            .cache
            .iter()
            .position(|entry| Rc::ptr_eq(entry, artifact))
        {
            let entry = self.cache.remove(index).unwrap();
            self.cache.push_back(entry);
        }
    }

    fn retain(&mut self, artifact: Rc<Artifact>) {
        let bytes = artifact.bytes();
        while !self.cache.is_empty()
            && (self.cache.len() >= MAX_CACHE_ENTRIES
                || self.cache_bytes.saturating_add(bytes) > MAX_CACHE_BYTES)
        {
            if let Some(old) = self.cache.pop_front() {
                self.cache_bytes -= old.bytes();
            }
        }
        self.cache_bytes += bytes;
        self.cache.push_back(artifact);
    }

    fn dispatch(&mut self) {
        if self.active.is_some() {
            return;
        }
        while let Some(job) = self.exports.pop_front() {
            if !export_is_current(&job.slot, job.work.revision) {
                set_export(&job.slot, job.work.revision, "mermaid.cancelled", "");
                continue;
            }
            let active = Active::Export(
                job.slot.clone(),
                job.work.revision,
                job.work.format,
                job.work.destination.is_some(),
            );
            if self
                .sender
                .as_ref()
                .is_some_and(|sender| sender.try_send(Work::Export(job.work)).is_ok())
            {
                self.active = Some(active);
                return;
            }
            set_export(
                &job.slot,
                active_revision(&active),
                "mermaid.exportError",
                "The export worker is unavailable.",
            );
        }
        while let Some(job) = self.pending.pop_front() {
            let Some(slot) = job.slot.upgrade() else {
                continue;
            };
            if !current(&job.work.current, job.work.revision) {
                continue;
            }
            let revision = job.work.revision;
            if self
                .sender
                .as_ref()
                .is_some_and(|sender| sender.try_send(Work::Render(job.work)).is_ok())
            {
                self.active = Some(Active::Render(job.slot, revision));
                return;
            }
            let mut slot = slot.borrow_mut();
            slot.submitted = None;
            slot.error = Some("The diagram worker is unavailable.".into());
        }
    }

    fn poll(&mut self, cx: &mut Cx) -> bool {
        let mut changed = false;
        while let Ok(done) = self.receiver.try_recv() {
            changed = true;
            match (self.active.take(), done) {
                (Some(Active::Render(owner, revision)), Finished::Render(result)) => {
                    if let Some(slot) = owner.upgrade() {
                        let mut slot = slot.borrow_mut();
                        if current(&slot.revision, revision) {
                            slot.submitted = None;
                            match result {
                                Ok(Some(rendered)) => {
                                    if rendered
                                        .rgba
                                        .len()
                                        .saturating_mul(2)
                                        .saturating_add(rendered.svg.len())
                                        > MAX_CACHE_BYTES
                                    {
                                        slot.error = Some(
                                            "The rendered diagram exceeds the texture cache limit."
                                                .into(),
                                        );
                                        continue;
                                    }
                                    // Texture upload is GUI-only; parsing, font discovery and rasterization are not.
                                    let decoded = crate::features::files::images::Decoded {
                                        width: rendered.width as usize,
                                        height: rendered.height as usize,
                                        rgba: rendered.rgba,
                                    };
                                    let texture =
                                        crate::features::files::images::texture(cx, &decoded);
                                    let rendered = engine::Rendered {
                                        rgba: decoded.rgba,
                                        ..rendered
                                    };
                                    if let Some(texture) = texture {
                                        let artifact = Rc::new(Artifact {
                                            rendered: Arc::new(rendered),
                                            texture,
                                        });
                                        slot.artifact = Rc::downgrade(&artifact);
                                        self.retain(artifact);
                                    } else {
                                        slot.error = Some(
                                            "The diagram texture could not be created.".into(),
                                        );
                                    }
                                }
                                Ok(None) => (),
                                Err(error) => slot.error = Some(error),
                            }
                        }
                    }
                }
                (
                    Some(Active::Export(owner, revision, format, desktop)),
                    Finished::Export(result),
                ) => {
                    match result {
                        Ok(Some(_)) if desktop => set_export(&owner, revision, "mermaid.saved", ""),
                        Ok(Some(path)) => {
                            if export_is_current(&owner, revision) {
                                if let Some(platform) = paseo_gui_platform::platform() {
                                    let request =
                                        platform.save_file(&path, format.mime(), format.name());
                                    self.platform.insert(
                                        request,
                                        PlatformPending::Save(owner, revision, path),
                                    );
                                } else {
                                    set_export(
                                        &owner,
                                        revision,
                                        "mermaid.exportError",
                                        "Platform services are unavailable.",
                                    );
                                    remove_export_file(path);
                                }
                            } else {
                                set_export(&owner, revision, "mermaid.cancelled", "");
                                remove_export_file(path);
                            }
                            // The document picker retains its source until actual completion.
                        }
                        Ok(None) => set_export(&owner, revision, "mermaid.cancelled", ""),
                        Err(error) => set_export(&owner, revision, "mermaid.exportError", &error),
                    }
                }
                _ => (),
            }
        }
        self.dispatch();
        if changed {
            self.epoch = self.epoch.wrapping_add(1);
        }
        changed
    }

    fn platform_result(
        &mut self,
        request: u64,
        result: &Result<PlatformValue, paseo_gui_platform::PlatformError>,
    ) -> bool {
        let Some(pending) = self.platform.remove(&request) else {
            return false;
        };
        match pending {
            PlatformPending::Choose(mut job) => match result {
                Ok(PlatformValue::SaveLocation(Some(destination))) => {
                    if export_is_current(&job.slot, job.work.revision) {
                        job.work.destination = Some(destination.clone());
                        self.exports.push_back(job);
                    } else {
                        set_export(&job.slot, job.work.revision, "mermaid.cancelled", "");
                    }
                }
                Ok(_) => set_export(&job.slot, job.work.revision, "mermaid.cancelled", ""),
                Err(error) => set_export(
                    &job.slot,
                    job.work.revision,
                    if error.code == "cancelled" {
                        "mermaid.cancelled"
                    } else {
                        "mermaid.exportError"
                    },
                    if error.code == "cancelled" {
                        ""
                    } else {
                        &error.message
                    },
                ),
            },
            PlatformPending::Save(owner, revision, path) => {
                remove_export_file(path);
                match result {
                    Ok(PlatformValue::Bool(true)) => {
                        set_export(&owner, revision, "mermaid.saved", "")
                    }
                    Ok(_) => set_export(&owner, revision, "mermaid.cancelled", ""),
                    Err(error) => set_export(
                        &owner,
                        revision,
                        if error.code == "cancelled" {
                            "mermaid.cancelled"
                        } else {
                            "mermaid.exportError"
                        },
                        if error.code == "cancelled" {
                            ""
                        } else {
                            &error.message
                        },
                    ),
                }
            }
        }
        self.dispatch();
        self.epoch = self.epoch.wrapping_add(1);
        true
    }
}

fn active_revision(active: &Active) -> u64 {
    match active {
        Active::Render(_, revision) | Active::Export(_, revision, _, _) => *revision,
    }
}

fn current(revision: &AtomicU64, expected: u64) -> bool {
    revision.load(Ordering::Relaxed) == expected
}

fn export_is_current(owner: &Weak<RefCell<Slot>>, revision: u64) -> bool {
    owner.upgrade().is_some_and(|slot| {
        let slot = slot.borrow();
        current(&slot.revision, revision) && slot.epoch == crate::theme::epoch()
    })
}

fn set_export(owner: &Weak<RefCell<Slot>>, revision: u64, key: &'static str, detail: &str) {
    if let Some(slot) = owner.upgrade() {
        let mut slot = slot.borrow_mut();
        if current(&slot.revision, revision) {
            slot.exporting = false;
            slot.export_status = Some((key, detail.chars().take(300).collect()));
        }
    }
}

thread_local! {
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

/// Shell hook, before row dispatch. Completion wakes Makepad through ToUISender;
/// redraw_all invalidates the PortalList's always-drawn row measurement pass.
pub fn handle_event(cx: &mut Cx, event: &Event) {
    let changed = WORKER.with(|worker| {
        let mut worker = worker.borrow_mut();
        let Some(worker) = worker.as_mut() else {
            return false;
        };
        let mut changed = false;
        if matches!(event, Event::Signal) {
            changed |= worker.poll(cx);
        }
        if let Event::Actions(actions) = event {
            for action in actions.iter() {
                if let Some(PlatformAction::Completed { request, result }) =
                    action.downcast_ref::<PlatformAction>()
                {
                    changed |= worker.platform_result(*request, result);
                }
            }
        }
        changed
    });
    if changed {
        cx.redraw_all();
    }
}

/// Changes only on completed render/export work, never on ordinary frames.
pub fn epoch() -> u64 {
    WORKER.with(|worker| worker.borrow().as_ref().map_or(0, |w| w.epoch))
}

fn request(request: &Request, source: &str, dark: bool, epoch: u64) -> Option<Rc<Artifact>> {
    WORKER.with(|worker| {
        let mut worker = worker.borrow_mut();
        let worker = worker.get_or_insert_with(Worker::new);
        let mut slot = request.0.borrow_mut();
        if source.len() > 32 * 1024 {
            slot.source_too_large = true;
            if slot.error.as_deref() != Some("Diagram source exceeds the 32 KiB limit.") {
                slot.revision.fetch_add(1, Ordering::Relaxed);
                slot.source = Arc::from("");
                slot.artifact = Weak::new();
                slot.submitted = None;
                slot.error = Some("Diagram source exceeds the 32 KiB limit.".into());
                slot.exporting = false;
                slot.export_status = None;
            }
            return None;
        }
        if slot.source_too_large
            || slot.source.as_ref() != source
            || slot.dark != dark
            || slot.epoch != epoch
        {
            slot.source_too_large = false;
            slot.source = Arc::from(source);
            slot.dark = dark;
            slot.epoch = epoch;
            slot.revision.fetch_add(1, Ordering::Relaxed);
            slot.submitted = None;
            slot.artifact = Weak::new();
            slot.error = None;
            slot.export_status = None;
            slot.exporting = false;
        }
        if let Some(artifact) = slot.artifact.upgrade() {
            worker.touch(&artifact);
            return Some(artifact);
        }
        if slot.error.is_none() && slot.submitted.is_none() {
            let revision = slot.revision.load(Ordering::Relaxed);
            let owner = Rc::downgrade(&request.0);
            worker
                .pending
                .retain(|queued| queued.slot.strong_count() > 0 && !queued.slot.ptr_eq(&owner));
            if worker.pending.len() == MAX_PENDING
                && let Some(old) = worker
                    .pending
                    .pop_front()
                    .and_then(|job| job.slot.upgrade())
            {
                old.borrow_mut().submitted = None;
            }
            slot.submitted = Some(revision);
            worker.pending.push_back(PendingRender {
                slot: owner,
                work: RenderWork {
                    source: slot.source.clone(),
                    dark,
                    revision,
                    current: slot.revision.clone(),
                },
            });
        }
        drop(slot);
        worker.dispatch();
        None
    })
}

/// Handles only export hits; source/fit controls use the existing Toggle target.
pub fn tap(state: &mut RowState, target: &HitTarget) -> bool {
    let HitTarget::MermaidExport {
        key,
        revision,
        format,
    } = target
    else {
        return false;
    };
    let Some(request) = state.mermaid.get(key) else {
        return true;
    };
    WORKER.with(|worker| {
        let mut worker = worker.borrow_mut();
        let Some(worker) = worker.as_mut() else {
            return;
        };
        let mut slot = request.0.borrow_mut();
        if !current(&slot.revision, *revision)
            || slot.epoch != crate::theme::epoch()
            || slot.exporting
        {
            return;
        }
        let Some(artifact) = slot.artifact.upgrade() else {
            return;
        };
        let count = worker.platform.len()
            + worker.exports.len()
            + usize::from(matches!(worker.active, Some(Active::Export(..))));
        if count >= MAX_EXPORTS {
            slot.export_status = Some((
                "mermaid.exportError",
                "Another diagram export is still pending.".into(),
            ));
            return;
        }
        let Some(platform) = paseo_gui_platform::platform() else {
            slot.export_status = Some((
                "mermaid.exportError",
                "Platform services are unavailable.".into(),
            ));
            return;
        };
        slot.exporting = true;
        slot.export_status = None;
        let job = PendingExport {
            slot: Rc::downgrade(&request.0),
            work: ExportWork {
                rendered: artifact.rendered.clone(),
                format: *format,
                destination: None,
                revision: *revision,
                current: slot.revision.clone(),
            },
        };
        if platform.capabilities().save_dialog {
            let id = platform.save_location(format.name());
            worker.platform.insert(id, PlatformPending::Choose(job));
        } else {
            worker.exports.push_back(job);
        }
        drop(slot);
        worker.dispatch();
    });
    true
}

pub struct DrawResult {
    pub height: f64,
    pub show_source: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    cx: &mut Cx2d,
    d: &mut Draw,
    opts: &mut MdOpts,
    source: &str,
    x: f64,
    y: f64,
    w: f64,
) -> DrawResult {
    opts.counter += 1;
    let key = format!("{}:mermaid:{}", opts.key, opts.counter);
    let dark = d.look.dark();
    let req = d.state.mermaid.entry(key.clone()).or_default();
    let artifact = request(req, source, dark, crate::theme::epoch());
    let (revision, error, exporting, status) = {
        let slot = req.0.borrow();
        (
            slot.revision.load(Ordering::Relaxed),
            slot.error.clone(),
            slot.exporting,
            slot.export_status.clone(),
        )
    };
    let source_key = format!("{key}:source");
    let fit_key = format!("{key}:fit");
    let show_source = d.state.is_expanded(&source_key, false);
    let fit = d.state.is_expanded(&fit_key, true);
    let p = d.look.palette;
    let width = w.max(1.0);
    let font_size = d.look.ui(12.0);
    let button_h = (font_size * 1.4 + 20.0).max(44.0);
    let mut bx = x;
    let mut by = y;
    let buttons = [
        (
            t(if show_source {
                "mermaid.hideSource"
            } else {
                "mermaid.source"
            }),
            Some(HitTarget::Toggle(source_key)),
        ),
        (
            t(if fit {
                "mermaid.actualSize"
            } else {
                "mermaid.fit"
            }),
            Some(HitTarget::Toggle(fit_key)),
        ),
        (
            t("mermaid.saveSvg"),
            (artifact.is_some() && !exporting).then(|| HitTarget::MermaidExport {
                key: key.clone(),
                revision,
                format: ExportFormat::Svg,
            }),
        ),
        (
            t("mermaid.savePng"),
            (artifact.is_some() && !exporting).then(|| HitTarget::MermaidExport {
                key: key.clone(),
                revision,
                format: ExportFormat::Png,
            }),
        ),
    ];
    for (label, target) in buttons {
        let bw = (d
            .kit
            .measure(cx, Font::Medium, font_size, 1.4, &label, None)
            .x
            + 24.0)
            .max(72.0)
            .min(width);
        if bx > x && bx + bw > x + width {
            bx = x;
            by += button_h + 4.0;
        }
        let rect = rect_xywh(bx, by, bw, button_h);
        d.kit.rect(cx, rect, p.surface2, 5.0);
        d.kit.label_line(
            cx,
            Font::Medium,
            font_size,
            1.4,
            if target.is_some() {
                p.foreground
            } else {
                p.muted
            },
            dvec2(bx + 12.0, by + (button_h - font_size * 1.4) * 0.5),
            &label,
            (bw - 24.0).max(1.0),
        );
        if let Some(target) = target {
            d.canvas.add_hit(rect, target);
        }
        bx += bw + 4.0;
    }
    let mut cy = by + button_h + 8.0;
    if let Some(artifact) = artifact {
        let iw = artifact.rendered.width as f64;
        let ih = artifact.rendered.height as f64;
        let bar = scrollbar::BAR;
        let scale = if fit {
            ((width - bar).max(1.0) / iw).min(1.0)
        } else {
            1.0
        };
        let image_w = iw * scale;
        let image_h = ih * scale;
        let view_h = image_h.clamp(1.0, 420.0);
        let pane = rect_xywh(x, cy, width, view_h + bar);
        let viewport = rect_xywh(x, cy, (width - bar).max(1.0), view_h);
        let max_x = (image_w - viewport.size.x).max(0.0);
        let max_y = (image_h - view_h).max(0.0);
        d.state.extent_x.insert(key.clone(), max_x);
        d.state.extent_y.insert(key.clone(), max_y);
        let sx = d
            .state
            .scroll_x
            .get(&key)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, max_x);
        let sy = d
            .state
            .scroll_y
            .get(&key)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, max_y);
        cx.begin_turtle(
            Walk {
                abs_pos: Some(viewport.pos),
                width: Size::Fixed(viewport.size.x),
                height: Size::Fixed(view_h),
                margin: Margin::default(),
            },
            Layout::default(),
        );
        d.canvas.push_clip(viewport);
        d.kit.texture(
            cx,
            rect_xywh(x - sx, cy - sy, image_w, image_h),
            &artifact.texture,
            0.0,
        );
        d.canvas.pop_clip();
        cx.end_turtle();
        if max_x > 0.0 {
            d.canvas.add_hit(viewport, HitTarget::ScrollX(key.clone()));
        }
        if max_y > 0.0 {
            d.canvas.add_hit(viewport, HitTarget::ScrollY(key.clone()));
        }
        scrollbar::draw(
            cx,
            d,
            pane,
            Some(p.surface0),
            Some(scrollbar::Axis {
                key: &key,
                offset: sx,
                max: max_x,
            }),
            Some(scrollbar::Axis {
                key: &key,
                offset: sy,
                max: max_y,
            }),
        );
        cy += pane.size.y + 8.0;
    } else {
        let label = if let Some(error) = error {
            format!(
                "{}\n{}",
                t("mermaid.error"),
                error.chars().take(300).collect::<String>()
            )
        } else {
            t("mermaid.loading")
        };
        let size = d.kit.label(
            cx,
            Font::Regular,
            font_size,
            1.4,
            p.muted,
            dvec2(x, cy),
            &label,
            Some(width),
        );
        cy += size.y.max(32.0) + 8.0;
    }
    if exporting || status.is_some() {
        let label = if exporting {
            t("mermaid.exporting")
        } else {
            let (key, detail) = status.unwrap();
            if detail.is_empty() {
                t(key)
            } else {
                format!("{}: {detail}", t(key))
            }
        };
        let size = d.kit.label(
            cx,
            Font::Regular,
            font_size,
            1.4,
            p.muted,
            dvec2(x, cy),
            &label,
            Some(width),
        );
        cy += size.y + 8.0;
    }
    DrawResult {
        height: cy - y,
        show_source,
    }
}

fn remove_export_file(path: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("mermaid-export-cleanup".into())
        .spawn(move || {
            if std::fs::remove_file(&path).is_ok()
                && let Some(directory) = path.parent()
            {
                let _ = std::fs::remove_dir(directory);
            }
        });
}

fn export_file(job: &ExportWork, session: &str) -> Result<Option<PathBuf>, String> {
    if !current(&job.current, job.revision) {
        return Ok(None);
    }
    let png;
    let bytes = match job.format {
        ExportFormat::Svg => job.rendered.svg.as_bytes(),
        ExportFormat::Png => {
            png = engine::encode_png(&job.rendered)?;
            &png
        }
    };
    if !current(&job.current, job.revision) {
        return Ok(None);
    }
    if let Some(destination) = &job.destination {
        save_atomic(destination, bytes).map_err(|e| e.to_string())?;
        return Ok(Some(destination.clone()));
    }
    let root = paseo_gui_platform::paths::app_paths()
        .temp_dir
        .join("paseo-mermaid");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let (count, size) = retained_exports(&root, session).map_err(|e| e.to_string())?;
    if count >= MAX_EXPORT_FILES || size.saturating_add(bytes.len() as u64) > MAX_EXPORT_BYTES {
        return Err("The temporary diagram export limit has been reached; close pending save dialogs and retry.".into());
    }
    let directory = root.join(format!("{session}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(job.format.name());
    if let Err(error) = std::fs::write(&path, bytes) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error.to_string());
    }
    Ok(Some(path))
}

/// Write beside the destination and replace only after complete encoding and IO.
/// A failed rename preserves the user's existing file (including on Windows).
fn save_atomic(destination: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("No destination directory"))?;
    let staging = parent.join(format!(
        ".paseo-diagram-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&staging, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result
}

/// Only private UUID-named prior-session directories are eligible. Completed
/// document exports are removed immediately; interrupted requests expire later.
fn retained_exports(root: &Path, session: &str) -> std::io::Result<(usize, u64)> {
    let mut count = 0;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !owned_export_directory(&name) || !entry.file_type()?.is_dir() {
            continue;
        }
        let metadata = entry.metadata()?;
        let old = metadata
            .modified()
            .ok()
            .and_then(|time| SystemTime::now().duration_since(time).ok())
            .is_some_and(|age| age > EXPORT_RETENTION);
        if !name.starts_with(session) && old && std::fs::remove_dir_all(entry.path()).is_ok() {
            continue;
        }
        for file in std::fs::read_dir(entry.path())? {
            let file = file?;
            if file.file_type()?.is_file() {
                count += 1;
                bytes = bytes.saturating_add(file.metadata()?.len());
            }
        }
    }
    Ok((count, bytes))
}

fn owned_export_directory(name: &str) -> bool {
    name.len() == 65
        && name.as_bytes()[32] == b'-'
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 32 || b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_revision_cannot_finish_old_export() {
        let slot = Rc::new(RefCell::new(Slot::default()));
        slot.borrow().revision.store(2, Ordering::Relaxed);
        set_export(&Rc::downgrade(&slot), 1, "mermaid.saved", "");
        assert!(slot.borrow().export_status.is_none());
        set_export(&Rc::downgrade(&slot), 2, "mermaid.saved", "");
        assert_eq!(
            slot.borrow().export_status.as_ref().unwrap().0,
            "mermaid.saved"
        );
        slot.borrow_mut().epoch = crate::theme::epoch();
        assert!(export_is_current(&Rc::downgrade(&slot), 2));
        slot.borrow_mut().epoch = crate::theme::epoch().wrapping_add(1);
        assert!(!export_is_current(&Rc::downgrade(&slot), 2));
    }

    #[test]
    fn cleanup_only_accepts_owned_uuid_directories() {
        assert!(owned_export_directory(
            "0123456789abcdef0123456789abcdef-0123456789abcdef0123456789abcdef"
        ));
        assert!(!owned_export_directory("../transfer-other"));
        assert!(!owned_export_directory("unrelated"));
        assert!(!owned_export_directory(
            "0123456789abcdef0123456789abcdef/0123456789abcdef0123456789abcdef"
        ));
    }
}
