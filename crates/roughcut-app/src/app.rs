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

use roughcut_core::import::{add_clip, remeasure, ImportOutcome};
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::{ClipId, Project, TimelineItem};
use roughcut_core::probe::MediaInfo;
use roughcut_core::project_io;
use roughcut_core::rotate::Turn;
use roughcut_core::time::{format_timecode, Rational};
use roughcut_core::timeline::{self, Edge};
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

/// Which of a source clip's two marks a drag has hold of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkEdge {
    In,
    Out,
}

/// A timeline clip being retrimmed by dragging one of its edges.
#[derive(Debug, Clone, Copy)]
pub struct TrimDrag {
    pub index: usize,
    pub edge: Edge,
    /// Frames moved so far, already clamped to what the footage allows.
    pub delta: i64,
}

/// A texture of tiles, plus how to index it.
pub struct Thumb {
    pub tex: egui::TextureHandle,
    pub cols: usize,
    pub rows: usize,
    pub tiles: usize,
    /// Value of `sheet_clock` when this was last drawn.
    last_seen: std::cell::Cell<u64>,
}

impl Thumb {
    /// The sub-rectangle of the texture holding `tile`, in UV space.
    pub fn uv(&self, tile: usize) -> egui::Rect {
        let tile = tile.min(self.tiles.saturating_sub(1));
        let (cw, ch) = (1.0 / self.cols as f32, 1.0 / self.rows as f32);
        let (cx, cy) = ((tile % self.cols) as f32, (tile / self.cols) as f32);
        egui::Rect::from_min_size(
            egui::pos2(cx * cw, cy * ch),
            egui::vec2(cw, ch),
        )
    }

    /// Aspect ratio of one tile, needed to letterbox it correctly.
    pub fn tile_aspect(&self) -> f32 {
        let size = self.tex.size_vec2();
        let (tw, th) = (size.x / self.cols as f32, size.y / self.rows as f32);
        if th > 0.0 {
            tw / th
        } else {
            16.0 / 9.0
        }
    }

    pub fn touch(&self, clock: u64) {
        self.last_seen.set(clock);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// A project Shotcut opens, referring to the original files. Instant.
    Mlt,
    /// A finished video, encoded by melt. Minutes.
    Mp4,
}

/// What the export dialog is currently set to.
///
/// Initialised from the profile suggested by the clips actually used, and
/// entirely overridable — the suggestion is a starting point, not a decision.
#[derive(Debug, Clone)]
pub struct ExportPlan {
    pub format: ExportFormat,
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    /// The suggestion, kept so the dialog can offer it back after fiddling.
    pub suggested: roughcut_core::Profile,
}

impl ExportPlan {
    /// The profile these choices describe, keeping the picture properties of
    /// the suggestion — nobody wants to choose a colourspace.
    pub fn profile(&self) -> roughcut_core::Profile {
        roughcut_core::Profile {
            frame_rate_num: self.fps.num,
            frame_rate_den: self.fps.den,
            width: self.width.max(2),
            height: self.height.max(2),
            ..self.suggested.clone()
        }
    }
}

pub struct RenderState {
    pub out: PathBuf,
    pub total: i64,
    pub frame: i64,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    rx: crossbeam_channel::Receiver<RenderMsg>,
    /// The MLT handed to melt, removed once it is finished with.
    scratch: PathBuf,
}

enum RenderMsg {
    Progress(i64),
    Done(Result<(), String>),
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
    /// The middle button is held and panning the timeline.
    pub panning: bool,
    /// Set when the playhead crosses a cut, so the next media sync forces mpv
    /// to the new position instead of letting it keep playing where it was.
    force_media_jump: bool,
    /// A copied range, waiting to be pasted. Roughcut's own, not the system
    /// clipboard: what is being copied is a reference to part of a file, which
    /// means nothing outside this application.
    clipboard: Option<TimelineItem>,
    /// The export dialog's pending choices, while it is open.
    pub export_plan: Option<ExportPlan>,
    /// A render in flight. One at a time: it is a foreground operation with a
    /// progress bar, not background work.
    pub render: Option<RenderState>,
    /// The timeline written out for mpv, and the cut list it describes. The
    /// signature is in the file name so that editing the timeline yields a
    /// path mpv has not seen — rewriting one path in place would leave it
    /// playing the old cut.
    edl: Option<(u64, PathBuf)>,
    /// A shift-drag in progress on the source scrub bar, as (anchor, current)
    /// source frames. Marks are only committed on release, so the drag costs
    /// one undo entry rather than one per pixel.
    pub mark_drag: Option<(i64, i64)>,
    /// One mark being dragged by its handle, as (which end, where it is now).
    /// Committed on release for the same reason `mark_drag` is.
    pub mark_grab: Option<(MarkEdge, i64)>,
    /// A timeline clip being retrimmed by its edge, as (item, which end, how
    /// far it has moved in frames). The move is previewed from here and only
    /// written to the project when the button comes up.
    pub trim_drag: Option<TrimDrag>,
    /// Mirrors the viewport's fullscreen state, so F11 toggles rather than
    /// guessing. `fullscreen_pending` defers the actual viewport command to
    /// `update`, which is the only place with a `Context` to send it on.
    pub fullscreen: bool,
    fullscreen_pending: bool,

