// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Roughcut's model layer: everything that is not pixels on screen.
//!
//! Nothing in this crate knows about egui, mpv or threads. That keeps the
//! correctness-critical parts — the timing model, timeline arithmetic and the
//! MLT writer — testable without a window or a GPU.

#![forbid(unsafe_code)]

pub mod import;
pub mod mlt;
pub mod model;
pub mod edl;
pub mod probe;
pub mod profile;
pub mod project_io;
pub mod proxy;
pub mod render;
pub mod rotate;
pub mod time;
pub mod timeline;
pub mod tools;
pub mod waveform;

pub use model::{ClipId, Profile, Project, SourceClip, TimelineItem, SCHEMA_VERSION};
pub use time::Rational;
pub use undo::History;

pub mod undo;
