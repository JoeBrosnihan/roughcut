//! libmpv bindings for Roughcut: the client API for control and events, and
//! the OpenGL render API for getting frames onto the screen.
//!
//! libmpv is opened at runtime rather than linked at build time — see
//! `sys::MpvLib::load` for why.

pub mod gl_loader;
pub mod player;
pub mod render;
pub mod sys;

pub use player::{Event, Player};
pub use render::RenderContext;
pub use sys::MpvLib;