    /// Files submitted for probing, in the order they were chosen, and the
    /// results that have come back. Probes finish out of order on the worker
    /// pool, so results are held here and applied strictly in order — the bin
    /// then matches the order you picked, and, far more importantly, the
    /// project profile is taken from the clip you actually chose first rather
    /// than from whichever one probed fastest.
    import_order: Vec<PathBuf>,
    probe_results: HashMap<PathBuf, Result<MediaInfo, String>>,

    /// One frame per clip, shown in the bin and on timeline blocks at rest.
    /// Tiny — a few tens of kilobytes each — so every clip keeps one.
    pub posters: HashMap<ClipId, Thumb>,
    /// The dense sheets hover-scrubbing indexes into. Each is a couple of
    /// megabytes, so only the most recently used are kept resident.
    pub sheets: HashMap<ClipId, Thumb>,
    /// One loudness envelope per clip that has been opened in the monitor, at
    /// a byte a bucket. Two kilobytes each, so unlike the sheets these are
    /// never evicted — a bin of a thousand clips would still be 2 MB.
    pub waveforms: HashMap<ClipId, Vec<u8>>,
    waveform_requested: HashSet<ClipId>,
    /// Bumped every frame, and stamped on a sheet whenever it is drawn, so
    /// the least recently *seen* sheet can be identified for eviction.
    sheet_clock: u64,
    thumb_requested: HashSet<ClipId>,
    sheet_requested: HashSet<ClipId>,
    pub proxy_state: HashMap<ClipId, ProxyState>,
    /// Clips whose file is being rewritten on disk right now. Rotating twice
    /// at once would have two ffmpeg processes racing for the same path.
    pub rotating: HashSet<ClipId>,

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
            panning: false,
            mark_drag: None,
            mark_grab: None,
            trim_drag: None,
            force_media_jump: false,
            clipboard: None,
            export_plan: None,
            render: None,
            edl: None,
            fullscreen: false,
            fullscreen_pending: false,
            import_order: Vec::new(),
            probe_results: HashMap::new(),
            posters: HashMap::new(),
            sheets: HashMap::new(),
            waveforms: HashMap::new(),
            waveform_requested: HashSet::new(),
            sheet_clock: 0,
            thumb_requested: HashSet::new(),
            sheet_requested: HashSet::new(),
            proxy_state: HashMap::new(),
            rotating: HashSet::new(),
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

    /// The best tiles available for a clip: its scrub sheet if one is
    /// resident, otherwise the single poster frame. Drawing through this is
    /// what keeps the sheet's place in the eviction order up to date.
    pub fn thumb(&self, id: ClipId) -> Option<&Thumb> {
        let t = self.sheets.get(&id).or_else(|| self.posters.get(&id))?;
        t.touch(self.sheet_clock);
        Some(t)
    }

