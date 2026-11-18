//! Backend real no Windows. Nos outros sistemas o binário de demonstração
//! usa [`crate::fake::FakeBackend`].

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use windows::{hardware_summary, mark_ui_thread, WindowsBackend};

#[cfg(not(windows))]
pub fn mark_ui_thread() {}
