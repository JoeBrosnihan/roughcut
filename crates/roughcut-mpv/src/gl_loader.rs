//! Resolving OpenGL function pointers for mpv's render API.
//!
//! eframe hands us a `glow::Context` but not a `get_proc_address`, so the
//! address of each GL entry point is looked up directly from the platform's
//! GL library. This is the same thing glutin does internally; doing it here
//! avoids having to thread a display handle through eframe.

use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_void, CStr};

pub struct GlProcLoader {
    gl: Library,
    #[cfg(windows)]
    wgl_get_proc_address: unsafe extern "system" fn(*const c_char) -> *mut c_void,
}

impl GlProcLoader {
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            // opengl32.dll is already mapped — glutin created the context —
            // so this just bumps its refcount.
            let gl = unsafe { Library::new("opengl32.dll") }
                .context("cannot open opengl32.dll")?;
            // SAFETY: wglGetProcAddress has this exact signature in wingdi.h.
            let wgl_get_proc_address = unsafe {
                let s: libloading::Symbol<unsafe extern "system" fn(*const c_char) -> *mut c_void> =
                    gl.get(b"wglGetProcAddress\0")
                        .context("opengl32.dll lacks wglGetProcAddress")?;
                *s
            };
            Ok(Self {
                gl,
                wgl_get_proc_address,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let gl = unsafe {
                Library::new("/System/Library/Frameworks/OpenGL.framework/OpenGL")
            }
            .context("cannot open the OpenGL framework")?;
            Ok(Self { gl })
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let gl = unsafe { Library::new("libGL.so.1") }
                .or_else(|_| unsafe { Library::new("libGL.so") })
                .context("cannot open libGL")?;
            Ok(Self { gl })
        }
    }

    /// Address of the named GL entry point, or null.
    pub fn resolve(&self, name: &CStr) -> *mut c_void {
        #[cfg(windows)]
        {
            // wglGetProcAddress resolves extensions and GL >1.1 but returns
            // one of these sentinels for core 1.1 functions, which have to
            // come from the DLL's export table instead.
            // SAFETY: `name` is NUL-terminated for the duration of the call.
            let p = unsafe { (self.wgl_get_proc_address)(name.as_ptr()) };
            let bogus = matches!(p as isize, -1..=3);
            if !bogus {
                return p;
            }
        }
        // SAFETY: a missing symbol returns Err rather than an invalid pointer.
        // The symbol's real signature is irrelevant here — only its address is
        // wanted, and mpv casts it back to the right type itself.
        unsafe {
            match self
                .gl
                .get::<unsafe extern "C" fn()>(name.to_bytes_with_nul())
            {
                Ok(sym) => *sym as *const () as *mut c_void,
                Err(_) => std::ptr::null_mut(),
            }
        }
    }
}

/// The C callback mpv calls to resolve each GL function.
///
/// # Safety
/// `ctx` must be a live `*const GlProcLoader` and `name` a NUL-terminated
/// string; mpv guarantees both.
pub unsafe extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    let loader = &*(ctx as *const GlProcLoader);
    loader.resolve(CStr::from_ptr(name))
}
