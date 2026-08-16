//! Roughcut's model layer: everything that is not pixels on screen.
//!
//! Nothing in this crate knows about egui, mpv or threads. That keeps the
//! correctness-critical parts — the timing model, timeline arithmetic and the
//! MLT writer — testable without a window or a GPU.

#![forbid(unsafe_code)]

pub mod import;
pub mod mlt;
pub mod model;
pub mod probe;
pub mod project_io;
pub mod proxy;
pub mod time;
pub mod timeline;
pub mod tools;

pub use model::{ClipId, Profile, Project, SourceClip, TimelineItem, SCHEMA_VERSION};
pub use time::Rational;
pub use undo::History;

pub mod undo;
