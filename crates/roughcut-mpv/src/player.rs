//! A safe wrapper around one `mpv_handle`.

use crate::sys::*;
use anyhow::{bail, Result};
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::Arc;

/// Userdata tags for observed properties, so a property change can be matched
/// without string comparison in the hot path.
pub mod observe {
    pub const TIME_POS: u64 = 1;
    pub const PAUSE: u64 = 2;
    pub const EOF_REACHED: u64 = 3;
    pub const HWDEC_CURRENT: u64 = 4;
    pub const CORE_IDLE: u64 = 5;
}

/// The subset of mpv events Roughcut acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    FileLoaded,
    /// A seek or a resume finished and the new frame is on screen.
    PlaybackRestart,
    EndFile,
    Seek,
    VideoReconfig,
    Shutdown,
    TimePos(Option<f64>),
    Pause(bool),
    EofReached(bool),
    CoreIdle(bool),
    HwdecCurrent(Option<String>),
    LogMessage { level: String, text: String },
    /// Anything else, kept so the caller can still trigger a repaint.
    Other,
}

pub struct Player {
    lib: Arc<MpvLib>,
    handle: *mut MpvHandle,
    /// Boxed so the pointer handed to mpv stays valid for the player's life.
    wakeup: Option<Box<Arc<dyn Fn() + Send + Sync>>>,
}

// The mpv client API is explicitly documented as safe to call from any thread.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

