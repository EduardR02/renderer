//! OS notifications for changes to the system's default playback output.
//!
//! The callback does no device enumeration or player work: it only wakes the
//! engine loop, which replaces the stream through its existing rebuild path.

#[cfg(target_os = "macos")]
#[path = "output_device/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "output_device/windows.rs"]
mod platform;

pub use platform::watch;
