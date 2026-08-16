// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

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