impl Player {
    /// Create and initialise an mpv instance configured for frame-accurate
    /// scrubbing: hardware decode, exact seeking, no OSD, no config files.
    pub fn new(lib: Arc<MpvLib>) -> Result<Self> {
        // SAFETY: mpv_create takes no arguments and returns NULL on failure.
        let handle = unsafe { (lib.create)() };
        if handle.is_null() {
            bail!("mpv_create failed");
        }
        let player = Self {
            lib,
            handle,
            wakeup: None,
        };

        // Options that must be set before mpv_initialize.
        let pre_init: &[(&str, &str)] = &[
            // Never read the user's mpv.conf: Roughcut's timing must not
            // depend on whatever they configured for watching films.
            ("config", "no"),
            ("load-scripts", "no"),
            ("terminal", "no"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("idle", "yes"),
            // §3: hardware decode is mandatory, not opportunistic.
            ("hwdec", "auto-safe"),
            ("vo", "libmpv"),
            // Frame-exact seeking everywhere, including while paused.
            ("hr-seek", "yes"),
            ("hr-seek-framedrop", "no"),
            // Hold the last frame instead of unloading at EOF, so the monitor
            // never goes black at the end of a clip.
            ("keep-open", "always"),
            ("keep-open-pause", "yes"),
            // Start paused: the editor opens a clip to look at it, not to play.
            ("pause", "yes"),
            ("audio-display", "no"),
            ("sub-auto", "no"),
            ("ytdl", "no"),
            // Everything is a local file being scrubbed, so mpv's streaming
            // cache is pure overhead. Bounding the demuxer queues keeps RSS
            // in check on 4K material, where the defaults are generous.
            ("cache", "no"),
            ("demuxer-max-bytes", "32MiB"),
            ("demuxer-max-back-bytes", "16MiB"),
            // Precise timestamps for the frame-number conversion.
            ("correct-pts", "yes"),
        ];
        for (k, v) in pre_init {
            player.set_option(k, v)?;
        }

        // SAFETY: `handle` is a live mpv handle that has not been initialised.
        let rc = unsafe { (player.lib.initialize)(player.handle) };
        player.check(rc, "mpv_initialize")?;

        player.request_log_messages("warn")?;
        player.observe_f64("time-pos", observe::TIME_POS)?;
        player.observe_flag("pause", observe::PAUSE)?;
        player.observe_flag("eof-reached", observe::EOF_REACHED)?;
        player.observe_flag("core-idle", observe::CORE_IDLE)?;
        player.observe_string("hwdec-current", observe::HWDEC_CURRENT)?;

        Ok(player)
    }

    pub fn lib(&self) -> &Arc<MpvLib> {
        &self.lib
    }

    pub(crate) fn raw(&self) -> *mut MpvHandle {
        self.handle
    }

    /// Called from mpv's thread whenever an event is queued. Use it to wake
    /// the UI event loop — never to do work.
    pub fn set_wakeup_callback(&mut self, f: Arc<dyn Fn() + Send + Sync>) {
        let boxed = Box::new(f);
        let ptr = (&*boxed) as *const Arc<dyn Fn() + Send + Sync> as *mut c_void;
        // SAFETY: `boxed` is stored on self and outlives the callback
        // registration, which is cleared in Drop before the box is released.
        unsafe {
            (self.lib.set_wakeup_callback)(self.handle, Some(wakeup_trampoline), ptr);
        }
        self.wakeup = Some(boxed);
    }

    fn check(&self, rc: c_int, what: &str) -> Result<()> {
        if rc < 0 {
            bail!("{what}: {}", self.lib.error_text(rc));
        }
        Ok(())
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<()> {
        let n = CString::new(name)?;
        let v = CString::new(value)?;
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let rc = unsafe { (self.lib.set_option_string)(self.handle, n.as_ptr(), v.as_ptr()) };
        self.check(rc, &format!("set option {name}"))
    }

    pub fn set_property_string(&self, name: &str, value: &str) -> Result<()> {
        let n = CString::new(name)?;
        let v = CString::new(value)?;
        // SAFETY: as above.
        let rc = unsafe { (self.lib.set_property_string)(self.handle, n.as_ptr(), v.as_ptr()) };
        self.check(rc, &format!("set {name}"))
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<()> {
        let n = CString::new(name)?;
        let mut v: c_int = if value { 1 } else { 0 };
        // SAFETY: FLAG expects a pointer to an int, which is what we pass.
        let rc = unsafe {
            (self.lib.set_property)(
                self.handle,
                n.as_ptr(),
                MpvFormat::FLAG,
                &mut v as *mut c_int as *mut c_void,
            )
        };
        self.check(rc, &format!("set {name}"))
    }

    pub fn set_f64(&self, name: &str, value: f64) -> Result<()> {
        let n = CString::new(name)?;
        let mut v: c_double = value;
        // SAFETY: DOUBLE expects a pointer to a double.
        let rc = unsafe {
            (self.lib.set_property)(
                self.handle,
                n.as_ptr(),
                MpvFormat::DOUBLE,
                &mut v as *mut c_double as *mut c_void,
            )
        };
        self.check(rc, &format!("set {name}"))
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let n = CString::new(name).ok()?;
        let mut v: c_double = 0.0;
        // SAFETY: DOUBLE writes into the double we point at, and only on success.
        let rc = unsafe {
            (self.lib.get_property)(
                self.handle,
                n.as_ptr(),
                MpvFormat::DOUBLE,
                &mut v as *mut c_double as *mut c_void,
            )
        };
        (rc >= 0).then_some(v)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let n = CString::new(name).ok()?;
        let mut v: i64 = 0;
        // SAFETY: INT64 writes into the i64 we point at, and only on success.
        let rc = unsafe {
            (self.lib.get_property)(
                self.handle,
                n.as_ptr(),
                MpvFormat::INT64,
                &mut v as *mut i64 as *mut c_void,
            )
        };
        (rc >= 0).then_some(v)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let n = CString::new(name).ok()?;
        let mut v: c_int = 0;
        // SAFETY: FLAG writes into the int we point at, and only on success.
        let rc = unsafe {
            (self.lib.get_property)(
                self.handle,
                n.as_ptr(),
                MpvFormat::FLAG,
                &mut v as *mut c_int as *mut c_void,
            )
        };
        (rc >= 0).then_some(v != 0)
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        let n = CString::new(name).ok()?;
        // SAFETY: mpv allocates the returned string; we copy it and hand the
        // original back to mpv_free, as the API requires.
        unsafe {
            let p = (self.lib.get_property_string)(self.handle, n.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.lib.free)(p as *mut c_void);
            Some(s)
        }
    }

    fn observe(&self, name: &str, id: u64, format: MpvFormat) -> Result<()> {
        let n = CString::new(name)?;
        // SAFETY: the name is copied by mpv; `id` is opaque userdata.
        let rc = unsafe { (self.lib.observe_property)(self.handle, id, n.as_ptr(), format) };
        self.check(rc, &format!("observe {name}"))
    }

    fn observe_f64(&self, name: &str, id: u64) -> Result<()> {
        self.observe(name, id, MpvFormat::DOUBLE)
    }
    fn observe_flag(&self, name: &str, id: u64) -> Result<()> {
        self.observe(name, id, MpvFormat::FLAG)
    }
    fn observe_string(&self, name: &str, id: u64) -> Result<()> {
        self.observe(name, id, MpvFormat::STRING)
    }

    pub fn request_log_messages(&self, level: &str) -> Result<()> {
        let l = CString::new(level)?;
        // SAFETY: NUL-terminated level name.
        let rc = unsafe { (self.lib.request_log_messages)(self.handle, l.as_ptr()) };
        self.check(rc, "request log messages")
    }

    /// Run an mpv command as an argument vector (never a joined string, so
    /// paths containing spaces or quotes cannot be misparsed).
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a))
            .collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: `ptrs` is a NULL-terminated array of live NUL-terminated
        // strings, exactly what mpv_command expects. mpv copies what it needs.
        let rc = unsafe { (self.lib.command)(self.handle, ptrs.as_mut_ptr()) };
        self.check(rc, &format!("command {:?}", args.first().unwrap_or(&"")))
    }

    /// Load a file, replacing whatever is playing.
    pub fn load_file(&self, path: &Path) -> Result<()> {
        let p = path.to_string_lossy();
        self.command(&["loadfile", &p, "replace"])
    }

    pub fn stop(&self) -> Result<()> {
        self.command(&["stop"])
    }

    /// Seek to an absolute time with frame-exact demuxer accuracy.
    /// `seconds` should come from `time::frame_to_seek_seconds`.
    pub fn seek_exact(&self, seconds: f64) -> Result<()> {
        let t = format!("{seconds:.9}");
        self.command(&["seek", &t, "absolute+exact"])
    }

