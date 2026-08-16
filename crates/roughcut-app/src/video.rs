//! Getting mpv's output onto the screen as an ordinary egui element.
//!
//! mpv renders the current frame into an FBO we own, and that FBO is blitted
//! into egui's target inside a paint callback. Using `glBlitFramebuffer`
//! instead of a textured quad means no shader, no VAO and no vertex buffer of
//! our own, so there is very little GL state to get wrong.
//!
//! Everything here runs on the thread that owns the GL context.

use eframe::glow::{self, HasContext};
use roughcut_mpv::{Player, RenderContext};
use std::sync::{Arc, Mutex};

/// Owns the FBO mpv draws into and the mpv render context itself.
pub struct VideoRenderer {
    render: Option<RenderContext>,
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    size: (i32, i32),
    /// Set once if the render context could not be created, so the app can
    /// say so in the alert bar instead of retrying every frame.
    pub init_error: Option<String>,
}

/// SAFETY: eframe's glow backend runs `App::update`, paint callbacks and
/// `App::on_exit` on the same thread — the one that owns the GL context. The
/// `Mutex` this is stored behind therefore never actually transfers the value
/// between threads; the bound exists only because egui's paint callbacks are
/// declared `Send + Sync`. `RenderContext` is created, used and dropped on
/// that single thread, which is exactly what libmpv requires.
unsafe impl Send for VideoRenderer {}

impl Default for VideoRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoRenderer {
    pub fn new() -> Self {
        Self {
            render: None,
            fbo: None,
            tex: None,
            size: (0, 0),
            init_error: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.render.is_some()
    }

    /// Create the mpv render context. Must run with the GL context current.
    pub fn ensure_context(
        &mut self,
        player: &Player,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> bool {
        if self.render.is_some() {
            return true;
        }
        if self.init_error.is_some() {
            return false;
        }
        match RenderContext::new(player) {
            Ok(mut ctx) => {
                ctx.set_update_callback(repaint);
                self.render = Some(ctx);
                true
            }
            Err(e) => {
                log::error!("mpv render context: {e:#}");
                self.init_error = Some(format!("{e}"));
                false
            }
        }
    }

    /// Tear everything down while the GL context is still current.
    pub fn destroy(&mut self, gl: &glow::Context) {
        // The render context must go first: it holds GL objects of its own.
        self.render = None;
        // SAFETY: called on the GL thread with a current context; the names
        // were produced by this same context.
        unsafe {
            if let Some(fbo) = self.fbo.take() {
                gl.delete_framebuffer(fbo);
            }
            if let Some(tex) = self.tex.take() {
                gl.delete_texture(tex);
            }
        }
        self.size = (0, 0);
    }

    /// (Re)allocate the render target when the panel size changes.
    ///
    /// # Safety
    /// The GL context must be current on the calling thread.
    unsafe fn ensure_target(&mut self, gl: &glow::Context, w: i32, h: i32) -> bool {
        if self.size == (w, h) && self.fbo.is_some() {
            return true;
        }
        if let Some(fbo) = self.fbo.take() {
            gl.delete_framebuffer(fbo);
        }
        if let Some(tex) = self.tex.take() {
            gl.delete_texture(tex);
        }

        let Ok(tex) = gl.create_texture() else {
            return false;
        };
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );

        let Ok(fbo) = gl.create_framebuffer() else {
            gl.delete_texture(tex);
            return false;
        };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        let complete =
            gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.bind_texture(glow::TEXTURE_2D, None);

        if !complete {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(tex);
            log::error!("video FBO {w}x{h} is not complete");
            return false;
        }

        self.fbo = Some(fbo);
        self.tex = Some(tex);
        self.size = (w, h);
        true
    }

    /// Render the current mpv frame and blit it into the egui target.
    /// `dst` is in physical pixels with a bottom-left origin, matching GL.
    pub fn paint(&mut self, gl: &glow::Context, dst: PixelRect) {
        if self.render.is_none() {
            return;
        }
        let (w, h) = (dst.width.max(1), dst.height.max(1));

        // The target must be sized before the render context is borrowed,
        // because resizing it mutates `self`.
        // SAFETY: this runs inside an egui paint callback, so the GL context
        // is current on this thread.
        if !unsafe { self.ensure_target(gl, w, h) } {
            return;
        }
        let Some(fbo) = self.fbo else { return };
        let Some(render) = self.render.as_ref() else {
            return;
        };

        // SAFETY: as above.
        unsafe {

            // egui is mid-render: remember what it had bound and put it back.
            let prev_draw = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
            let prev_read = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
            let scissor_was_on = gl.is_enabled(glow::SCISSOR_TEST);

            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, w, h);
            // mpv letterboxes into whatever size it is given, so the clip's
            // aspect ratio is preserved without any arithmetic here.
            //
            // `flip_y` is on because mpv writes the top of the picture to row
            // zero, while the blit below treats row zero as the bottom (GL's
            // convention, which is what `from_bottom_px` gives us). Without
            // it the monitor shows the image upside down.
            if let Err(e) = render.render(framebuffer_name(fbo), w, h, true) {
                log::warn!("mpv render: {e}");
            }

            // Blit into egui's target. A scissor rect left over from egui
            // would clip this to the wrong region, so it is turned off for the
            // duration and restored afterwards.
            if scissor_was_on {
                gl.disable(glow::SCISSOR_TEST);
            }
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, framebuffer_from_name(prev_draw));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                dst.left,
                dst.bottom,
                dst.left + w,
                dst.bottom + h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            if scissor_was_on {
                gl.enable(glow::SCISSOR_TEST);
            }

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, framebuffer_from_name(prev_read));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, framebuffer_from_name(prev_draw));
            gl.viewport(dst.left, dst.bottom, w, h);
        }

        render.report_swap();
    }

}

/// A rectangle in physical pixels, bottom-left origin, as OpenGL wants it.
#[derive(Debug, Clone, Copy)]
pub struct PixelRect {
    pub left: i32,
    pub bottom: i32,
    pub width: i32,
    pub height: i32,
}

fn framebuffer_name(fbo: glow::Framebuffer) -> u32 {
    fbo.0.get()
}

fn framebuffer_from_name(name: i32) -> Option<glow::Framebuffer> {
    std::num::NonZeroU32::new(name.max(0) as u32).map(glow::NativeFramebuffer)
}

/// Shared handle so the paint callback, which must be `Send + Sync`, can reach
/// the renderer that lives on the UI thread.
pub type SharedVideo = Arc<Mutex<VideoRenderer>>;

pub fn shared() -> SharedVideo {
    Arc::new(Mutex::new(VideoRenderer::new()))
}
