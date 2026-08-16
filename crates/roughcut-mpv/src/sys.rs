//! Raw libmpv C ABI declarations and runtime symbol loading.
//!
//! libmpv is loaded with `dlopen`/`LoadLibrary` at startup rather than linked
//! at build time. That keeps `cargo build` working on a machine without the
//! mpv development package, and lets the app degrade to "no monitor, but every
//! other feature works" with a status-bar warning instead of failing to start.

use anyhow::{anyhow, Context, Result};
use libloading::{Library, Symbol};
use std::ffi::{c_char, c_double, c_int, c_void, CStr};
use std::path::{Path, PathBuf};

pub type MpvHandle = c_void;
pub type MpvRenderContext = c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpvFormat(pub c_int);

impl MpvFormat {
    pub const NONE: Self = Self(0);
    pub const STRING: Self = Self(1);
    pub const FLAG: Self = Self(3);
    pub const INT64: Self = Self(4);
    pub const DOUBLE: Self = Self(5);
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpvEventId(pub c_int);

impl MpvEventId {
    pub const NONE: Self = Self(0);
    pub const SHUTDOWN: Self = Self(1);
    pub const LOG_MESSAGE: Self = Self(2);
    pub const START_FILE: Self = Self(6);
    pub const END_FILE: Self = Self(7);
    pub const FILE_LOADED: Self = Self(8);
    pub const VIDEO_RECONFIG: Self = Self(17);
    pub const SEEK: Self = Self(20);
    pub const PLAYBACK_RESTART: Self = Self(21);
    pub const PROPERTY_CHANGE: Self = Self(22);
}

#[repr(C)]
pub struct MpvEvent {
    pub event_id: MpvEventId,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: MpvFormat,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvEventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

// --- render API -------------------------------------------------------------

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
pub const MPV_RENDER_PARAM_ADVANCED_CONTROL: c_int = 10;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;

pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

/// `MPV_RENDER_API_TYPE_OPENGL`, NUL-terminated for the C side.
pub const RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";

#[repr(C)]
pub struct MpvRenderParam {
    pub type_: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvOpenglInitParams {
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct MpvOpenglFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

// --- symbol table -----------------------------------------------------------

type FnCreate = unsafe extern "C" fn() -> *mut MpvHandle;
type FnInitialize = unsafe extern "C" fn(*mut MpvHandle) -> c_int;
type FnTerminateDestroy = unsafe extern "C" fn(*mut MpvHandle);
type FnSetOptionString = unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnSetPropertyString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type FnSetProperty =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, MpvFormat, *mut c_void) -> c_int;
type FnGetProperty =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, MpvFormat, *mut c_void) -> c_int;
type FnGetPropertyString = unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> *mut c_char;
type FnCommand = unsafe extern "C" fn(*mut MpvHandle, *mut *const c_char) -> c_int;
type FnObserveProperty =
    unsafe extern "C" fn(*mut MpvHandle, u64, *const c_char, MpvFormat) -> c_int;
type FnWaitEvent = unsafe extern "C" fn(*mut MpvHandle, c_double) -> *mut MpvEvent;
type FnSetWakeupCallback =
    unsafe extern "C" fn(*mut MpvHandle, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void);
type FnRequestLogMessages = unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> c_int;
type FnFree = unsafe extern "C" fn(*mut c_void);
type FnErrorString = unsafe extern "C" fn(c_int) -> *const c_char;
type FnClientApiVersion = unsafe extern "C" fn() -> std::ffi::c_ulong;

type FnRenderCreate =
    unsafe extern "C" fn(*mut *mut MpvRenderContext, *mut MpvHandle, *mut MpvRenderParam) -> c_int;
type FnRenderFree = unsafe extern "C" fn(*mut MpvRenderContext);
type FnRenderRender = unsafe extern "C" fn(*mut MpvRenderContext, *mut MpvRenderParam) -> c_int;
type FnRenderUpdate = unsafe extern "C" fn(*mut MpvRenderContext) -> u64;
type FnRenderSetUpdateCallback = unsafe extern "C" fn(
    *mut MpvRenderContext,
    Option<unsafe extern "C" fn(*mut c_void)>,
    *mut c_void,
);
type FnRenderReportSwap = unsafe extern "C" fn(*mut MpvRenderContext);

/// Every libmpv entry point Roughcut uses, resolved once at load.
pub struct MpvLib {
    // Keeps the library mapped; every function pointer below borrows from it.
    _lib: Library,
    pub path: PathBuf,
    pub create: FnCreate,
    pub initialize: FnInitialize,
    pub terminate_destroy: FnTerminateDestroy,
    pub set_option_string: FnSetOptionString,
    pub set_property_string: FnSetPropertyString,
    pub set_property: FnSetProperty,
    pub get_property: FnGetProperty,
    pub get_property_string: FnGetPropertyString,
    pub command: FnCommand,
    pub observe_property: FnObserveProperty,
    pub wait_event: FnWaitEvent,
    pub set_wakeup_callback: FnSetWakeupCallback,
    pub request_log_messages: FnRequestLogMessages,
    pub free: FnFree,
    pub error_string: FnErrorString,
    pub client_api_version: FnClientApiVersion,
    pub render_create: FnRenderCreate,
    pub render_free: FnRenderFree,
    pub render_render: FnRenderRender,
    pub render_update: FnRenderUpdate,
    pub render_set_update_callback: FnRenderSetUpdateCallback,
    pub render_report_swap: FnRenderReportSwap,
}

// The mpv client API is documented as thread-safe; the render context is not,
// and is kept on the render thread by holding a raw pointer (see render.rs).
unsafe impl Send for MpvLib {}
unsafe impl Sync for MpvLib {}

/// Shared-library file names to try, in order, for the running platform.
pub fn candidate_names() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        vec!["libmpv-2.dll", "mpv-2.dll", "libmpv.dll", "mpv-1.dll"]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            "libmpv.2.dylib",
            "libmpv.dylib",
            // Homebrew: /opt/homebrew on Apple Silicon, /usr/local on Intel.
            "/opt/homebrew/lib/libmpv.2.dylib",
            "/opt/homebrew/lib/libmpv.dylib",
            "/usr/local/lib/libmpv.2.dylib",
            "/usr/local/lib/libmpv.dylib",
            // MacPorts.
            "/opt/local/lib/libmpv.2.dylib",
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec!["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
    }
}

/// Places to look before falling back to the platform loader's own search.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            // Inside a macOS app bundle the binary sits in Contents/MacOS and
            // shared libraries belong in Contents/Frameworks.
            #[cfg(target_os = "macos")]
            if dir.ends_with("Contents/MacOS") {
                if let Some(contents) = dir.parent() {
                    dirs.push(contents.join("Frameworks"));
                }
            }
            // `cargo run` puts the binary in target/<profile>/; the vendored
            // copy sits at the workspace root during development.
            for up in [1usize, 2, 3] {
                if let Some(anc) = dir.ancestors().nth(up) {
                    dirs.push(anc.join("vendor").join("mpv"));
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("vendor").join("mpv"));
    }
    dirs
}

