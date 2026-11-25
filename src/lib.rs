#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

pub mod audio;
pub mod config;
pub mod pipeline;

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub mod desktop;