    /// Step exactly one frame forward. mpv decodes precisely one more frame,
    /// which is O(1); reaching the same frame with `seek absolute+exact` costs
    /// a re-decode from the start of the GOP and gets steadily slower the
    /// further into the GOP you are.
    pub fn frame_step(&self) -> Result<()> {
        self.command(&["frame-step"])
    }

    /// Step exactly one frame backward.
    ///
    /// Only works when mpv has a backward decode cache to draw on. Roughcut
    /// runs with `cache=no`, where this was measured to be a silent no-op, so
    /// it deliberately seeks for backward steps instead. Kept for callers that
    /// configure a cache.
    pub fn frame_back_step(&self) -> Result<()> {
        self.command(&["frame-back-step"])
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.set_flag("pause", paused)
    }

    pub fn set_speed(&self, speed: f64) -> Result<()> {
        self.set_f64("speed", speed)
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        self.set_f64("volume", volume.clamp(0.0, 100.0))
    }

    /// Which hardware decoder is actually in use, if any. `None` or `"no"`
    /// means software decoding, which §3 says to warn about.
    pub fn hwdec_active(&self) -> Option<String> {
        match self.get_string("hwdec-current") {
            Some(s) if s != "no" && !s.is_empty() => Some(s),
            _ => None,
        }
    }

    /// Drain one queued event without blocking. Returns `None` when the queue
    /// is empty — this never sleeps, so it cannot burn CPU.
    pub fn poll_event(&self) -> Option<Event> {
        // SAFETY: timeout 0 makes this non-blocking. The returned pointer is
        // owned by mpv and stays valid until the next mpv_wait_event on this
        // handle, which is strictly after we finish reading it here.
        unsafe {
            let ev = (self.lib.wait_event)(self.handle, 0.0);
            if ev.is_null() {
                return None;
            }
            let ev = &*ev;
            Some(match ev.event_id {
                MpvEventId::NONE => return None,
                MpvEventId::FILE_LOADED => Event::FileLoaded,
                MpvEventId::PLAYBACK_RESTART => Event::PlaybackRestart,
                MpvEventId::END_FILE => Event::EndFile,
                MpvEventId::SEEK => Event::Seek,
                MpvEventId::VIDEO_RECONFIG => Event::VideoReconfig,
                MpvEventId::SHUTDOWN => Event::Shutdown,
                MpvEventId::LOG_MESSAGE => {
                    let m = &*(ev.data as *const MpvEventLogMessage);
                    Event::LogMessage {
                        level: cstr_opt(m.level).unwrap_or_default(),
                        text: cstr_opt(m.text).unwrap_or_default().trim_end().to_string(),
                    }
                }
                MpvEventId::PROPERTY_CHANGE => {
                    let p = &*(ev.data as *const MpvEventProperty);
                    match ev.reply_userdata {
                        observe::TIME_POS => Event::TimePos(read_f64(p)),
                        observe::PAUSE => Event::Pause(read_flag(p).unwrap_or(false)),
                        observe::EOF_REACHED => Event::EofReached(read_flag(p).unwrap_or(false)),
                        observe::CORE_IDLE => Event::CoreIdle(read_flag(p).unwrap_or(false)),
                        observe::HWDEC_CURRENT => Event::HwdecCurrent(read_string(p)),
                        _ => Event::Other,
                    }
                }
                _ => Event::Other,
            })
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: clearing the callback before the boxed closure is dropped
        // guarantees mpv cannot call into freed memory. terminate_destroy
        // blocks until the core has shut down.
        unsafe {
            (self.lib.set_wakeup_callback)(self.handle, None, std::ptr::null_mut());
            (self.lib.terminate_destroy)(self.handle);
        }
        self.wakeup = None;
    }
}

/// # Safety
/// `d` must be the pointer passed to `mpv_set_wakeup_callback`, which is
/// always a live `&Arc<dyn Fn()>` owned by the `Player`.
unsafe extern "C" fn wakeup_trampoline(d: *mut c_void) {
    if d.is_null() {
        return;
    }
    let f = &*(d as *const Arc<dyn Fn() + Send + Sync>);
    f();
}

/// # Safety
/// `p` must be a valid, NUL-terminated C string or null.
unsafe fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// # Safety
/// `p.data` must match `p.format`, which mpv guarantees.
unsafe fn read_f64(p: &MpvEventProperty) -> Option<f64> {
    if p.format == MpvFormat::DOUBLE && !p.data.is_null() {
        Some(*(p.data as *const c_double))
    } else {
        None
    }
}

/// # Safety
/// As `read_f64`.
unsafe fn read_flag(p: &MpvEventProperty) -> Option<bool> {
    if p.format == MpvFormat::FLAG && !p.data.is_null() {
        Some(*(p.data as *const c_int) != 0)
    } else {
        None
    }
}

/// # Safety
/// As `read_f64`. The string is owned by mpv and only copied here.
unsafe fn read_string(p: &MpvEventProperty) -> Option<String> {
    if p.format == MpvFormat::STRING && !p.data.is_null() {
        cstr_opt(*(p.data as *const *const c_char))
    } else {
        None
    }
}