    /// Drop the sheets nobody has looked at recently.
    ///
    /// A hundred clips of 112 tiles is a quarter of a gigabyte, well past the
    /// budget, and almost all of it is for clips scrolled out of sight. The
    /// posters stay, so every clip still shows a picture; only the ability to
    /// scrub a long-untouched clip is given up, and it comes back from the
    /// disk cache the moment it is wanted.
    fn evict_stale_sheets(&mut self) {
        const MAX_RESIDENT: usize = 48;
        while self.sheets.len() > MAX_RESIDENT {
            let Some(oldest) = self
                .sheets
                .iter()
                .min_by_key(|(_, t)| t.last_seen.get())
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.sheets.remove(&oldest);
            self.sheet_requested.remove(&oldest);
        }
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

    /// Write the timeline out for mpv, if it has changed since last time.
    ///
    /// Cheap to call every pass: the signature covers only what mpv would
    /// play, so flagging a clip or renaming the project does not rewrite it.
    fn sync_edl(&mut self) {
        if self.project.timeline.is_empty() {
            self.edl = None;
            return;
        }
        let sig = roughcut_core::edl::signature(&self.project);
        // Signature only: this runs every pass, and a filesystem stat per
        // frame is exactly the kind of idle cost this application does not pay.
        if self.edl.as_ref().is_some_and(|(s, _)| *s == sig) {
            return;
        }
        let Some(text) = roughcut_core::edl::to_text(&self.project) else {
            self.edl = None;
            return;
        };
        let Some(dir) = crate::settings::config_dir() else {
            return;
        };
        let path = dir.join(format!("timeline-{sig:016x}.edl"));
        if let Err(e) = project_io::atomic_write(&path, text.as_bytes()) {
            log::warn!("cannot write the timeline for playback: {e}");
            self.edl = None;
            return;
        }
        // Only the current one is of any use; the rest are last edit's.
        if let Some(old) = self.edl.replace((sig, path)) {
            let _ = std::fs::remove_file(old.1);
        }
    }

    /// The file and frame the monitor should be showing right now.
    pub fn current_media(&self) -> Option<(PathBuf, i64)> {
        match self.focus {
            Focus::Source => {
                let clip = self.selected_source()?;
                let f = self.source_frame.clamp(0, clip.last_frame());
                Some((clip.playback_path().to_path_buf(), f))
            }
            // The whole cut list, as one stream. mpv can then open the next
            // segment before the current one ends, which is the only way the
            // joins stop being audible and visible.
            Focus::Timeline => {
                let path = self.edl.as_ref().map(|(_, p)| p.clone())?;
                Some((path, self.playhead.max(0)))
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
                // A seek issued mid-playback has not landed yet, and mpv is
                // still reporting where it was. Adopting that would drag the
                // playhead straight back off the point just clicked on.
                if self.monitor.is_seeking() {
                    return;
                }
                let Some(reported) = self.monitor.playback_frame() else {
                    return;
                };
                let last = self.position_max();
                self.set_position(reported);
                // The end really is the end, for a bin clip and for the cut
                // list alike — mpv is playing one stream in both cases.
                if reported >= last || self.monitor.eof {
                    self.monitor.pause();
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
            Action::TogglePlay => {
                // Play from the top when there is nothing left to play.
                // Otherwise the key does nothing at all at the end of a clip,
                // which reads as the application having stopped responding.
                if matches!(self.monitor.transport, Transport::Paused)
                    && self.position() >= self.position_max()
                {
                    self.set_position(0);
                }
                self.monitor.toggle_play();
            }
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

            Action::Copy => self.copy_selection(false),
            Action::Cut => self.copy_selection(true),
            Action::Paste => self.paste(),
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

            Action::NewProject => self.new_project(),
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

    /// Copy what is under the playhead, optionally lifting it out.
    ///
    /// Works from either region, because both have an obvious answer to "what
    /// is selected": on the timeline it is the cut under the playhead, and in
    /// the bin it is the marked range — the same range `A` would append. Only
    /// the timeline can be cut, since there is nothing in the bin to remove.
    fn copy_selection(&mut self, lift: bool) {
        let item = match self.focus {
            Focus::Timeline => self.target_item().map(|i| self.project.timeline[i]),
            Focus::Source => self
                .marked_range()
                .map(|(clip_id, in_frame, out_frame)| TimelineItem {
                    clip_id,
                    in_frame,
                    out_frame,
                }),
        };
        let Some(item) = item else {
            self.set_status(
                match self.focus {
                    Focus::Timeline => "no clip under the playhead",
                    Focus::Source => "nothing marked to copy",
                },
                StatusKind::Warn,
            );
            return;
        };
        self.clipboard = Some(item);

        let name = self
            .project
            .clip(item.clip_id)
            .map(|c| c.file_name())
            .unwrap_or_default();
        let len = format_timecode(item.len(), self.project.fps());
        if lift && self.focus == Focus::Timeline {
            self.ripple_delete();
            self.set_status(format!("cut {len} of {name}"), StatusKind::Info);
        } else {
            self.set_status(format!("copied {len} of {name}"), StatusKind::Info);
        }
    }

    /// Drop the copied range in at the playhead, rippling everything after it.
    ///
    /// Always inserts rather than overwrites, which is the only behaviour an
    /// assembly editor with one track can offer without silently destroying
    /// something.
    fn paste(&mut self) {
        let Some(item) = self.clipboard else {
            self.set_status("nothing copied yet", StatusKind::Warn);
            return;
        };
        // The clip it refers to can have been removed from the bin since.
        if self.project.clip(item.clip_id).is_none() {
            self.clipboard = None;
            self.set_status("the copied clip is no longer in the bin", StatusKind::Warn);
            return;
        }
        self.focus = Focus::Timeline;
        let at = self.playhead;
        let mut landed = None;
        let ok = self.edit(|p| {
            landed = timeline::insert_at(p, at, item.clip_id, item.in_frame, item.out_frame);
            landed.is_some()
        });
        if ok {
            let at = landed.unwrap_or(at);
            self.selected_item = timeline::item_at(&self.project.timeline, at).map(|(i, _)| i);
            // Leave the playhead at the end of what was just pasted, so
            // pasting twice lays two cuts down in order rather than on top of
            // each other.
            self.playhead =
                (at + item.len()).min(timeline::last_frame(&self.project.timeline));
        }
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
            self.posters.remove(&id);
            self.sheets.remove(&id);
            self.sheet_requested.remove(&id);
            self.thumb_requested.remove(&id);
            self.proxy_state.remove(&id);
            self.selected_clip = next;
            self.source_frame = 0;
            self.monitor.clear();
            log::info!("removed {name}");
        }
    }

    /// Commit a shift-drag on the scrub bar as the clip's in and out points.
    /// Move the playhead because the pointer said so, without stopping.
    ///
    /// Clicking the bar used to pause, which made playing and looking at the
    /// same time impossible: every attempt to jump somewhere killed the thing
    /// being watched. Seeking and playing are separate ideas, so this only
    /// does the one that was asked for.
    ///
    /// While mpv is playing it owns its own position, and `show` deliberately
    /// leaves it alone; the new position therefore has to be pushed at it
    /// explicitly. One seek at a time — dragging produces a target per pixel,
    /// and every one of them but the last is already out of date.
    pub fn scrub_to(&mut self, frame: i64) {
        self.set_position(frame);
        if self.monitor.transport.is_playing() && !self.monitor.is_seeking() {
            self.force_media_jump = true;
        }
    }

    /// The last word on where a scrub ended, issued even if a seek is already
    /// in flight — otherwise a drag can finish on a frame mpv never hears
    /// about, and playback carries on from wherever the last seek landed.
    pub fn scrub_settled(&mut self) {
        if self.monitor.transport.is_playing() {
            self.force_media_jump = true;
        }
    }

    /// Start dragging one end of the marked range.
    pub fn grab_mark(&mut self, edge: MarkEdge, frame: i64) {
        self.mark_grab = Some((edge, frame));
        self.scrub_to(frame);
    }

    /// Move a grabbed mark, keeping it on its own side of the other one.
    pub fn drag_mark(&mut self, frame: i64) {
        let Some((edge, _)) = self.mark_grab else {
            return;
        };
        let Some(clip) = self.selected_source() else {
            return;
        };
        let frame = match edge {
            MarkEdge::In => frame.min(clip.mark_out.unwrap_or(clip.last_frame())),
            MarkEdge::Out => frame.max(clip.mark_in.unwrap_or(0)),
        }
        .clamp(0, clip.last_frame());
        self.mark_grab = Some((edge, frame));
        // The monitor follows the handle: a mark you cannot see the frame of
        // is a mark you are placing blind.
        self.scrub_to(frame);
    }

    pub fn commit_mark_grab(&mut self) {
        let Some((edge, frame)) = self.mark_grab.take() else {
            return;
        };
        let Some(id) = self.selected_clip else { return };
        self.edit(|p| {
            let Some(c) = p.clip_mut(id) else { return false };
            match edge {
                MarkEdge::In if c.mark_in != Some(frame) => c.mark_in = Some(frame),
                MarkEdge::Out if c.mark_out != Some(frame) => c.mark_out = Some(frame),
                _ => return false,
            }
            true
        });
    }

    /// Take hold of a timeline clip's head or tail.
    pub fn grab_trim(&mut self, index: usize, edge: Edge) {
        self.trim_drag = Some(TrimDrag {
            index,
            edge,
            delta: 0,
        });
    }

    /// Move a grabbed edge, clamped to what the source behind it allows.
    pub fn drag_trim(&mut self, delta: i64) {
        let Some(drag) = self.trim_drag else { return };
        let delta = timeline::clamp_trim(&self.project, drag.index, drag.edge, delta);
        self.trim_drag = Some(TrimDrag { delta, ..drag });
    }

    pub fn commit_trim(&mut self) {
        let Some(drag) = self.trim_drag.take() else {
            return;
        };
        if !self.edit(|p| timeline::trim_edge(p, drag.index, drag.edge, drag.delta)) {
            return;
        }
        self.selected_item = Some(drag.index);
        self.focus = Focus::Timeline;
        // Land on the edge that just moved, so the frame now at the cut is the
        // one on screen — the whole reason for retrimming by hand.
        let start = timeline::item_start(&self.project.timeline, drag.index);
        let at = match drag.edge {
            Edge::Head => start,
            Edge::Tail => start + self.project.timeline[drag.index].len() - 1,
        };
        self.set_position(at);
        self.force_media_jump = true;
    }

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
        self.recovery.is_some()
            || self.show_missing_tool
            || !self.missing_media.is_empty()
            || self.export_plan.is_some()
            || self.render.is_some()
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
        // Touching the bin always focuses the source, *including* when the
        // clip was already the selected one. Anything else and a key that acts
        // on "whichever region has focus" — Delete most visibly — keeps acting
        // on the timeline, and clicking the clip again does not fix it because
        // there is nothing to change.
        self.focus = Focus::Source;

        if self.selected_clip == Some(id) {
            return;
        }
        self.monitor.pause();
        self.selected_clip = Some(id);
        self.source_frame = self
            .project
            .clip(id)
            .and_then(|c| c.mark_in)
            .unwrap_or(0);
    }

    /// Turn a clip's file a quarter turn on disk.
    ///
    /// This edits the user's original file, which nothing else in Roughcut
    /// does — but it is the only fix that survives leaving the program, and
    /// footage that arrives on its side is otherwise useless everywhere, not
    /// just here. It is safe to do mid-edit because rewriting the orientation
    /// is a stream copy: the frame count and rate cannot move, so marks and
    /// cuts made before the rotation still name the same frames after it.
    pub fn rotate_clip(&mut self, id: ClipId, turn: Turn) {
        if !self.tools.has_ffmpeg() || self.tools.ffprobe.is_none() {
            self.set_status(
                "rotating needs ffmpeg and ffprobe, which were not found",
                StatusKind::Warn,
            );
            return;
        }
        if !self.rotating.insert(id) {
            return; // already turning
        }
        let Some(clip) = self.project.clip(id) else {
            self.rotating.remove(&id);
            return;
        };
        let path = clip.path.clone();
        if !path.exists() {
            self.rotating.remove(&id);
            self.set_status(
                format!("{} is missing — relink it first", clip.file_name()),
                StatusKind::Warn,
            );
            return;
        }
        self.set_status(format!("rotating {}…", clip.file_name()), StatusKind::Info);
        self.workers.submit(Job::Rotate {
            clip_id: id,
            path,
            turn,
        });
    }

    /// Adopt the new orientation of a file Roughcut just rewrote.
    ///
    /// Everything derived from the old orientation is thrown away: the
    /// filmstrip, and the proxy, which was encoded the wrong way round and
    /// would otherwise keep showing the clip on its side during playback.
    fn on_rotated(&mut self, id: ClipId, info: MediaInfo) {
        self.rotating.remove(&id);

        let name = self.project.clip(id).map(|c| c.file_name()).unwrap_or_default();
        self.edit(|p| {
            let Some(clip) = p.clip_mut(id) else {
                return false;
            };
            clip.width = info.width;
            clip.height = info.height;
            clip.sample_aspect_num = info.sample_aspect_num;
            clip.sample_aspect_den = info.sample_aspect_den;
            true
        });
        // A different frame shape can change what the project should be; the
        // usual rule decides whether it is still free to move.
        roughcut_core::profile::refresh_working(&mut self.project);

        // Drop the stale proxy and filmstrip, then rebuild both.
        let stale_proxy = self.project.clip(id).and_then(|c| c.proxy_path.clone());
        if let Some(p) = stale_proxy {
            let _ = std::fs::remove_file(&p);
            if let Some(c) = self.project.clip_mut(id) {
                c.proxy_path = None;
            }
        }
        self.posters.remove(&id);
            self.sheets.remove(&id);
            self.sheet_requested.remove(&id);
        self.thumb_requested.remove(&id);
        self.proxy_state.remove(&id);
        self.request_thumbnail(id);
        let (w, h) = (info.width, info.height);
        self.request_proxy(id, info);

        // Whatever is on screen is now the wrong way round.
        self.monitor.reload();

        self.set_status(format!("{name} is now {w}x{h}"), StatusKind::Info);
    }

    /// Toggle whether a clip is flagged as worth using.
    ///
    /// Undoable, like any other edit to the project — flagging a folder of
    /// footage is real work, and losing it to a misclick would be as annoying
    /// as losing a cut.
    pub fn toggle_flag(&mut self, id: ClipId) {
        let mut now = false;
        self.edit(|p| match p.clip_mut(id) {
            Some(c) => {
                c.flagged = !c.flagged;
                now = c.flagged;
                true
            }
            None => false,
        });
        let name = self.project.clip(id).map(|c| c.file_name()).unwrap_or_default();
        self.set_status(
            if now {
                format!("flagged {name}")
            } else {
                format!("unflagged {name}")
            },
            StatusKind::Info,
        );
    }

    /// Open the system file manager with this clip selected.
    pub fn reveal_clip(&mut self, id: ClipId) {
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        let path = clip.path.clone();
        if !path.exists() {
            self.set_status(
                format!("{} is not where the project expects it", clip.file_name()),
                StatusKind::Warn,
            );
            return;
        }
        if let Err(e) = reveal(&path) {
            self.set_status(format!("cannot show that folder: {e}"), StatusKind::Warn);
        }
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
                self.import_order.push(file.clone());
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

    /// Start again with an empty project.
    ///
    /// This is also the only way to change the project's format: the profile
    /// is fixed by the first clip imported and never moves, because every
    /// position in the application is a frame number in profile time and
    /// changing the rate underneath them would silently invalidate the lot.
    fn new_project(&mut self) {
        // Make sure whatever is on screen is recoverable before it goes. The
        // snapshot is not deleted, so File > Recover last session can reach it
        // until the new project's first edit overwrites it.
        let had_unsaved = self.dirty;
        if had_unsaved {
            self.autosave_pending = true;
            self.flush_autosave();
        }

        self.workers.clear_queue();
        self.import_order.clear();
        self.probe_results.clear();
        self.posters.clear();
                self.sheets.clear();
                self.sheet_requested.clear();
        self.thumb_requested.clear();
                self.sheet_requested.clear();
        self.proxy_state.clear();
        self.missing_media.clear();
        self.monitor.clear();

        self.project = Project::new();
        self.history.clear();
        self.project_path = None;
        self.dirty = false;
        self.selected_clip = None;
        self.selected_item = None;
        self.source_frame = 0;
        self.playhead = 0;
        self.focus = Focus::Source;
        self.zoom_fit = true;
        self.monitor.set_fps(self.project.fps());

        if had_unsaved {
            self.set_status(
                "new project — the previous unsaved work is under File ▸ Recover                  last session until you change something here",
                StatusKind::Warn,
            );
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

    pub fn open_recent(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.open_project_at(&path);
    }

    pub fn clear_recents(&mut self) {
        self.settings.recent_projects.clear();
        self.settings.save();
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
                self.posters.clear();
                self.sheets.clear();
                self.sheet_requested.clear();
                self.thumb_requested.clear();
                self.sheet_requested.clear();
                self.proxy_state.clear();
                self.workers.clear_queue();
                self.monitor.clear();
                self.monitor.set_fps(self.project.fps());
                self.selected_clip = self.project.clips.first().map(|c| c.id);
                self.selected_item = None;
                self.source_frame = 0;
                self.playhead = 0;
                self.missing_media = project_io::missing_media(&self.project);
                self.revalidate_clips();
                if let Some(dir) = path.parent() {
                    self.settings.last_project_dir = Some(dir.to_path_buf());
                }
                self.settings.remember_recent(&path);
                self.settings.save();
                if self.missing_media.is_empty() {
                    self.set_status(format!("opened {}", path.display()), StatusKind::Info);
                } else {
                    self.set_status(
                        format!("{} clip(s) need relinking", self.missing_media.len()),
                        StatusKind::Warn,
                    );
                }
            }
            Err(e) => {
                // A path that no longer loads is not worth offering again.
                self.settings.forget_recent(&path);
                self.settings.save();
                self.set_status(format!("{e:#}"), StatusKind::Error);
            }
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
                }
                self.settings.remember_recent(&path);
                self.settings.save();
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

    /// Open the export dialog, set to what the used clips suggest.
    fn export_dialog(&mut self) {
        if self.project.timeline.is_empty() {
            self.set_status("the timeline is empty — nothing to export", StatusKind::Warn);
            return;
        }
        if self.render.is_some() {
            self.set_status("a render is already running", StatusKind::Warn);
            return;
        }
        let suggested = roughcut_core::profile::for_export(&self.project);
        self.export_plan = Some(ExportPlan {
            format: ExportFormat::Mlt,
            width: suggested.width,
            height: suggested.height,
            fps: suggested.fps(),
            suggested,
        });
    }

    /// Carry out whatever the dialog was set to.
    pub fn run_export(&mut self, plan: ExportPlan) {
        self.export_plan = None;
        if self.project.timeline.is_empty() {
            return;
        }
        let ext = match plan.format {
            ExportFormat::Mlt => "mlt",
            ExportFormat::Mp4 => "mp4",
        };
        let stem = self
            .project_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "roughcut".to_string());
        let mut dialog = rfd::FileDialog::new()
            .set_title(match plan.format {
                ExportFormat::Mlt => "Export MLT XML",
                ExportFormat::Mp4 => "Export MP4",
            })
            .add_filter(
                match plan.format {
                    ExportFormat::Mlt => "MLT XML",
                    ExportFormat::Mp4 => "MP4 video",
                },
                &[ext],
            )
            .set_file_name(format!("{stem}.{ext}"));
        if let Some(dir) = &self.settings.last_project_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };

        let opts = ExportOptions {
            profile: Some(plan.profile()),
            ..ExportOptions::default()
        };
        match plan.format {
            ExportFormat::Mlt => match mlt::write_to_file(&self.project, &opts, &path) {
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
            },
            ExportFormat::Mp4 => self.start_render(&opts, &plan, &path),
        }
    }

    /// Hand the cut to melt, on a thread of its own.
    ///
    /// Not a worker-pool job: those are background work nobody is waiting on,
    /// suspended when the window loses focus and run at the lowest priority
    /// the OS offers. A render is the opposite of all three.
    fn start_render(&mut self, opts: &ExportOptions, plan: &ExportPlan, out: &Path) {
        let Some(melt) = self.tools.melt.clone() else {
            self.set_status(
                "melt was not found — it comes with Shotcut, and is what does the encoding",
                StatusKind::Error,
            );
            return;
        };
        // The XML melt reads is scratch, not something the user asked for.
        let Some(dir) = crate::settings::config_dir() else {
            self.set_status("nowhere to write the render's project file", StatusKind::Error);
            return;
        };
        let scratch = dir.join("render.mlt");
        if let Err(e) = mlt::write_to_file(&self.project, opts, &scratch) {
            self.set_status(format!("{e:#}"), StatusKind::Error);
            return;
        }

        // Length in the *export* rate, which is what melt will count in.
        let total = {
            let frames = self.timeline_len();
            let from = self.project.fps();
            let to = plan.fps;
            roughcut_core::time::convert_frames(frames, from, to).max(1)
        };

        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let repaint = self.repaint.clone();
        let (c, o, sc) = (cancel.clone(), out.to_path_buf(), scratch.clone());
        std::thread::Builder::new()
            .name("roughcut-render".into())
            .spawn(move || {
                let tx2 = tx.clone();
                let r2 = repaint.clone();
                let result = roughcut_core::render::to_mp4(&melt, &sc, &o, &c, move |f| {
                    let _ = tx2.send(RenderMsg::Progress(f));
                    r2();
                });
                let _ = tx.send(RenderMsg::Done(result.map_err(|e| format!("{e:#}"))));
                repaint();
            })
            .expect("cannot spawn the render thread");

        self.render = Some(RenderState {
            out: out.to_path_buf(),
            total,
            frame: 0,
            cancel,
            rx,
            scratch,
        });
        self.set_status(
            format!("rendering {} frames to {}", total, out.display()),
            StatusKind::Info,
        );
    }

    /// Pick up whatever the render thread has said since last pass.
    fn drain_render(&mut self) {
        let Some(state) = &mut self.render else {
            return;
        };
        let mut finished = None;
        while let Ok(msg) = state.rx.try_recv() {
            match msg {
                RenderMsg::Progress(f) => state.frame = f,
                RenderMsg::Done(r) => finished = Some(r),
            }
        }
        let Some(result) = finished else {
            return;
        };
        let state = self.render.take().expect("checked above");
        let _ = std::fs::remove_file(&state.scratch);
        match result {
            Ok(()) => self.set_status(
                format!("rendered {}", state.out.display()),
                StatusKind::Info,
            ),
            Err(e) if e.contains("cancelled") => {
                self.set_status("render cancelled", StatusKind::Warn)
            }
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    pub fn cancel_render(&mut self) {
        if let Some(state) = &self.render {
            state
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
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
            self.posters.remove(&id);
            self.sheets.remove(&id);
            self.sheet_requested.remove(&id);
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
                JobResult::Probed { path, result } => {
                    self.probe_results
                        .insert(path, result.map_err(|e| format!("{e:#}")));
                    self.drain_probes();
                }
                JobResult::Thumbs { clip_id, result } => match result {
                    Ok(sheet) => {
                        let poster = sheet.tiles <= 1;
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [sheet.width, sheet.height],
                            &sheet.rgba,
                        );
                        log::debug!(
                            "sheet {clip_id}: {}x{}, {} tiles ({} KiB)",
                            sheet.width,
                            sheet.height,
                            sheet.tiles,
                            sheet.rgba.len() / 1024
                        );
                        let handle = ctx.load_texture(
                            format!("tiles-{clip_id}-{}", sheet.tiles),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        let thumb = Thumb {
                            tex: handle,
                            cols: sheet.cols,
                            rows: sheet.rows,
                            tiles: sheet.tiles,
                            last_seen: std::cell::Cell::new(self.sheet_clock),
                        };
                        if poster {
                            self.posters.insert(clip_id, thumb);
                        } else {
                            self.sheets.insert(clip_id, thumb);
                            self.evict_stale_sheets();
                        }
                    }
                    Err(e) => log::warn!("tiles for {clip_id}: {e:#}"),
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
                JobResult::Rotated { clip_id, result } => match result {
                    Ok(info) => self.on_rotated(clip_id, info),
                    Err(e) => {
                        self.rotating.remove(&clip_id);
                        self.set_status(format!("{e:#}"), StatusKind::Error);
                    }
                },
                JobResult::Waved { clip_id, result } => match result {
                    Ok(peaks) => {
                        self.waveforms.insert(clip_id, peaks);
                    }
                    // Silent: a clip whose audio cannot be read still edits
                    // perfectly well, and the bar simply stays empty. Saying
                    // so in the status bar would be noise about something
                    // nobody asked for.
                    Err(e) => log::debug!("waveform for {clip_id}: {e:#}"),
                },
            }
        }
    }

    /// Apply finished probes in the order the files were chosen, stopping at
    /// the first one that has not come back yet.
    fn drain_probes(&mut self) {
        while let Some(next) = self.import_order.first().cloned() {
            let Some(result) = self.probe_results.remove(&next) else {
                return; // still probing; everything after it waits
            };
            self.import_order.remove(0);
            match result {
                Ok(info) => self.on_probed(next, info),
                Err(e) => self.set_status(
                    format!(
                        "{}: {e}",
                        next.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    StatusKind::Error,
                ),
            }
        }
    }

    fn on_probed(&mut self, path: PathBuf, info: MediaInfo) {
        let was_empty = self.project.clips.is_empty();
        let before = self.project.profile.clone();
        let mut new_id = None;
        let mut duplicate = None;
        self.edit(|p| match add_clip(p, &path, &info) {
            ImportOutcome::Added(id) => {
                new_id = Some(id);
                true
            }
            ImportOutcome::Duplicate(id) => {
                duplicate = Some(id);
                false
            }
        });
        if let Some(id) = duplicate {
            self.apply_remeasure(id, &path, &info);
            // A clip already in the bin still needs a proxy if it has not got
            // one — this is the path every clip takes when a project is opened
            // and when proxies are switched on.
            self.request_proxy(id, info);
            return;
        }
        let Some(id) = new_id else {
            return;
        };

        if was_empty {
            self.selected_clip = Some(id);
        }
        // Importing can still move the project's format — it tracks the whole
        // bin until the first mark or cut pins it — so say so whenever it does,
        // not only on the first file.
        if self.project.profile != before {
            self.monitor.set_fps(self.project.fps());
            self.set_status(
                format!("project is {}", self.project.profile.description()),
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
        self.request_proxy(id, info);
    }

    /// Turn 540p proxies on or off for the whole project.
    ///
    /// Turning them on re-probes the bin, which is what feeds proxy requests;
    /// clips that already have a good proxy on disk cost one ffprobe and are
    /// adopted rather than rebuilt. Turning them off leaves the files alone —
    /// they are a cache, and the next time this is switched on they are still
    /// there.
    pub fn set_proxies_enabled(&mut self, on: bool) {
        self.settings.proxies_enabled = on;
        self.settings.save();
        if !on {
            self.workers.clear_queue();
            self.proxy_state.clear();
            self.set_status("playing the originals", StatusKind::Info);
            return;
        }
        if self
            .settings
            .resolve_proxy_dir(self.project_path.as_deref())
            .is_none()
        {
            // Proxies live beside the project, so there has to be a project.
            self.settings.proxies_enabled = false;
            self.settings.save();
            self.set_status(
                "save the project first, so the proxies have somewhere to live",
                StatusKind::Warn,
            );
            return;
        }
        let n = self.project.clips.len();
        self.revalidate_clips();
        self.set_status(
            format!("building proxies for {n} clip(s) — each one speeds up as it lands"),
            StatusKind::Info,
        );
    }

    fn request_proxy(&mut self, id: ClipId, info: MediaInfo) {
        if !self.settings.proxies_enabled {
            return;
        }
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        // Already has one, or is already having one made.
        if clip.proxy_path.as_ref().is_some_and(|p| p.exists())
            || matches!(
                self.proxy_state.get(&id),
                Some(ProxyState::Queued | ProxyState::Running)
            )
        {
            return;
        }
        let source = clip.path.clone();
        let Some(dir) = self.settings.resolve_proxy_dir(self.project_path.as_deref()) else {
            self.set_status(
                "save the project before generating proxies, so they have somewhere to live",
                StatusKind::Warn,
            );
            return;
        };
        self.proxy_state.insert(id, ProxyState::Queued);
        self.workers.submit(Job::Proxy {
            clip_id: id,
            source,
            info: Box::new(info),
            proxy_dir: dir,
        });
    }

    /// Correct a clip in the bin against a fresh probe of its file.
    ///
    /// Silent when nothing moved, which is the ordinary case. When something
    /// does move it is worth saying so plainly: the project on disk recorded
    /// something about this file that is not true, and cuts may have been
    /// pulled back to fit.
    fn apply_remeasure(&mut self, id: ClipId, path: &Path, info: &MediaInfo) {
        let before = self.project.clip(id).map(|c| c.duration_frames).unwrap_or(0);
        let mut outcome = roughcut_core::import::Remeasured::default();
        self.edit(|p| {
            outcome = remeasure(p, id, info);
            outcome.any()
        });
        if !outcome.any() {
            return;
        }
        let after = self.project.clip(id).map(|c| c.duration_frames).unwrap_or(0);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let fps = self.project.fps();
        self.set_status(
            format!(
                "{name} is {} long, not {}{}",
                format_timecode(after, fps),
                format_timecode(before, fps),
                if outcome.trimmed {
                    " — cuts using it were pulled back to fit"
                } else {
                    ""
                }
            ),
            StatusKind::Warn,
        );
        // Anything derived from the old length is now wrong.
        self.posters.remove(&id);
        self.sheets.remove(&id);
        self.thumb_requested.remove(&id);
        self.sheet_requested.remove(&id);
        self.monitor.reload();
    }

    /// Re-probe everything in the bin, cheaply and in the background.
    ///
    /// Run when a project is opened. A project outlives the code that wrote
    /// it, and a clip measured wrongly by an older Roughcut stays wrong on
    /// disk forever otherwise — silently, until playback runs out of picture
    /// early or MLT is handed an out point past the end of the file. ffprobe
    /// costs a few milliseconds per file and these run ahead of thumbnails,
    /// so the cost is invisible.
    fn revalidate_clips(&mut self) {
        if self.tools.ffprobe.is_none() {
            return;
        }
        let paths: Vec<PathBuf> = self
            .project
            .clips
            .iter()
            .filter(|c| c.path.exists())
            .map(|c| c.path.clone())
            .collect();
        for path in paths {
            self.workers.submit(Job::Probe { path });
        }
    }

    fn request_thumbnail(&mut self, id: ClipId) {
        self.request_tiles(id, 1);
    }

    /// Ask for the dense sheet that makes a clip scrubbable.
    ///
    /// Called when a tile comes into view rather than at import, so a bin of
    /// two hundred clips only ever builds sheets for the ones looked at.
    pub fn request_scrub_sheet(&mut self, id: ClipId) {
        if self.sheets.contains_key(&id) {
            return;
        }
        self.request_tiles(id, crate::workers::SCRUB_TILES);
    }

    fn request_tiles(&mut self, id: ClipId, tiles: usize) {
        let poster = tiles <= 1;
        let already = if poster {
            self.posters.contains_key(&id) || !self.thumb_requested.insert(id)
        } else {
            self.sheets.contains_key(&id) || !self.sheet_requested.insert(id)
        };
        if already {
            return;
        }
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        if !self.tools.has_ffmpeg() {
            return;
        }
        self.workers.submit(Job::Thumbs {
            clip_id: id,
            // The proxy when there is one: it is 540p, so every seek and
            // decode is a fraction of the cost of the same work on the
            // original, and it holds frame-for-frame the same picture.
            path: clip.playback_path().to_path_buf(),
            duration_frames: clip.duration_frames,
            fps: self.project.fps(),
            tiles,
            cache_dir: crate::settings::thumb_cache_dir(),
        });
    }

    /// Ask for the clip's loudness envelope, if it has audio and has not been
    /// asked for already.
    ///
    /// Called from the scrub bar as it draws, so only clips actually opened in
    /// the monitor ever cost an audio decode.
    pub fn request_waveform(&mut self, id: ClipId) {
        if self.waveforms.contains_key(&id) || !self.tools.has_ffmpeg() {
            return;
        }
        let Some(clip) = self.project.clip(id) else {
            return;
        };
        if !clip.has_audio {
            return;
        }
        // The proxy carries the same audio re-encoded, and reading it means
        // decoding 540p instead of 4K to throw the picture away.
        let path = clip.playback_path().to_path_buf();
        if !self.waveform_requested.insert(id) {
            return;
        }
        self.workers.submit(Job::Waveform {
            clip_id: id,
            path,
            cache_dir: crate::settings::waveform_cache_dir(),
        });
    }

    fn request_missing_thumbnails(&mut self) {
        let ids: Vec<ClipId> = self
            .project
            .clips
            .iter()
            .filter(|c| !self.posters.contains_key(&c.id))
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
        self.sheet_clock = self.sheet_clock.wrapping_add(1);
        self.remember_window_geometry(ctx);

        // Background work stops while the window is not focused (§3).
        let focused = ctx.input(|i| i.focused);
        self.workers.set_suspended(!focused);

        self.handle_dropped_files(ctx);
        self.drain_workers(ctx);
        self.drain_render();
        self.sync_edl();
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

/// Show a file in the system file manager, selected rather than opened.
fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // `explorer /select,<path>` exits non-zero even when it works, so the
        // status is deliberately not checked — only the spawn is.
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
            .map(|_| ())
    }
}
