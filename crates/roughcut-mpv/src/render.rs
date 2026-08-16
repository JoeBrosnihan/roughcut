//! mpv's OpenGL render API.
//!
//! The context renders the current video frame into an FBO we own; the app
//! then draws that FBO's texture as an ordinary egui image. Everything here
//! must run on the thread that owns the GL context.

use crate::gl_loader::{get_proc_address, GlProcLoader};
use crate::player::Player;
use crate::sys::*;
use anyhow::{bail, Result};
use std::ffi::{c_int, c_void};
use std::sync::Arc;

pub struct RenderContext {
    lib: Arc<MpvLib>,
    ctx: *mut MpvRenderContext,
    /// Held for the context's lifetime: mpv keeps the pointer and may call
    /// `get_proc_address` again after creation.
    _loader: Box<GlProcLoader>,
    update_cb: Option<Box<Arc<dyn Fn() + Send + Sync>>>,
}

// Deliberately not Send: `*mut MpvRenderContext` is bound to the GL thread.

impl RenderContext {
    /// Create the render context against the *current* GL context. Must be
    /// called on the thread that owns it.
    pub fn new(player: &Player) -> Result<Self> {
        let lib = player.lib().clone();
        let loader = Box::new(GlProcLoader::new()?);

        let mut init = MpvOpenglInitParams {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: (&*loader) as *const GlProcLoader as *mut c_void,
        };
        // `advanced_control` stays off: it moves decoding onto mpv's own
        // threads and requires us to service render updates promptly even when
        // idle, which conflicts with the zero-CPU-when-idle requirement.
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut MpvOpenglInitParams as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];

        let mut ctx: *mut MpvRenderContext = std::ptr::null_mut();
        // SAFETY: `params` is terminated by an INVALID entry as the API
        // requires, and every pointer inside it outlives the call. `loader`
        // outlives the context because it is moved into the returned struct.
        let rc = unsafe { (lib.render_create)(&mut ctx, player.raw(), params.as_mut_ptr()) };
        if rc < 0 {
            bail!("mpv_render_context_create: {}", lib.error_text(rc));
        }
        if ctx.is_null() {
            bail!("mpv_render_context_create returned no context");
        }

        Ok(Self {
            lib,
            ctx,
            _loader: loader,
            update_cb: None,
        })
    }

    /// Called from an arbitrary mpv thread when a new frame is ready. Use it
    /// only to request a repaint — the actual render must happen on the GL
    /// thread inside `render`.
    pub fn set_update_callback(&mut self, f: Arc<dyn Fn() + Send + Sync>) {
        let boxed = Box::new(f);
        let ptr = (&*boxed) as *const Arc<dyn Fn() + Send + Sync> as *mut c_void;
        // SAFETY: the box is stored on self and the callback is cleared in
        // Drop before it is released.
        unsafe {
            (self.lib.render_set_update_callback)(self.ctx, Some(update_trampoline), ptr);
        }
        self.update_cb = Some(boxed);
    }

    /// True when mpv has a new frame that needs rendering.
    pub fn needs_frame(&self) -> bool {
        // SAFETY: `ctx` is live for the struct's lifetime.
        let flags = unsafe { (self.lib.render_update)(self.ctx) };
        flags & MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Draw the current frame into `fbo` at `width` x `height` pixels.
    ///
    /// `fbo` is a GL framebuffer name, or 0 for the default framebuffer.
    /// `flip_y` compensates for OpenGL's bottom-left origin when the target
    /// will be sampled as a normal top-left-origin texture.
    pub fn render(&self, fbo: u32, width: i32, height: i32, flip_y: bool) -> Result<()> {
        let mut target = MpvOpenglFbo {
            fbo: fbo as c_int,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = if flip_y { 1 } else { 0 };
        // Never block waiting for a frame's presentation time: the UI thread
        // is drawing and must not stall.
        let mut block: c_int = 0;
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut target as *mut MpvOpenglFbo as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut c_int as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: &mut block as *mut c_int as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: called on the GL thread with a current context; the target
        // FBO is complete and colour-renderable at the given size.
        let rc = unsafe { (self.lib.render_render)(self.ctx, params.as_mut_ptr()) };
        if rc < 0 {
            bail!("mpv_render_context_render: {}", self.lib.error_text(rc));
        }
        Ok(())
    }

    /// Tell mpv the frame reached the screen, so its timing stays honest.
    pub fn report_swap(&self) {
        // SAFETY: `ctx` is live for the struct's lifetime.
        unsafe { (self.lib.render_report_swap)(self.ctx) }
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        // SAFETY: the callback is cleared first so mpv cannot call into the
        // box as it is freed. Freeing must happen on the GL thread, which is
        // guaranteed because RenderContext is not Send.
        unsafe {
            (self.lib.render_set_update_callback)(self.ctx, None, std::ptr::null_mut());
            (self.lib.render_free)(self.ctx);
        }
        self.update_cb = None;
    }
}

/// # Safety
/// `d` must be the pointer passed to `mpv_render_context_set_update_callback`.
unsafe extern "C" fn update_trampoline(d: *mut c_void) {
    if d.is_null() {
        return;
    }
    let f = &*(d as *const Arc<dyn Fn() + Send + Sync>);
    f();
}
