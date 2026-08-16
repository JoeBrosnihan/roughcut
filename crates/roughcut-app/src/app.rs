//! Application state and the update loop.
//!
//! The repaint discipline that makes §3's zero-idle-CPU requirement work lives
//! here: nothing in `update` requests a repaint unconditionally. Repaints come
//! from input events (egui does that for us), from a playback timer while the
//! transport is running, and from background workers and mpv calling
//! `Context::request_repaint` when they have something new.

use crate::actions::Action;
use crate::monitor::{Monitor, Transport};
use crate::settings::Settings;
use crate::video::{shared as shared_video, SharedVideo};
use crate::workers::{Job, JobResult, WorkerPool};
use crate::{keys, theme, ui};

use roughcut_core::import::{add_clip, ImportOutcome};
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::{ClipId, Project};
use roughcut_core::probe::MediaInfo;
use roughcut_core::project_io;
use roughcut_core::time::Rational;
use roughcut_core::timeline;
use roughcut_core::tools::{expand_drop, Tools};
use roughcut_core::undo::History;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Source,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Warn,
    Error,
}

/// The five marking operations. An enum rather than a pair of booleans, so
/// that call sites read as what they do instead of as `mark(true, false)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkOp {
    SetIn,
    SetOut,
    ClearIn,
    ClearOut,
    ClearBoth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyState {
    Queued,
    Running,
    Failed,
}

pub struct RoughcutApp {
    pub project: Project,
    pub history: History,
    pub project_path: Option<PathBuf>,
    pub dirty: bool,

    pub settings: Settings,
    pub tools: Tools,
    pub workers: WorkerPool,
    pub monitor: Monitor,
    pub video: SharedVideo,

    pub focus: Focus,
    /// Bin selection — what the source monitor shows.
    pub selected_clip: Option<ClipId>,
    /// Playhead within the selected source clip.
    pub source_frame: i64,
    /// Playhead along the assembled timeline.
    pub playhead: i64,
    /// Timeline selection, as an index into `project.timeline`.
    pub selected_item: Option<usize>,

    /// Pixels per frame; `zoom_fit` recomputes it from the panel width.
    pub zoom: f32,
    pub zoom_fit: bool,
    /// Horizontal scroll of the timeline as of the last pass, and a position
    /// to jump it to on the next one. Together these let a wheel zoom stay
    /// anchored on the frame under the pointer instead of on the left edge.
    pub timeline_offset: f32,
    pub timeline_scroll_to: Option<f32>,
    /// Timeline item being dragged to a new position, if any.
    pub dragging_item: Option<usize>,
    /// A scrub in progress on the timeline ruler.
    pub scrubbing: bool,
    /// Set when the playhead crosses a cut, so the next media sync forces mpv
    /// to the new position instead of letting it keep playing where it was.
    force_media_jump: bool,
    /// A shift-drag in progress on the source scrub bar, as (anchor, current)
    /// source frames. Marks are only committed on release, so the drag costs
    /// one undo entry rather than one per pixel.
    pub mark_drag: Option<(i64, i64)>,
    /// Mirrors the viewport's fullscreen state, so F11 toggles rather than
    /// guessing. `fullscreen_pending` defers the actual viewport command to
    /// `update`, which is the only place with a `Context` to send it on.
    pub fullscreen: bool,
    fullscreen_pending: bool,

    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    thumb_requested: HashSet<ClipId>,
    pub proxy_state: HashMap<ClipId, ProxyState>,

    /// Text for the alert bar, with when it was set. Info messages expire;
    /// warnings and errors stay until the condition behind them clears.
    pub status: Option<(String, StatusKind, std::time::Instant)>,
    pub show_help: bool,
    pub show_missing_tool: bool,
    pub missing_media: Vec<(ClipId, PathBuf)>,

    /// Where the recovery snapshot is written.
    autosave_path: Option<PathBuf>,
    /// Set by `edit`; the snapshot is written once at the end of the pass, so
    /// an action that makes several edits still costs one write.
    autosave_pending: bool,
    /// Work found on disk from a previous session, awaiting the user's answer.
    pub recovery: Option<project_io::AutoSave>,

    /// Wakes the event loop from mpv's callbacks.
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// Last title actually pushed to the window. Sending a viewport command
    /// schedules another pass, so re-sending an unchanged title every frame
    /// would spin the event loop forever.
    last_title: String,
}

