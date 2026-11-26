#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

pub mod audio;
pub mod config;
pub mod pipeline;

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub mod capture;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub mod desktop;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub mod dictation;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
mod sound;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows as platform;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod unix;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use unix as platform;