impl MpvLib {
    /// Load libmpv. `ROUGHCUT_MPV` overrides every other location.
    pub fn load() -> Result<Self> {
        let mut attempts: Vec<String> = Vec::new();

        if let Some(explicit) = std::env::var_os("ROUGHCUT_MPV") {
            let p = PathBuf::from(explicit);
            match unsafe { Library::new(&p) } {
                Ok(lib) => return Self::bind(lib, p),
                Err(e) => attempts.push(format!("{} ({e})", p.display())),
            }
        }

        for dir in search_dirs() {
            for name in candidate_names() {
                let p = dir.join(name);
                if !p.is_file() {
                    continue;
                }
                match unsafe { Library::new(&p) } {
                    Ok(lib) => return Self::bind(lib, p),
                    Err(e) => attempts.push(format!("{} ({e})", p.display())),
                }
            }
        }

        // Finally let the OS loader search its own paths.
        for name in candidate_names() {
            match unsafe { Library::new(name) } {
                Ok(lib) => return Self::bind(lib, PathBuf::from(name)),
                Err(e) => attempts.push(format!("{name} ({e})")),
            }
        }

        Err(anyhow!(
            "libmpv could not be loaded. Tried: {}. Set ROUGHCUT_MPV to the \
             full path of the library, or place it next to the executable.",
            attempts.join("; ")
        ))
    }

    /// Load from an explicit path, for the settings override.
    pub fn load_from(path: &Path) -> Result<Self> {
        let lib = unsafe { Library::new(path) }
            .with_context(|| format!("cannot load {}", path.display()))?;
        Self::bind(lib, path.to_path_buf())
    }

    fn bind(lib: Library, path: PathBuf) -> Result<Self> {
        // SAFETY: every name below is a documented libmpv export and the type
        // aliases mirror the signatures in client.h / render.h verbatim.
        unsafe {
            macro_rules! sym {
                ($name:literal) => {{
                    let s: Symbol<_> = lib
                        .get($name)
                        .with_context(|| format!("{} lacks {}", path.display(),
                            String::from_utf8_lossy(&$name[..$name.len() - 1])))?;
                    *s
                }};
            }
            Ok(Self {
                create: sym!(b"mpv_create\0"),
                initialize: sym!(b"mpv_initialize\0"),
                terminate_destroy: sym!(b"mpv_terminate_destroy\0"),
                set_option_string: sym!(b"mpv_set_option_string\0"),
                set_property_string: sym!(b"mpv_set_property_string\0"),
                set_property: sym!(b"mpv_set_property\0"),
                get_property: sym!(b"mpv_get_property\0"),
                get_property_string: sym!(b"mpv_get_property_string\0"),
                command: sym!(b"mpv_command\0"),
                observe_property: sym!(b"mpv_observe_property\0"),
                wait_event: sym!(b"mpv_wait_event\0"),
                set_wakeup_callback: sym!(b"mpv_set_wakeup_callback\0"),
                request_log_messages: sym!(b"mpv_request_log_messages\0"),
                free: sym!(b"mpv_free\0"),
                error_string: sym!(b"mpv_error_string\0"),
                client_api_version: sym!(b"mpv_client_api_version\0"),
                render_create: sym!(b"mpv_render_context_create\0"),
                render_free: sym!(b"mpv_render_context_free\0"),
                render_render: sym!(b"mpv_render_context_render\0"),
                render_update: sym!(b"mpv_render_context_update\0"),
                render_set_update_callback: sym!(b"mpv_render_context_set_update_callback\0"),
                render_report_swap: sym!(b"mpv_render_context_report_swap\0"),
                _lib: lib,
                path,
            })
        }
    }

    /// `(major, minor)` of the loaded client API.
    // `c_ulong` is 32-bit on Windows and 64-bit elsewhere, so the cast is
    // redundant on some targets and load-bearing on others.
    #[allow(clippy::unnecessary_cast)]
    pub fn api_version(&self) -> (u32, u32) {
        // SAFETY: no arguments, no state.
        let v = unsafe { (self.client_api_version)() } as u32;
        (v >> 16, v & 0xffff)
    }

    /// Human-readable text for a libmpv error code.
    pub fn error_text(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static NUL-terminated string for
        // any input, including unknown codes.
        unsafe {
            let p = (self.error_string)(code);
            if p.is_null() {
                format!("mpv error {code}")
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }
}