impl RoughcutApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let settings = Settings::load();
        let tools = Tools::discover()
            .with_overrides(settings.ffprobe_path.as_deref(), settings.ffmpeg_path.as_deref());

        let ctx_for_workers = cc.egui_ctx.clone();
        let workers = WorkerPool::new(ctx_for_workers, tools.clone());

        // mpv wakes the UI through this; it must never do work itself.
        let ctx_for_mpv = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || ctx_for_mpv.request_repaint());
        let monitor = Monitor::new(repaint.clone());

        let mut app = Self {
            project: Project::new(),
            history: History::new(),
            project_path: None,
            dirty: false,
            settings,
            tools,
            workers,
            monitor,
            video: shared_video(),
            focus: Focus::Source,
            selected_clip: None,
            source_frame: 0,
            playhead: 0,
            selected_item: None,
            zoom: 0.5,
            zoom_fit: true,
            timeline_offset: 0.0,
            timeline_scroll_to: None,
            dragging_item: None,
            scrubbing: false,
            mark_drag: None,
            force_media_jump: false,
            fullscreen: false,
            fullscreen_pending: false,
            thumbnails: HashMap::new(),
            thumb_requested: HashSet::new(),
            proxy_state: HashMap::new(),
            status: None,
            show_help: false,
            show_missing_tool: false,
            missing_media: Vec::new(),
            autosave_path: crate::settings::autosave_path(),
            autosave_pending: false,
            recovery: None,
            repaint,
            last_title: String::new(),
        };
        log::info!(
            "config dir: {}",
            crate::settings::config_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".into())
        );
        app.recovery = app.find_recovery();
        app.monitor.set_fps(app.project.fps());
        app.monitor.set_volume(app.settings.volume);
        if !app.tools.has_ffprobe() {
            app.set_status(
                "ffprobe was not found — import is unavailable until it is located",
                StatusKind::Error,
            );
        }
        app
    }

    // --- small helpers ------------------------------------------------------

    pub fn fps(&self) -> Rational {
        self.project.fps()
    }

    pub fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        let text = text.into();
        match kind {
            StatusKind::Error => log::error!("{text}"),
            StatusKind::Warn => log::warn!("{text}"),
            StatusKind::Info => log::info!("{text}"),
        }
        self.status = Some((text, kind, std::time::Instant::now()));
    }

    /// How long an informational message stays on screen. Long enough to read
    /// a filename, short enough that the bar is not permanent furniture.
    pub const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(5);

    /// Drop an expired message, and say when to come back if one is still
    /// counting down. Returns the repaint delay, if any.
    fn expire_status(&mut self) -> Option<std::time::Duration> {
        let (_, kind, at) = self.status.as_ref()?;
        if *kind != StatusKind::Info {
            return None;
        }
        let elapsed = at.elapsed();
        if elapsed >= Self::STATUS_TTL {
            self.status = None;
            None
        } else {
            Some(Self::STATUS_TTL - elapsed)
        }
    }

    /// Run an edit, snapshotting for undo only if it actually changed things.
    ///
    /// This is the single funnel every project mutation passes through, which
    /// makes it the right and only place to arm the autosave.
    fn edit(&mut self, f: impl FnOnce(&mut Project) -> bool) -> bool {
        let before = self.project.clone();
        if f(&mut self.project) {
            self.history.snapshot(&before);
            self.dirty = true;
            self.autosave_pending = true;
            true
        } else {
            // Guarantee no partial mutation survives a refused edit.
            self.project = before;
            false
        }
    }

    // --- autosave -----------------------------------------------------------

    /// Write the recovery snapshot, if one is due. Called once per pass, and
    /// passes only happen in response to input — so this never runs while the
    /// application is idle.
    fn flush_autosave(&mut self) {
        if !self.autosave_pending {
            return;
        }
        // Never overwrite work the user has not yet been asked about. Media
        // named on the command line imports straight away, which would
        // otherwise destroy the snapshot before the prompt is answered.
        if self.recovery.is_some() {
            return;
        }
        self.autosave_pending = false;
        let Some(path) = self.autosave_path.clone() else {
            return;
        };
        if let Err(e) = project_io::write_autosave(
            &path,
            &self.project,
            self.project_path.as_deref(),
            self.dirty,
        ) {
            // Never interrupt editing for this; the alert bar is enough.
            log::warn!("autosave failed: {e:#}");
            self.set_status(format!("autosave failed: {e}"), StatusKind::Warn);
        }
    }

    /// Note that the snapshot no longer holds unsaved work. The file itself is
    /// kept — nothing in Roughcut deletes a snapshot, so File > Recover last
    /// session can always reach it, even after the prompt has been dismissed.
    fn mark_autosave_saved(&mut self) {
        self.autosave_pending = false;
        if let Some(path) = &self.autosave_path {
            project_io::mark_autosave_saved(path);
        }
    }

    /// Work left behind by a previous session, if any is worth offering.
    fn find_recovery(&self) -> Option<project_io::AutoSave> {
        let path = self.autosave_path.as_ref()?;
        if !path.exists() {
            return None;
        }
        match project_io::read_autosave(path) {
            // An empty project is not worth interrupting startup for.
            Ok(s) if s.project.clips.is_empty() && s.project.timeline.is_empty() => None,
            // A snapshot that was already saved is kept, but is not worth
            // interrupting startup for; File > Recover last session reaches it.
            Ok(s) if !s.unsaved => None,
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("ignoring unreadable autosave: {e:#}");
                None
            }
        }
    }

    /// Adopt recovered work. It stays dirty, because it has not been written
    /// to its real project file yet — that is the whole point.
    pub fn accept_recovery(&mut self) {
        let Some(snapshot) = self.recovery.take() else {
            return;
        };
        let had_path = snapshot.project_path.is_some();
        self.project = snapshot.project;
        self.project_path = snapshot.project_path;
        self.history.clear();
        self.dirty = true;
        self.monitor.set_fps(self.project.fps());
        self.selected_clip = self.project.clips.first().map(|c| c.id);
        self.selected_item = None;
        self.source_frame = 0;
        self.playhead = 0;
        self.missing_media = project_io::missing_media(&self.project);
        self.set_status(
            if had_path {
                "recovered — still unsaved, press Ctrl+S"
            } else {
                "recovered — this project has never been saved, press Ctrl+S"
            },
            StatusKind::Warn,
        );
    }

    /// "Discard" on the prompt means "do not ask me again", not "destroy it".
    pub fn decline_recovery(&mut self) {
        self.recovery = None;
        self.mark_autosave_saved();
    }

    /// Load the last session's snapshot on demand, whatever its state. This is
    /// the way back if the startup prompt was dismissed, or if the work was
    /// saved and then regretted.
    pub fn recover_last_session(&mut self) {
        let Some(path) = self.autosave_path.clone() else {
            return;
        };
        match project_io::read_autosave(&path) {
            Ok(snapshot) => {
                self.recovery = Some(snapshot);
            }
            Err(e) => self.set_status(format!("no session to recover: {e}"), StatusKind::Warn),
        }
    }

    /// Whether there is anything for File > Recover last session to offer.
    pub fn has_recoverable_session(&self) -> bool {
        self.autosave_path
            .as_ref()
            .is_some_and(|p| project_io::read_autosave(p).is_ok())
    }

    pub fn timeline_len(&self) -> i64 {
        timeline::total_frames(&self.project.timeline)
    }

    pub fn selected_source(&self) -> Option<&roughcut_core::SourceClip> {
        self.selected_clip.and_then(|id| self.project.clip(id))
    }

    /// The file and frame the monitor should be showing right now.
    pub fn current_media(&self) -> Option<(PathBuf, i64)> {
        match self.focus {
            Focus::Source => {
                let clip = self.selected_source()?;
                let f = self.source_frame.clamp(0, clip.last_frame());
                Some((clip.playback_path().to_path_buf(), f))
            }
            Focus::Timeline => {
                let (idx, offset) = timeline::item_at(&self.project.timeline, self.playhead)?;
                let item = self.project.timeline[idx];
                let clip = self.project.clip(item.clip_id)?;
                Some((
                    clip.playback_path().to_path_buf(),
                    (item.in_frame + offset).clamp(0, clip.last_frame()),
                ))
            }
        }
    }

    pub fn window_title(&self) -> String {
        let name = self
            .project_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());
        format!("{}{} — Roughcut", if self.dirty { "*" } else { "" }, name)
    }

    // --- playback -----------------------------------------------------------

    /// Move the authoritative playhead in response to mpv's progress, and
    /// service the reverse-shuttle timer.
    fn advance_playback(&mut self) {
        match self.monitor.transport {
            Transport::Forward(_) => {
                let Some(reported) = self.monitor.playback_frame() else {
                    return;
                };
                match self.focus {
                    Focus::Source => {
                        let last = self.position_max();
                        self.set_position(reported);
                        // The end of a bin clip really is the end.
                        if reported >= last || self.monitor.eof {
                            self.monitor.pause();
                        }
                    }
                    Focus::Timeline => self.advance_timeline_playback(reported),
                }
            }
            Transport::Reverse(_) => {
                let steps = self.monitor.reverse_step_due();
                if steps > 0 {
                    self.step_frames(-steps);
                    if self.position() == 0 {
                        self.monitor.pause();
                    }
                }
            }
            Transport::Paused => {}
        }
    }

    /// Timeline playback runs one item at a time: when mpv passes the item's
    /// out point we load the next one. There is a small hitch at each cut,
    /// which is the honest cost of not building an EDL.
    fn advance_timeline_playback(&mut self, reported_source_frame: i64) {
        let Some((idx, _)) = timeline::item_at(&self.project.timeline, self.playhead) else {
            self.monitor.pause();
            return;
        };
        // Reached only with timeline focus, so `set_position` moves the
        // timeline playhead and clamps it.
        let item = self.project.timeline[idx];
        let start = timeline::item_start(&self.project.timeline, idx);

        // This item is finished either when mpv plays past its out point, or
        // when mpv runs out of file — which happens whenever the out point is
        // the last frame of its source, i.e. for any whole clip.
        let done = reported_source_frame > item.out_frame || self.monitor.eof;
        if done {
            let next_start = start + item.len();
            if next_start >= self.timeline_len() {
                self.monitor.pause();
                self.set_position(timeline::last_frame(&self.project.timeline));
            } else {
                log::debug!(
                    "timeline: item {idx} done at source frame {reported_source_frame}                      (out {}, eof {}) - rolling to frame {next_start}",
                    item.out_frame,
                    self.monitor.eof
                );
                self.set_position(next_start);
                // mpv is either at the end of a file or midway through the
                // wrong part of one; either way it must be told where to go.
                self.force_media_jump = true;
            }
            return;
        }
        let offset = (reported_source_frame - item.in_frame).max(0);
        self.set_position(start + offset);
    }

    // The source monitor and the timeline each have their own playhead. These
    // three are the only places that know which one the keyboard is driving,
    // and `set_position` is the only place either is clamped or written.

    pub fn position(&self) -> i64 {
        match self.focus {
            Focus::Source => self.source_frame,
            Focus::Timeline => self.playhead,
        }
    }

    /// Last legal position for the focused region.
    pub fn position_max(&self) -> i64 {
        match self.focus {
            Focus::Source => self.selected_source().map_or(0, |c| c.last_frame()),
            Focus::Timeline => timeline::last_frame(&self.project.timeline),
        }
    }

    pub fn set_position(&mut self, frame: i64) {
        let frame = frame.clamp(0, self.position_max());
        match self.focus {
            Focus::Source => self.source_frame = frame,
            Focus::Timeline => self.playhead = frame,
        }
    }

    pub fn step_frames(&mut self, delta: i64) {
        let target = self.position() + delta;
        self.set_position(target);
    }

    // --- actions ------------------------------------------------------------

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::TogglePlay => self.monitor.toggle_play(),
            Action::ShuttleForward => self.monitor.shuttle_forward(),
            Action::ShuttleReverse => self.monitor.shuttle_reverse(),
            Action::Pause => self.monitor.pause(),
            Action::StepFrames(d) => {
                self.monitor.pause();
                self.step_frames(d);
            }
            Action::StepSeconds(d) => {
                self.monitor.pause();
                self.step_frames(d * self.fps().nominal_fps());
            }
            Action::GoToStart => {
                self.monitor.pause();
                self.set_position(0);
            }
            Action::GoToEnd => {
                self.monitor.pause();
                let end = self.position_max();
                self.set_position(end);
            }
            Action::PrevCut => {
                if self.focus == Focus::Timeline {
                    if let Some(p) = timeline::prev_cut(&self.project.timeline, self.playhead) {
                        self.monitor.pause();
                        self.set_position(p);
                    }
                }
            }
            Action::NextCut => {
                if self.focus == Focus::Timeline {
                    if let Some(p) = timeline::next_cut(&self.project.timeline, self.playhead) {
                        self.monitor.pause();
                        self.set_position(p);
                    }
                }
            }

            Action::MarkIn => self.mark(MarkOp::SetIn),
            Action::MarkOut => self.mark(MarkOp::SetOut),
            Action::ClearIn => self.mark(MarkOp::ClearIn),
            Action::ClearOut => self.mark(MarkOp::ClearOut),
            Action::ClearMarks => self.mark(MarkOp::ClearBoth),

            Action::Append => self.append_marked(),
            Action::Insert => self.insert_marked(),

            Action::Split => self.split(),
            // Delete removes whatever is in the region you are looking at.
            Action::RippleDelete => match self.focus {
                Focus::Source => self.remove_selected_clip(),
                Focus::Timeline => self.ripple_delete(),
            },
            Action::TrimHead => self.trim(true),
            Action::TrimTail => self.trim(false),
            Action::MoveEarlier => self.move_selected(-1),
            Action::MoveLater => self.move_selected(1),

            Action::OpenProject => self.open_project_dialog(),
            Action::SaveProject => self.save_project(false),
            Action::SaveProjectAs => self.save_project(true),
            Action::Import => self.import_dialog(),
            Action::ExportMlt => self.export_dialog(),
            Action::Undo => {
                if self.history.undo(&mut self.project) {
                    self.after_history_change();
                        }
            }
            Action::Redo => {
                if self.history.redo(&mut self.project) {
                    self.after_history_change();
                        }
            }

            Action::ToggleFocus => {
                self.monitor.pause();
                self.focus = match self.focus {
                    Focus::Source => Focus::Timeline,
                    Focus::Timeline => Focus::Source,
                };
            }
            Action::ZoomIn => {
                self.zoom_fit = false;
                self.zoom = (self.zoom * 1.5).min(40.0);
            }
            Action::ZoomOut => {
                self.zoom_fit = false;
                self.zoom = (self.zoom / 1.5).max(0.002);
            }
            Action::ZoomFit => self.zoom_fit = true,
            Action::ToggleFullscreen => self.fullscreen_pending = true,
            Action::ToggleHelp => self.show_help = !self.show_help,
        }
    }

    fn mark(&mut self, op: MarkOp) {
        let Some(id) = self.selected_clip else {
            self.set_status("select a clip in the bin first", StatusKind::Warn);
            return;
        };
        let frame = self.source_frame;
        let changed = self.edit(|p| {
            let Some(c) = p.clip_mut(id) else { return false };
            match op {
                MarkOp::SetIn => {
                    // Marking in past out pushes out along, rather than
                    // silently creating an impossible range.
                    if c.mark_out.is_some_and(|o| o < frame) {
                        c.mark_out = None;
                    }
                    let changed = c.mark_in != Some(frame);
                    c.mark_in = Some(frame);
                    changed
                }
                MarkOp::SetOut => {
                    if c.mark_in.is_some_and(|i| i > frame) {
                        c.mark_in = None;
                    }
                    let changed = c.mark_out != Some(frame);
                    c.mark_out = Some(frame);
                    changed
                }
                MarkOp::ClearIn => {
                    let changed = c.mark_in.is_some();
                    c.mark_in = None;
                    changed
                }
                MarkOp::ClearOut => {
                    let changed = c.mark_out.is_some();
                    c.mark_out = None;
                    changed
                }
                MarkOp::ClearBoth => {
                    let changed = c.mark_in.is_some() || c.mark_out.is_some();
                    c.mark_in = None;
                    c.mark_out = None;
                    changed
                }
            }
        });
        let _ = changed;
    }

    fn marked_range(&self) -> Option<(ClipId, i64, i64)> {
        let clip = self.selected_source()?;
        let (i, o) = clip.marked_range()?;
        Some((clip.id, i, o))
    }

    fn append_marked(&mut self) {
        let Some(id) = self.selected_clip else {
            self.set_status("nothing marked to append", StatusKind::Warn);
            return;
        };
        self.append_clip(id);
    }

    /// Append a specific bin clip's marked range to the timeline — the range
    /// `A` would append, so double-clicking a tile and pressing `A` do the
    /// same thing.
    pub fn append_clip(&mut self, id: ClipId) {
        let Some((in_frame, out_frame)) =
            self.project.clip(id).and_then(|c| c.marked_range())
        else {
            self.set_status("that clip has no usable range", StatusKind::Warn);
            return;
        };
        if self.edit(|p| timeline::append(p, id, in_frame, out_frame)) {
        }
    }

    fn insert_marked(&mut self) {
        let Some((id, i, o)) = self.marked_range() else {
            self.set_status("nothing marked to insert", StatusKind::Warn);
            return;
        };
        let at = self.playhead;
        let mut inserted_at = None;
        let ok = self.edit(|p| {
            inserted_at = timeline::insert_at(p, at, id, i, o);
            inserted_at.is_some()
        });
        if ok {
            self.selected_item =
                timeline::item_at(&self.project.timeline, inserted_at.unwrap_or(at))
                    .map(|(idx, _)| idx);
        }
    }

    /// Drop a bin clip onto the timeline at `frame`.
    ///
    /// Uses the clip's marked range, which defaults to the whole clip when
    /// nothing is marked — the same range `V` would insert, so dragging and
    /// the keyboard cannot disagree about what a clip means.
    pub fn drop_clip_at(&mut self, clip_id: ClipId, frame: i64) {
        let Some((in_frame, out_frame)) =
            self.project.clip(clip_id).and_then(|c| c.marked_range())
        else {
            self.set_status("that clip has no usable range", StatusKind::Warn);
            return;
        };
        let mut landed = None;
        let ok = self.edit(|p| {
            landed = timeline::insert_at(p, frame.max(0), clip_id, in_frame, out_frame);
            landed.is_some()
        });
        if ok {
            let at = landed.unwrap_or(0);
            self.focus = Focus::Timeline;
            self.set_position(at);
            self.selected_item =
                timeline::item_at(&self.project.timeline, at).map(|(i, _)| i);
        }
    }

    /// Which timeline item an edit applies to: an explicit mouse selection if
    /// there is one, otherwise whatever the playhead is sitting on. That
    /// removes the need for a separate "select this" key — the playhead is
    /// already pointing at what you mean.
    fn target_item(&self) -> Option<usize> {
        self.selected_item.or_else(|| {
            timeline::item_at(&self.project.timeline, self.playhead).map(|(i, _)| i)
        })
    }

    fn split(&mut self) {
        let at = self.playhead;
        if self.edit(|p| timeline::split_at(p, at)) {
            self.selected_item = None;
        }
    }

    /// Remove the selected clip from the bin.
    pub fn remove_selected_clip(&mut self) {
        let Some(id) = self.selected_clip else {
            self.set_status("no clip selected in the bin", StatusKind::Warn);
            return;
        };
        let uses = self.project.timeline_uses(id);
        if uses > 0 {
            self.set_status(
                format!(
                    "still used by {uses} cut{} on the timeline — delete those first",
                    if uses == 1 { "" } else { "s" }
                ),
                StatusKind::Warn,
            );
            return;
        }
        let name = self
            .project
            .clip(id)
            .map(|c| c.file_name())
            .unwrap_or_default();
        // Pick the neighbour to land on before the clip disappears.
        let next = {
            let i = self.project.clips.iter().position(|c| c.id == id);
            i.and_then(|i| {
                self.project
                    .clips
                    .get(i + 1)
                    .or_else(|| if i > 0 { self.project.clips.get(i - 1) } else { None })
            })
            .map(|c| c.id)
        };
        if self.edit(|p| p.remove_clip(id)) {
            self.thumbnails.remove(&id);
            self.thumb_requested.remove(&id);
            self.proxy_state.remove(&id);
            self.selected_clip = next;
            self.source_frame = 0;
            self.monitor.clear();
            log::info!("removed {name}");
        }
    }

    /// Commit a shift-drag on the scrub bar as the clip's in and out points.
    pub fn commit_mark_drag(&mut self) {
        let Some((a, b)) = self.mark_drag.take() else {
            return;
        };
        let Some(id) = self.selected_clip else { return };
        let (lo, hi) = (a.min(b), a.max(b));
        if lo == hi {
            return;
        }
        self.edit(|p| {
            let Some(c) = p.clip_mut(id) else { return false };
            c.mark_in = Some(lo);
            c.mark_out = Some(hi);
            true
        });
    }

    fn ripple_delete(&mut self) {
        let Some(idx) = self.target_item() else {
            self.set_status("no clip under the playhead", StatusKind::Warn);
            return;
        };
        let start = timeline::item_start(&self.project.timeline, idx);
        if self.edit(|p| timeline::ripple_delete(p, idx)) {
            let last = timeline::last_frame(&self.project.timeline);
            self.playhead = start.min(last);
            self.selected_item = if self.project.timeline.is_empty() {
                None
            } else {
                Some(idx.min(self.project.timeline.len() - 1))
            };
        }
    }

    fn trim(&mut self, head: bool) {
        let Some(idx) = self.target_item() else {
            self.set_status("no clip under the playhead", StatusKind::Warn);
            return;
        };
        let at = self.playhead;
        let ok = self.edit(|p| {
            if head {
                timeline::trim_head(p, idx, at)
            } else {
                timeline::trim_tail(p, idx, at)
            }
        });
        if ok {
            let last = timeline::last_frame(&self.project.timeline);
            if head {
                // The clip start moved to where the playhead was.
                self.playhead = timeline::item_start(&self.project.timeline, idx).min(last);
            } else {
                self.playhead = self.playhead.min(last);
            }
        } else {
            self.set_status(
                "the playhead must be inside the selected clip, and a clip cannot be trimmed away entirely",
                StatusKind::Warn,
            );
        }
    }

    /// Drop a dragged timeline item at a new index.
    pub fn reorder_item(&mut self, from: usize, to: usize) {
        if self.edit(|p| timeline::reorder(p, from, to)) {
            self.selected_item = Some(to);
            let at = timeline::item_start(&self.project.timeline, to);
            self.focus = Focus::Timeline;
            self.set_position(at);
        }
    }

    fn move_selected(&mut self, delta: isize) {
        let Some(idx) = self.target_item() else {
            self.set_status("no clip under the playhead", StatusKind::Warn);
            return;
        };
        let mut moved_to = None;
        let ok = self.edit(|p| {
            moved_to = timeline::move_item(p, idx, delta);
            moved_to.is_some()
        });
        if ok {
            let new_idx = moved_to.unwrap();
            self.selected_item = Some(new_idx);
            self.playhead = timeline::item_start(&self.project.timeline, new_idx);
        }
    }

    fn after_history_change(&mut self) {
        self.dirty = true;
        // Undo and redo bypass `edit`, so they arm the autosave themselves.
        self.autosave_pending = true;
        self.monitor.set_fps(self.project.fps());
        self.clamp_selection();
    }

    /// Keep the window's position and size up to date in settings, so the next
    /// launch opens where this one left off. Reading viewport info is free and
    /// requests no repaint; the values are written to disk once, on exit.
    fn remember_window_geometry(&mut self, ctx: &egui::Context) {
        let (rect, maximized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.maximized.unwrap_or(false),
                v.fullscreen.unwrap_or(false),
            )
        });
        self.fullscreen = fullscreen;

        let mut geometry = self.settings.window.unwrap_or(crate::settings::WindowGeometry {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
            maximized: false,
        });
        geometry.maximized = maximized;
        // Only record the restored geometry, never the maximised or fullscreen
        // rect — otherwise un-maximising would drop the window at full size.
        if !maximized && !fullscreen {
            if let Some(r) = rect {
                geometry.x = r.min.x;
                geometry.y = r.min.y;
                geometry.width = r.width();
                geometry.height = r.height();
            }
        }
        if geometry.is_plausible() {
            self.settings.window = Some(geometry);
        }
    }

    /// Send a deferred fullscreen toggle. Viewport commands schedule another
    /// pass, so this only ever runs when F11 was actually pressed.
    fn apply_fullscreen(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.fullscreen_pending) {
            return;
        }
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    /// True while a modal is up. Key dispatch pauses so a stray `A` cannot
    /// append to a project the user is in the middle of deciding about.
    pub fn modal_open(&self) -> bool {
        self.recovery.is_some() || self.show_missing_tool || !self.missing_media.is_empty()
    }

    pub fn clamp_selection(&mut self) {
        if let Some(idx) = self.selected_item {
            if idx >= self.project.timeline.len() {
                self.selected_item = if self.project.timeline.is_empty() {
                    None
                } else {
                    Some(self.project.timeline.len() - 1)
                };
            }
        }
        if let Some(id) = self.selected_clip {
            if self.project.clip(id).is_none() {
                self.selected_clip = self.project.clips.first().map(|c| c.id);
                self.source_frame = 0;
            }
        }
        let last = timeline::last_frame(&self.project.timeline);
        self.playhead = self.playhead.clamp(0, last);
        if let Some(c) = self.selected_source() {
            let l = c.last_frame();
            self.source_frame = self.source_frame.clamp(0, l);
        }
    }

    pub fn select_bin_clip(&mut self, id: ClipId) {
        if self.selected_clip == Some(id) {
            return;
        }
        self.monitor.pause();
        self.selected_clip = Some(id);
        self.focus = Focus::Source;
        self.source_frame = self
            .project
            .clip(id)
            .and_then(|c| c.mark_in)
            .unwrap_or(0);
    }

    // --- file operations ----------------------------------------------------

    fn import_dialog(&mut self) {
        if !self.tools.has_ffprobe() {
            self.show_missing_tool = true;
            return;
        }
        let mut dialog = rfd::FileDialog::new().set_title("Import media");
        if let Some(dir) = &self.settings.last_import_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(files) = dialog.pick_files() else {
            return;
        };
        if let Some(dir) = files.first().and_then(|f| f.parent()) {
            self.settings.last_import_dir = Some(dir.to_path_buf());
            self.settings.save();
        }
        self.import_paths(files);
    }

    pub fn import_paths(&mut self, paths: Vec<PathBuf>) {
        if !self.tools.has_ffprobe() {
            self.show_missing_tool = true;
            return;
        }
        let mut n = 0;
        for path in paths {
            for file in expand_drop(&path) {
                self.workers.submit(Job::Probe { path: file });
                n += 1;
            }
        }
        if n == 0 {
            self.set_status("nothing importable in that drop", StatusKind::Warn);
        } else {
            log::info!("probing {n} file(s)");
        }
    }

    /// Startup arguments: at most one project, plus any media to import.
    pub fn open_from_command_line(&mut self, args: &[PathBuf]) {
        let (projects, media): (Vec<_>, Vec<_>) = args.iter().cloned().partition(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(project_io::PROJECT_EXTENSION))
        });
        if let Some(path) = projects.into_iter().next() {
            self.open_project_at(&path);
        }
        if !media.is_empty() {
            self.import_paths(media);
        }
    }

    fn open_project_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open project")
            .add_filter("Roughcut project", &[project_io::PROJECT_EXTENSION]);
        if let Some(dir) = &self.settings.last_project_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        self.open_project_at(&path);
    }

    fn open_project_at(&mut self, path: &Path) {
        let path = path.to_path_buf();
        match project_io::load(&path) {
            Ok(project) => {
                // Opening a project is an explicit choice to work on that
                // instead. The previous snapshot is not destroyed, only
                // superseded on the next edit.
                self.recovery = None;
                self.mark_autosave_saved();
                self.project = project;
                self.history.clear();
                self.project_path = Some(path.clone());
                self.dirty = false;
                self.thumbnails.clear();
                self.thumb_requested.clear();
                self.proxy_state.clear();
                self.workers.clear_queue();
                self.monitor.clear();
                self.monitor.set_fps(self.project.fps());
                self.selected_clip = self.project.clips.first().map(|c| c.id);
                self.selected_item = None;
                self.source_frame = 0;
                self.playhead = 0;
                self.missing_media = project_io::missing_media(&self.project);
                if let Some(dir) = path.parent() {
                    self.settings.last_project_dir = Some(dir.to_path_buf());
                    self.settings.save();
                }
                if self.missing_media.is_empty() {
                    self.set_status(format!("opened {}", path.display()), StatusKind::Info);
                } else {
                    self.set_status(
                        format!("{} clip(s) need relinking", self.missing_media.len()),
                        StatusKind::Warn,
                    );
                }
            }
            Err(e) => self.set_status(format!("{e:#}"), StatusKind::Error),
        }
    }

    fn save_project(&mut self, force_dialog: bool) {
        let path = if force_dialog || self.project_path.is_none() {
            let mut dialog = rfd::FileDialog::new()
                .set_title("Save project")
                .add_filter("Roughcut project", &[project_io::PROJECT_EXTENSION])
                .set_file_name(format!("untitled.{}", project_io::PROJECT_EXTENSION));
            if let Some(dir) = &self.settings.last_project_dir {
                dialog = dialog.set_directory(dir);
            }
            let Some(p) = dialog.save_file() else { return };
            p
        } else {
            self.project_path.clone().unwrap()
        };
        match project_io::save(&self.project, &path) {
            Ok(()) => {
                if let Some(dir) = path.parent() {
                    self.settings.last_project_dir = Some(dir.to_path_buf());
                    self.settings.save();
                }
                self.project_path = Some(path.clone());
                self.dirty = false;
                // The work is in a real file now, so the snapshot no longer
                // needs to prompt — but it is kept.
                self.mark_autosave_saved();
                self.set_status(format!("saved {}", path.display()), StatusKind::Info);
            }
            Err(e) => self.set_status(format!("{e:#}"), StatusKind::Error),
        }
    }

    fn export_dialog(&mut self) {
        if self.project.timeline.is_empty() {
            self.set_status("the timeline is empty — nothing to export", StatusKind::Warn);
            return;
        }
        let default_name = self
            .project_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| format!("{}.mlt", s.to_string_lossy()))
            .unwrap_or_else(|| "roughcut.mlt".to_string());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export MLT XML")
            .add_filter("MLT XML", &["mlt"])
            .set_file_name(default_name);
        if let Some(dir) = &self.settings.last_project_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let opts = ExportOptions::default();
        match mlt::write_to_file(&self.project, &opts, &path) {
            Ok(()) => self.set_status(
                format!(
                    "exported {} clips / {} frames to {}",
                    self.project.timeline.len(),
                    self.timeline_len(),
                    path.display()
                ),
                StatusKind::Info,
            ),
            Err(e) => self.set_status(format!("{e:#}"), StatusKind::Error),
        }
    }

    pub fn relink_dialog(&mut self, id: ClipId) {
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        let name = clip.file_name();
        let Some(path) = rfd::FileDialog::new()
            .set_title(format!("Locate {name}"))
            .pick_file()
        else {
            return;
        };
        if self.edit(|p| project_io::relink(p, id, &path)) {
            self.missing_media.retain(|(mid, _)| *mid != id);
            self.thumbnails.remove(&id);
            self.thumb_requested.remove(&id);
            self.set_status(format!("relinked {name}"), StatusKind::Info);
        }
    }

    pub fn locate_tool_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Locate ffprobe")
            .pick_file()
        else {
            return;
        };
        self.settings.ffprobe_path = Some(path.clone());
        // ffmpeg almost always sits beside ffprobe.
        if self.settings.ffmpeg_path.is_none() {
            let sibling = path.with_file_name(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            });
            if sibling.is_file() {
                self.settings.ffmpeg_path = Some(sibling);
            }
        }
        self.settings.save();
        self.tools = Tools::discover().with_overrides(
            self.settings.ffprobe_path.as_deref(),
            self.settings.ffmpeg_path.as_deref(),
        );
        if self.tools.has_ffprobe() {
            self.show_missing_tool = false;
            self.set_status(
                "ffprobe located — restart is not needed, but already-queued imports were dropped",
                StatusKind::Info,
            );
        }
    }

    // --- background results -------------------------------------------------

    fn drain_workers(&mut self, ctx: &egui::Context) {
        let results: Vec<JobResult> = self.workers.poll().collect();
        for result in results {
            match result {
                JobResult::Probed { path, result } => match result {
                    Ok(info) => self.on_probed(path, info),
                    Err(e) => self.set_status(
                        format!(
                            "{}: {e:#}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        StatusKind::Error,
                    ),
                },
                JobResult::Filmstrip { clip_id, result } => match result {
                    Ok(strip) => {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [strip.width, strip.height],
                            &strip.rgba,
                        );
                        log::debug!(
                            "filmstrip {clip_id}: {}x{} ({} KiB)",
                            strip.width,
                            strip.height,
                            strip.rgba.len() / 1024
                        );
                        let handle = ctx.load_texture(
                            format!("strip-{clip_id}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.thumbnails.insert(clip_id, handle);
                    }
                    Err(e) => log::warn!("filmstrip for {clip_id}: {e:#}"),
                },
                JobResult::ProxyStarted { clip_id } => {
                    self.proxy_state.insert(clip_id, ProxyState::Running);
                }
                JobResult::ProxyDone { clip_id, result } => match result {
                    Ok(path) => {
                        self.proxy_state.remove(&clip_id);
                        // Not an undoable edit: a proxy is a cache, not content.
                        if let Some(c) = self.project.clip_mut(clip_id) {
                            c.proxy_path = Some(path);
                        }
                    }
                    Err(e) => {
                        self.proxy_state.insert(clip_id, ProxyState::Failed);
                        self.set_status(format!("proxy: {e:#}"), StatusKind::Warn);
                    }
                },
            }
        }
    }

    fn on_probed(&mut self, path: PathBuf, info: MediaInfo) {
        let was_empty = self.project.clips.is_empty();
        let mut new_id = None;
        self.edit(|p| match add_clip(p, &path, &info) {
            ImportOutcome::Added(id) => {
                new_id = Some(id);
                true
            }
            ImportOutcome::Duplicate(_) => false,
        });
        let Some(id) = new_id else {
            log::info!(
                "{} is already in the bin",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            return;
        };

        if was_empty {
            self.monitor.set_fps(self.project.fps());
            self.selected_clip = Some(id);
            self.set_status(
                format!("project profile set to {}", self.project.profile.description()),
                StatusKind::Info,
            );
        }
        if self
            .project
            .clip(id)
            .is_some_and(|c| c.rate_mismatch)
        {
            self.set_status(
                format!(
                    "{} runs at {} but the project is {} — MLT will resample it",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    info.fps,
                    self.project.fps()
                ),
                StatusKind::Warn,
            );
        }

        self.request_thumbnail(id);

        if self.settings.proxies_enabled {
            if let Some(dir) = self.settings.resolve_proxy_dir(self.project_path.as_deref()) {
                self.proxy_state.insert(id, ProxyState::Queued);
                self.workers.submit(Job::Proxy {
                    clip_id: id,
                    source: path,
                    info: Box::new(info),
                    proxy_dir: dir,
                });
            } else {
                self.set_status(
                    "save the project before generating proxies, so they have somewhere to live",
                    StatusKind::Warn,
                );
            }
        }
    }

    fn request_thumbnail(&mut self, id: ClipId) {
        if self.thumbnails.contains_key(&id) || !self.thumb_requested.insert(id) {
            return;
        }
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        if !self.tools.has_ffmpeg() {
            return;
        }
        self.workers.submit(Job::Filmstrip {
            clip_id: id,
            path: clip.path.clone(),
            duration_frames: clip.duration_frames,
            fps: self.project.fps(),
        });
    }

    fn request_missing_thumbnails(&mut self) {
        let ids: Vec<ClipId> = self
            .project
            .clips
            .iter()
            .filter(|c| !self.thumbnails.contains_key(&c.id))
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.request_thumbnail(id);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.import_paths(dropped);
        }
    }
}

impl eframe::App for RoughcutApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Roughcut owns the keyboard. egui's Tab-navigation would otherwise
        // park focus on whatever widget it reached and, because
        // `wants_keyboard_input` means "anything is focused", make the app
        // stop responding to keys. Nothing here needs widget focus, so it is
        // surrendered every pass.
        ctx.memory_mut(|m| m.stop_text_input());
        self.remember_window_geometry(ctx);

        // Background work stops while the window is not focused (§3).
        let focused = ctx.input(|i| i.focused);
        self.workers.set_suspended(!focused);

        self.handle_dropped_files(ctx);
        self.drain_workers(ctx);
        self.monitor.pump_events();
        self.advance_playback();
        self.request_missing_thumbnails();

        if !self.modal_open() {
            for action in keys::actions_this_frame(ctx) {
                self.dispatch(action);
            }
        }

        ui::status::show(self, ctx);
        ui::timeline::show(self, ctx);
        ui::bin::show(self, ctx);
        ui::monitor_panel::show(self, ctx);
        ui::help::show(self, ctx);
        ui::dialogs::show(self, ctx);

        // Tell mpv what to display, after the UI has settled this frame's
        // playhead. Nothing is sent when the position has not moved.
        match self.current_media() {
            Some((path, position)) => {
                if self.monitor.ensure_started(self.settings.mpv_path.as_deref()) {
                    // eframe makes the GL context current before calling
                    // `update`, so this is a legal place to build mpv's render
                    // context — and it keeps the paint callback, which has to
                    // be `Send + Sync`, from needing to borrow the player.
                    if frame.gl().is_some() {
                        if let (Some(player), Ok(mut video)) =
                            (self.monitor.player(), self.video.lock())
                        {
                            video.ensure_context(player, self.repaint.clone());
                        }
                    }
                    if std::mem::take(&mut self.force_media_jump) {
                        self.monitor.jump(&path, position);
                    } else {
                        self.monitor.show(&path, position);
                    }
                }
            }
            None => self.monitor.clear(),
        }

        self.apply_fullscreen(ctx);

        // One write per pass at most, and only when something actually
        // changed. Passes happen on input, so this costs nothing at idle.
        self.flush_autosave();

        let title = self.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        // Scheduled repaints, and the only two of them. Both stop once the
        // thing they are waiting on is done, so idle returns to no repaints at
        // all and the process back to using no CPU.
        if let Some(interval) = self.monitor.repaint_interval() {
            // A tick per frame while the transport is running.
            ctx.request_repaint_after(interval);
        }
        if let Some(remaining) = self.expire_status() {
            // Come back when the message is due to disappear.
            ctx.request_repaint_after(remaining);
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        // The GL context is still current here, which is the only safe place
        // to destroy mpv's render context and our FBO.
        if let Some(gl) = gl {
            if let Ok(mut video) = self.video.lock() {
                video.destroy(gl);
            }
        }
        let _ = self.monitor.shutdown();
        self.settings.save();

        // The snapshot is always left behind, marked according to whether it
        // holds unsaved work. Quitting dirty prompts on next launch; quitting
        // clean does not, but the session is still reachable from File. That
        // is why there is no "are you sure?" dialog to dismiss every time you
        // quit on purpose.
        self.autosave_pending = true;
        self.flush_autosave();
    }
}
