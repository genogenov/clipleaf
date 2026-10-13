//! Dev-only test infrastructure for clipleaf (never shipped).
//!
//! - [`compositor`]: the ONE bootstrap for user-scenario tests (binaries, core session flows):
//!   a wayland-server relay on its own thread, reporting [`compositor::Note`]s.
//! - [`raw`]: a scripted byte-level peer for fault injection, independent of `clipleaf_core::ffi`.
//! - [`bins`]: spawning the real binaries against a compositor, with guaranteed cleanup.
//! - [`fds`]: pipes and EOF checks.
//! - [`procs`]: finding processes by exact name and runtime dir.

pub mod bins;
pub mod compositor;
pub mod fds;
pub mod procs;
pub mod raw;

pub use compositor::{Config, Note, TestCompositor, compositor, compositor_with};
pub use rustix;
